import { ManagedBrowserVoice } from "../pkg-web/nanocodex.js";
import { initializeBrowserEngine } from "../browser/engine.mjs";
import {
  cancelManagedTurn,
  routeManagedRealtime,
  startManagedRealtime,
  stopManagedRealtime,
} from "./internal.mjs";

/** @internal Adapts the Rust-owned protocol core to one remote managed Agent. */
export async function createManagedBrowserVoice(agent, voice, options = {}) {
  await initializeBrowserEngine(options);
  const raw = new ManagedBrowserVoice(voice);
  const voiceSessionId = uuidV7();
  const startOperationId = crypto.randomUUID();
  const stopOperationId = crypto.randomUUID();
  const tailOperationId = crypto.randomUUID();
  const delegationOperations = new Map();
  let startedTurnId;
  let activeTurnId;
  let routePending = false;
  let pendingEvents = [];

  function observe(value) {
    if (value?.turnId !== activeTurnId) return undefined;
    const effects = raw.agentEvent(JSON.stringify(value.event));
    if (isTerminalAgentEvent(value.event)) {
      if (startedTurnId === activeTurnId) startedTurnId = undefined;
      activeTurnId = undefined;
    }
    return effects;
  }

  function adoptRoute(routed) {
    if (typeof routed.turn_id !== "string") return [];
    activeTurnId = routed.turn_id;
    if (routed.route === "started") startedTurnId = routed.turn_id;
    const buffered = pendingEvents;
    pendingEvents = [];
    return buffered.map(observe).filter((effects) => effects !== undefined);
  }

  return {
    async start() {
      const started = await startManagedRealtime(agent, voiceSessionId, startOperationId);
      raw.start(JSON.stringify(started.context));
    },
    callBody(sdp) {
      const envelope = JSON.parse(raw.callBody(sdp, voiceSessionId));
      envelope.managed_agent_id = agent.id;
      return JSON.stringify(envelope);
    },
    completeCall: (body, location) => raw.completeCall(body, location),
    sidebandUrl(callId) {
      const path = raw.sidebandUrl(callId, voiceSessionId);
      const url = new URL(path, globalThis.location?.href ?? "http://localhost");
      url.searchParams.set("managed_agent_id", agent.id);
      return `${url.pathname}${url.search}`;
    },
    sidebandOpened: () => raw.sidebandOpened(),
    sidebandClosed: (connectedMs) => raw.sidebandClosed(connectedMs),
    framesSent: (count) => raw.framesSent(count),
    requiresAgentAdmission: (payload) => raw.requiresAgentAdmission(payload),
    async realtimeMessage(payload) {
      const update = JSON.parse(raw.realtimeMessage(payload));
      if (typeof update.delegation === "string" && update.delegation.trim()) {
        const delegationId = managedDelegationId(payload);
        let operationId = delegationId && delegationOperations.get(delegationId);
        if (!operationId) {
          operationId = crypto.randomUUID();
          if (delegationId) delegationOperations.set(delegationId, operationId);
        }
        routePending = true;
        try {
          const bufferedEffects = adoptRoute(await routeManagedRealtime(
            agent,
            voiceSessionId,
            operationId,
            update.delegation,
          ));
          update.effects = mergeVoiceEffects(update.effects, bufferedEffects);
        } finally {
          routePending = false;
          if (activeTurnId === undefined) pendingEvents = [];
        }
      }
      return JSON.stringify(update.effects);
    },
    agentEvent(envelope) {
      const value = typeof envelope === "string" ? JSON.parse(envelope) : envelope;
      if (routePending && activeTurnId === undefined) {
        pendingEvents.push(value);
        return undefined;
      }
      return observe(value);
    },
    flush: (finalChunk) => raw.flush(finalChunk),
    async stop() {
      const update = JSON.parse(raw.stop());
      let routeFailure;
      if (typeof update.delegation === "string" && update.delegation.trim()) {
        try {
          await routeManagedRealtime(
            agent,
            voiceSessionId,
            tailOperationId,
            update.delegation,
          );
        } catch (error) {
          routeFailure = error;
        }
      }
      let stopFailure;
      try {
        await stopManagedRealtime(agent, voiceSessionId, stopOperationId);
      } catch (error) {
        stopFailure = error;
      }
      startedTurnId = undefined;
      activeTurnId = undefined;
      pendingEvents = [];
      if (routeFailure !== undefined) throw routeFailure;
      if (stopFailure !== undefined) throw stopFailure;
      return JSON.stringify(update.effects);
    },
    async cancel() {
      if (!startedTurnId) return false;
      const turnId = startedTurnId;
      await cancelManagedTurn(agent, turnId);
      if (startedTurnId === turnId) startedTurnId = undefined;
      return true;
    },
    preferredPhysicalInput: (current, labels) => raw.preferredPhysicalInput(current, labels),
    free: () => raw.free(),
  };
}

function uuidV7() {
  if (typeof globalThis.crypto?.getRandomValues !== "function") {
    throw new Error("managed browser voice requires crypto.getRandomValues()");
  }
  const bytes = globalThis.crypto.getRandomValues(new Uint8Array(16));
  let timestamp = Date.now();
  for (let index = 5; index >= 0; index -= 1) {
    bytes[index] = timestamp % 256;
    timestamp = Math.floor(timestamp / 256);
  }
  bytes[6] = (bytes[6] & 0x0f) | 0x70;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;
  const encoded = [...bytes].map((byte) => byte.toString(16).padStart(2, "0"));
  return `${encoded.slice(0, 4).join("")}-${encoded.slice(4, 6).join("")}-${encoded.slice(6, 8).join("")}-${encoded.slice(8, 10).join("")}-${encoded.slice(10).join("")}`;
}

function managedDelegationId(payload) {
  try {
    const event = typeof payload === "string" ? JSON.parse(payload) : payload;
    return typeof event?.item?.id === "string" ? event.item.id : undefined;
  } catch {
    return undefined;
  }
}

function isTerminalAgentEvent(event) {
  return event?.type === "run.completed" || event?.type === "run.failed"
    || event?.type === "run.cancelled";
}

function mergeVoiceEffects(base, encoded) {
  const effects = [base, ...encoded.map((value) => (
    typeof value === "string" ? JSON.parse(value) : value
  ))].filter((value) => value && typeof value === "object");
  if (effects.length === 0) return base;
  return {
    ...effects[0],
    acknowledge_frames: effects.some((value) => value.acknowledge_frames),
    frames: effects.flatMap((value) => value.frames ?? []),
    transcripts: effects.flatMap((value) => value.transcripts ?? []),
    schedule_flush: effects.some((value) => value.schedule_flush),
  };
}
