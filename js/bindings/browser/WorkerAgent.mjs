import { agentActions } from "../actions/index.mjs";
import {
  createAgentClient,
  createBrowserVoice,
  defineRuntime,
  freezeJson,
  getEncodedTurnSnapshot,
  getEncodedTurnUsage,
  releaseAgentSession,
  reportError,
  reserveAgentSession,
} from "../internal.mjs";
import { resolveResponsesTransport } from "../runtime/responses-transport.mjs";
import { utf8ByteLength } from "../runtime/utf8.mjs";
import { createIndexedDbDurabilityStore } from "./indexeddb-durability-store.mjs";
import { createProvider as createWebMcpProvider, isProvider as isWebMcpProvider } from "../webmcp/WebMcp.mjs";

const DEFAULT_MAX_PENDING_RPCS = 1_024;
const MAX_RETAINED_RESULTS = 1_024;
export const WORKER_EVENT_BATCH_MAX_EVENTS = 256;
export const WORKER_EVENT_BATCH_MAX_BYTES = 256 * 1024;
export const WORKER_HEARTBEAT_INTERVAL_MS = 10_000;
export const WORKER_HEARTBEAT_TIMEOUT_MS = 30_000;
const WORKER_HEARTBEAT_MISSES = 2;
const PREWARM_TIMEOUT_MS = 15_000;
const PREWARM_RETENTION_MS = 30_000;
const PROTOCOL = "nanocodex.worker-agent.v1";
const eventEncoder = new TextEncoder();
const eventDecoder = new TextDecoder("utf-8", { fatal: true });
const recentImages = new Map();
let prewarmedWorker;

/** Creates a DefaultAgent whose owned driver and browser host live in one module Worker. */
export async function createWorkerAgent(options = {}, workerOptions = {}) {
  workerOptions.signal?.throwIfAborted();
  const providerResolution = resolveWebMcpProvider(options.webMcp);
  const webMcpProvider = providerResolution instanceof Promise
    ? await providerResolution
    : providerResolution;
  let config;
  let reservation;
  try {
    config = serializeConfig(options, webMcpProvider);
    reservation = config.sessionId === undefined
      ? undefined
      : reserveAgentSession(config.sessionId);
  } catch (error) {
    webMcpProvider?.close();
    throw error;
  }
  let worker;
  try {
    const claimed = claimWorker(config.harness, config.module, workerOptions);
    worker = claimed instanceof Promise ? await claimed : claimed;
  } catch (error) {
    webMcpProvider?.close();
    releaseAgentSession(reservation);
    throw error;
  }
  let connection;
  try {
    connection = new WorkerConnection(worker, {
      ...workerOptions,
      hostToolProviders: webMcpProvider ? [webMcpProvider] : [],
    });
  } catch (error) {
    const errors = [error];
    try {
      disposeWorker(worker);
    } catch (cleanupError) {
      appendErrors(errors, cleanupError);
    } finally {
      webMcpProvider?.close();
      releaseAgentSession(reservation);
    }
    throwErrors(errors, "Worker Agent connection construction and cleanup both failed");
  }
  const stopAbort = listenForAbort(workerOptions.signal, (error) => connection.fail(error));
  try {
    const root = await connection.boot(config);
    workerOptions.signal?.throwIfAborted();
    stopAbort();
    return await connection.createClient(root, reservation);
  } catch (error) {
    stopAbort();
    connection.fail(error);
    releaseAgentSession(reservation);
    throw error;
  }
}

/** Starts the exact module Worker and browser harness consumed by the next Agent.create. */
export function prepareWorkerAgent(options = {}, workerOptions = {}) {
  workerOptions.signal?.throwIfAborted();
  const harness = options.harness === false ? false : harnessDescriptor(options, true);
  const key = harnessKey(harness);
  const module = options.module;
  if (prewarmedWorker?.key === key && prewarmedWorker.module === module) {
    const entry = prewarmedWorker;
    return entry.prepare(workerOptions.signal);
  }
  prewarmedWorker?.cancel(new Error("Nanocodex Agent Worker prewarm was replaced"));
  const worker = createWorker(workerOptions);
  const channel = randomId();
  let claimed = false;
  let disposed = false;
  let expiryTimer;
  let startupTimer;
  let cancel;
  const preparationAborts = new Set();
  const clearPreparationAborts = () => {
    for (const stopAbort of preparationAborts) stopAbort();
    preparationAborts.clear();
  };
  const ready = new Promise((resolve, reject) => {
    let settled = false;
    cancel = (error) => {
      if (disposed) return;
      disposed = true;
      if (!settled) {
        settled = true;
        reject(error);
      }
      const cleanupErrors = [];
      try { clearPreparationAborts(); } catch (cleanupError) { appendErrors(cleanupErrors, cleanupError); }
      try { clearTimeout(expiryTimer); } catch (cleanupError) { appendErrors(cleanupErrors, cleanupError); }
      try { clearTimeout(startupTimer); } catch (cleanupError) { appendErrors(cleanupErrors, cleanupError); }
      try {
        if (prewarmedWorker?.worker === worker) prewarmedWorker = undefined;
      } catch (cleanupError) {
        appendErrors(cleanupErrors, cleanupError);
      }
      try { disposeWorker(worker); } catch (cleanupError) { appendErrors(cleanupErrors, cleanupError); }
      for (const cleanupError of cleanupErrors) reportError(cleanupError);
    };
    worker.onmessage = ({ data }) => {
      if (data?.protocol !== PROTOCOL || data.channel !== channel) return;
      if (data.type === "prewarmed") {
        if (settled || disposed) return;
        settled = true;
        clearTimeout(startupTimer);
        worker.onmessage = null;
        worker.onerror = null;
        worker.onmessageerror = null;
        if (!claimed) {
          expiryTimer = setTimeout(
            () => cancel(new Error("Nanocodex prepared Agent Worker expired")),
            PREWARM_RETENTION_MS,
          );
        }
        resolve();
      } else if (data.type === "fatal") {
        cancel(decodeError(data.error));
      }
    };
    worker.onerror = (event) => cancel(new Error(event?.message || "Nanocodex Agent Worker prewarm failed"));
    worker.onmessageerror = () => cancel(new Error("Nanocodex Agent Worker returned an unreadable prewarm message"));
    startupTimer = setTimeout(() => cancel(new Error("Nanocodex Agent Worker prewarm timed out")), PREWARM_TIMEOUT_MS);
    try { worker.postMessage({ protocol: PROTOCOL, channel, type: "prewarm", harness, module }); }
    catch (error) { cancel(error); }
  });
  const entry = {
    cancel,
    get claimed() { return claimed; },
    claim(signal) {
      claimed = true;
      clearPreparationAborts();
      clearTimeout(expiryTimer);
      return abortable(ready, signal, (error) => cancel(error)).then(() => {
        if (disposed) throw new Error("Nanocodex prepared Agent Worker was disposed");
        return worker;
      });
    },
    prepare(signal) {
      if (signal !== undefined) {
        let stopAbort = () => {};
        stopAbort = listenForAbort(signal, (error) => {
          preparationAborts.delete(stopAbort);
          if (!claimed) cancel(error);
        });
        if (!signal.aborted) preparationAborts.add(stopAbort);
      }
      return ready;
    },
    key,
    module,
    ready,
    worker,
  };
  if (!disposed) prewarmedWorker = entry;
  return entry.prepare(workerOptions.signal);
}

/**
 * Installs the reusable package RPC runtime in a Worker-like global scope.
 * Tests and advanced integrations may inject the explicitly local Agent creator.
 */
export function installWorkerAgentRuntime(scope = globalThis, options = {}) {
  if (!scope || typeof scope.postMessage !== "function") {
    throw new TypeError("the Worker Agent runtime requires a Worker-like scope");
  }
  const createAgent = options.createAgent ?? loadAgent;
  const createDurabilityStore = options.createDurabilityStore
    ?? (() => createIndexedDbDurabilityStore());
  const prewarmLocal = options.prewarmLocal ?? prewarmWorkerRuntime;
  const prewarmBoot = options.createAgent === undefined || options.prewarmLocal !== undefined
    ? prewarmLocal
    : undefined;
  let generation = 0;
  let channel;
  let bootPromise;
  let watcher;
  let watcherAgentId;
  let eventsEnabled = false;
  let eventQueue = [];
  let eventQueueBytes = 0;
  let eventFlushScheduled = false;
  let eventDeliveryGeneration = 0;
  let nextAgent = 1;
  let nextResult = 1;
  let nextVoice = 1;
  let nextChunkedEvent = 1;
  const agents = new Map();
  const turns = new Map();
  const results = new Map();
  const voices = new Map();
  const hostToolProviders = new Map();
  const hostToolCalls = new Map();
  let nextHostToolCall = 1;

  const post = (message, expectedGeneration = generation, transfer) => {
    if (expectedGeneration !== generation) return;
    const envelope = { protocol: PROTOCOL, channel, ...message };
    if (transfer) scope.postMessage(envelope, transfer);
    else scope.postMessage(envelope);
  };

  const cleanup = () => {
    eventsEnabled = false;
    stopWatching();
    for (const turn of turns.values()) {
      try { turn.dispose(); } catch (error) { reportError(error); }
    }
    for (const agent of agents.values()) {
      try { agent.dispose(); } catch (error) { reportError(error); }
    }
    for (const voice of voices.values()) {
      try { voice.free(); } catch (error) { reportError(error); }
    }
    turns.clear();
    for (const resultId of [...results.keys()]) {
      try { releaseWorkerResult(results, resultId); } catch (error) { reportError(error); }
    }
    agents.clear();
    voices.clear();
    for (const pending of hostToolCalls.values()) {
      pending.reject(new Error("Worker Agent host tool execution was cancelled"));
    }
    hostToolCalls.clear();
    hostToolProviders.clear();
  };

  const boot = async (message) => {
    generation += 1;
    const currentGeneration = generation;
    cleanup();
    channel = message.channel;
    nextAgent = 1;
    nextResult = 1;
    nextVoice = 1;
    try {
      for (const descriptor of message.config.hostToolProviders ?? []) {
        const provider = createHostToolProxy(descriptor, callHostTool);
        hostToolProviders.set(provider.sourceId, provider);
      }
      const hydration = hydrateConfig(
        message.config,
        createDurabilityStore,
        [...hostToolProviders.values()],
      );
      const preparation = prewarmBoot?.(message.config.harness, {
        module: message.config.module,
      });
      const [config] = await Promise.all([hydration, preparation]);
      const agent = await createAgent(config);
      if (currentGeneration !== generation) {
        agent.dispose();
        return;
      }
      const agentId = `agent-${nextAgent++}`;
      agents.set(agentId, agent);
      post({ type: "ready", root: describeAgent(agentId, agent) }, currentGeneration);
    } catch (error) {
      if (currentGeneration !== generation) return;
      post({ type: "fatal", error: encodeError(error) }, currentGeneration);
      cleanup();
    }
  };

  const handle = async (message, currentGeneration) => {
    try {
      const value = await dispatch(message, {
        agents,
        turns,
        results,
        voices,
        allocateAgent: (agent) => allocateAgent(agent, currentGeneration),
        allocateResult,
        allocateVoice: (voice) => {
          const id = `voice-${nextVoice++}`;
          voices.set(id, voice);
          return id;
        },
        isCurrent: () => currentGeneration === generation,
        moveWatcherFrom,
        setEventsEnabled,
      });
      if (!message.noReply) post({ type: "resolve", id: message.id, value }, currentGeneration);
    } catch (error) {
      post({ type: "reject", id: message.id, error: encodeError(error) }, currentGeneration);
    }
  };

  function allocateAgent(agent, expectedGeneration) {
    if (expectedGeneration !== generation) {
      agent.dispose();
      throw new Error("the Worker Agent child belongs to a replaced runtime");
    }
    const id = `agent-${nextAgent++}`;
    agents.set(id, agent);
    if (eventsEnabled && !watcher) watchAgent(id, agent);
    return describeAgent(id, agent);
  }

  function allocateResult(result) {
    if (results.size >= MAX_RETAINED_RESULTS) {
      result.dispose();
      throw new RangeError(
        `Worker Agent exceeded its bound of ${MAX_RETAINED_RESULTS} retained turn results`,
      );
    }
    const id = `result-${nextResult++}`;
    results.set(id, { active: 0, released: false, result });
    return id;
  }

  function watchAgent(agentId, agent, expectedGeneration = generation) {
    if (!eventsEnabled || watcher) return;
    watcherAgentId = agentId;
    watcher = agent.events.watch({ includeAllSessions: true });
    watcher.onEvent((event, encodedBytes, encodedEvent) => {
      enqueueEvent(event, encodedBytes, encodedEvent, expectedGeneration);
    });
  }

  function stopWatching() {
    watcher?.off();
    watcher = undefined;
    watcherAgentId = undefined;
    eventQueue.length = 0;
    eventQueue = [];
    eventQueueBytes = 0;
    eventFlushScheduled = false;
    eventDeliveryGeneration += 1;
  }

  function setEventsEnabled(enabled) {
    if (typeof enabled !== "boolean") throw new TypeError("Worker Agent event demand must be boolean");
    if (eventsEnabled === enabled) return;
    eventsEnabled = enabled;
    if (!enabled) {
      stopWatching();
      return;
    }
    for (const [agentId, agent] of agents) {
      watchAgent(agentId, agent);
      break;
    }
  }

  function enqueueEvent(event, encodedBytes, encodedEvent, expectedGeneration) {
    if (!eventsEnabled || expectedGeneration !== generation) return;
    const immutable = freezeJson(event);
    const encoded = typeof encodedEvent === "string" ? encodedEvent : undefined;
    const bytes = Number.isSafeInteger(encodedBytes) && encodedBytes >= 0
      ? encodedBytes
      : eventBytes(immutable, encoded);
    if (
      eventQueue.length
      && (
        eventQueue.length >= WORKER_EVENT_BATCH_MAX_EVENTS
        || eventQueueBytes + bytes > WORKER_EVENT_BATCH_MAX_BYTES
      )
    ) {
      flushEvents(expectedGeneration, eventDeliveryGeneration);
    }
    eventQueue.push({ bytes, encoded, event: immutable });
    eventQueueBytes += bytes;
    if (
      eventQueue.length >= WORKER_EVENT_BATCH_MAX_EVENTS
      || eventQueueBytes >= WORKER_EVENT_BATCH_MAX_BYTES
    ) {
      flushEvents(expectedGeneration, eventDeliveryGeneration);
      return;
    }
    if (eventFlushScheduled) return;
    eventFlushScheduled = true;
    const deliveryGeneration = eventDeliveryGeneration;
    queueMicrotask(() => flushEvents(expectedGeneration, deliveryGeneration));
  }

  function flushEvents(expectedGeneration, deliveryGeneration) {
    if (
      expectedGeneration !== generation
      || deliveryGeneration !== eventDeliveryGeneration
      || !eventsEnabled
    ) return;
    const pending = eventQueue;
    eventQueue = [];
    eventQueueBytes = 0;
    eventFlushScheduled = false;
    let head = 0;
    while (head < pending.length) {
      const first = pending[head];
      if (first.bytes > WORKER_EVENT_BATCH_MAX_BYTES) {
        postChunkedEvent(first, expectedGeneration);
        head += 1;
        continue;
      }
      const events = [];
      let encodedBytes = 0;
      while (head < pending.length && events.length < WORKER_EVENT_BATCH_MAX_EVENTS) {
        const entry = pending[head];
        if (entry.bytes > WORKER_EVENT_BATCH_MAX_BYTES) break;
        if (events.length && encodedBytes + entry.bytes > WORKER_EVENT_BATCH_MAX_BYTES) break;
        events.push({ encodedBytes: entry.bytes, event: entry.event });
        encodedBytes += entry.bytes;
        head += 1;
      }
      post({ type: "event.batch", encodedBytes, events }, expectedGeneration);
    }
  }

  function postChunkedEvent(entry, expectedGeneration) {
    const encoded = eventEncoder.encode(entry.encoded ?? JSON.stringify(entry.event));
    const id = `event-${nextChunkedEvent++}`;
    for (let offset = 0, index = 0; offset < encoded.byteLength; index += 1) {
      const end = Math.min(offset + WORKER_EVENT_BATCH_MAX_BYTES, encoded.byteLength);
      const chunk = encoded.slice(offset, end);
      post({
        type: "event.chunk",
        chunk,
        encodedBytes: encoded.byteLength,
        id,
        index,
        last: end === encoded.byteLength,
      }, expectedGeneration, [chunk.buffer]);
      offset = end;
    }
  }

  function moveWatcherFrom(agentId) {
    if (watcherAgentId !== agentId) return;
    watcher?.off();
    watcher = undefined;
    watcherAgentId = undefined;
    for (const [candidateId, candidate] of agents) {
      if (candidateId !== agentId) {
        watchAgent(candidateId, candidate);
        break;
      }
    }
  }

  scope.onmessage = ({ data: message }) => {
    if (message?.protocol !== PROTOCOL) return;
    if (message.type === "prewarm") {
      const currentGeneration = generation;
      void Promise.resolve().then(() => prewarmLocal(message.harness, {
        module: message.module,
      })).then(
        () => {
          if (currentGeneration === generation) {
            scope.postMessage({ protocol: PROTOCOL, channel: message.channel, type: "prewarmed" });
          }
        },
        (error) => {
          if (currentGeneration === generation) {
            scope.postMessage({
              protocol: PROTOCOL,
              channel: message.channel,
              type: "fatal",
              error: encodeError(error),
            });
          }
        },
      );
      return;
    }
    if (message.type === "boot") {
      bootPromise = boot(message);
      return;
    }
    if (message.channel !== channel || !bootPromise) return;
    const currentGeneration = generation;
    if (message.type === "host-tool.resolve" || message.type === "host-tool.reject") {
      settleHostToolCall(message);
      return;
    }
    if (message.type === "host-tools.update") {
      hostToolProviders.get(message.sourceId)?.update(message.definitions);
      return;
    }
    if (message.type === "liveness.ping") {
      if (Number.isSafeInteger(message.sequence) && message.sequence > 0) {
        post({ type: "liveness.pong", sequence: message.sequence }, currentGeneration);
      }
      return;
    }
    void bootPromise.then(() => handle(message, currentGeneration)).catch((error) => {
      post({ type: "reject", id: message.id, error: encodeError(error) }, currentGeneration);
    });
  };

  return Object.freeze({ dispose() { generation += 1; cleanup(); scope.onmessage = null; } });

  function callHostTool(sourceId, name, input, context = {}) {
    const callId = `host-tool-${nextHostToolCall++}`;
    const signal = context.signal ?? new AbortController().signal;
    signal.throwIfAborted?.();
    return new Promise((resolve, reject) => {
      let settled = false;
      const finish = (complete, value) => {
        if (settled) return;
        settled = true;
        signal.removeEventListener?.("abort", abort);
        hostToolCalls.delete(callId);
        complete(value);
      };
      const abort = () => {
        post({ type: "host-tool.cancel", callId }, currentGeneration());
        finish(reject, signal.reason ?? new Error("host tool execution was cancelled"));
      };
      hostToolCalls.set(callId, {
        resolve: (value) => finish(resolve, value),
        reject: (error) => finish(reject, error),
      });
      signal.addEventListener?.("abort", abort, { once: true });
      post({
        type: "host-tool.call",
        callId,
        sourceId,
        name,
        input,
        context: {
          callId: context.callId,
          parentCallId: context.parentCallId,
          sessionId: context.sessionId,
        },
      }, currentGeneration());
    });
  }

  function settleHostToolCall(message) {
    const pending = hostToolCalls.get(message.callId);
    if (!pending) return;
    if (message.type === "host-tool.resolve") pending.resolve(message.value);
    else pending.reject(decodeError(message.error));
  }

  function currentGeneration() { return generation; }
}

async function dispatch(message, state) {
  const { agents, turns, results, voices } = state;
  switch (message.type) {
    case "prompt": {
      const agent = required(agents, message.agentId, "agent");
      const turn = agent.turn.prompt(message.options);
      if (turns.has(message.turnId)) throw new Error(`duplicate Worker Agent turn: ${message.turnId}`);
      turns.set(message.turnId, turn);
      try {
        return await turn.accepted();
      } catch (error) {
        if (turns.get(message.turnId) === turn) turns.delete(message.turnId);
        turn.dispose();
        throw error;
      }
    }
    case "events": {
      state.setEventsEnabled(message.enabled);
      return undefined;
    }
    case "rpc": break;
    default: throw new Error(`unknown Worker Agent message: ${message.type}`);
  }
  const { method, args = [] } = message;
  if (method === "turn.result") {
    const turn = required(turns, args[0], "turn");
    let result;
    try {
      result = await turn.result();
    } finally {
      if (turns.get(args[0]) === turn) turns.delete(args[0]);
      turn.dispose();
    }
    if (!state.isCurrent()) {
      result.dispose();
      throw new Error("the Worker Agent completion belongs to a replaced runtime");
    }
    const resultId = state.allocateResult(result);
    return { finalMessage: result.finalMessage, resultId };
  }
  if (method === "turn.steer") return required(turns, args[0], "turn").steer(args[1]);
  if (method === "turn.cancel") return required(turns, args[0], "turn").cancel();
  if (method === "turn.dispose") {
    const turn = turns.get(args[0]);
    turns.delete(args[0]);
    turn?.dispose();
    return;
  }
  if (method === "result.snapshot") {
    return withWorkerResult(results, args[0], getEncodedTurnSnapshot);
  }
  if (method === "result.usage") {
    return withWorkerResult(results, args[0], getEncodedTurnUsage);
  }
  if (method === "result.dispose") {
    releaseWorkerResult(results, args[0]);
    return;
  }
  if (method === "voice.start") return required(voices, args[0], "voice").start();
  if (method === "voice.callBody") return required(voices, args[0], "voice").callBody(args[1]);
  if (method === "voice.completeCall") {
    return required(voices, args[0], "voice").completeCall(args[1], args[2]);
  }
  if (method === "voice.sidebandUrl") {
    return required(voices, args[0], "voice").sidebandUrl(args[1]);
  }
  if (method === "voice.sidebandOpened") {
    return required(voices, args[0], "voice").sidebandOpened();
  }
  if (method === "voice.sidebandClosed") {
    return required(voices, args[0], "voice").sidebandClosed(args[1]);
  }
  if (method === "voice.framesSent") {
    return required(voices, args[0], "voice").framesSent(args[1]);
  }
  if (method === "voice.requiresAgentAdmission") {
    return required(voices, args[0], "voice").requiresAgentAdmission(args[1]);
  }
  if (method === "voice.realtimeMessage") {
    return required(voices, args[0], "voice").realtimeMessage(args[1]);
  }
  if (method === "voice.agentEvent") return required(voices, args[0], "voice").agentEvent(args[1]);
  if (method === "voice.flush") return required(voices, args[0], "voice").flush(args[1]);
  if (method === "voice.stop") return required(voices, args[0], "voice").stop();
  if (method === "voice.cancel") return required(voices, args[0], "voice").cancel();
  if (method === "voice.preferredPhysicalInput") {
    return required(voices, args[0], "voice").preferredPhysicalInput(args[1], args[2]);
  }
  if (method === "voice.dispose") {
    const voice = voices.get(args[0]);
    voices.delete(args[0]);
    voice?.free();
    return;
  }
  const agent = required(agents, args[0], "agent");
  if (method === "agent.voice.create") {
    return state.allocateVoice(await createBrowserVoice(agent, args[1]));
  }
  if (method === "agent.fork") {
    if (args[1] === undefined) return state.allocateAgent(await agent.session.fork());
    return withWorkerResult(results, args[1], async (at) => (
      state.allocateAgent(await agent.session.fork({ at }))
    ));
  }
  if (method === "agent.spawn") return state.allocateAgent(await agent.session.spawn());
  if (method === "agent.compact") return agent.session.compact();
  if (method === "agent.context") return agent.session.context();
  if (method === "agent.setThinking") return agent.session.setThinking(args[1]);
  if (method === "agent.setFastMode") return agent.session.setFastMode(args[1]);
  if (method === "agent.appendDeveloperMessage") return agent.session.appendDeveloperMessage(args[1]);
  if (method === "agent.realtime.start") return agent.session.realtime.start();
  if (method === "agent.realtime.end") return agent.session.realtime.end();
  if (method === "agent.realtime.delegation") {
    return agent.session.realtime.delegation(args[1], JSON.parse(args[2]));
  }
  if (method === "agent.realtime.tailDelegation") {
    return agent.session.realtime.tailDelegation(JSON.parse(args[1]));
  }
  if (method === "agent.shutdown") {
    state.moveWatcherFrom(args[0]);
    return agent.session.shutdown();
  }
  if (method === "agent.dispose") {
    state.moveWatcherFrom(args[0]);
    agents.delete(args[0]);
    agent.dispose();
    return;
  }
  throw new Error(`unknown Worker Agent RPC method: ${method}`);
}

function required(map, id, kind) {
  const value = map.get(id);
  if (!value) throw new Error(`unknown Worker Agent ${kind}: ${id}`);
  return value;
}

async function withWorkerResult(results, resultId, action) {
  const entry = required(results, resultId, "turn result");
  entry.active += 1;
  try {
    return await action(entry.result);
  } finally {
    entry.active -= 1;
    if (entry.released && entry.active === 0) entry.result.dispose();
  }
}

function releaseWorkerResult(results, resultId) {
  const entry = results.get(resultId);
  if (!entry) return;
  results.delete(resultId);
  entry.released = true;
  if (entry.active === 0) entry.result.dispose();
}

function describeAgent(agentId, agent) {
  return { handleId: agentId, agentId: agent.agentId, sessionId: agent.sessionId };
}

class WorkerConnection {
  constructor(worker, options) {
    this.worker = worker;
    if (options.onFailure !== undefined && typeof options.onFailure !== "function") {
      throw new TypeError("onFailure must be a function");
    }
    this.onFailure = options.onFailure;
    this.channel = randomId();
    this.maxPending = options.maxPendingRpcs === undefined
      ? DEFAULT_MAX_PENDING_RPCS
      : positiveInteger(options.maxPendingRpcs, "maxPendingRpcs");
    this.nextRpc = 1;
    this.nextTurn = 1;
    this.nextHeartbeat = 1;
    this.pending = new Map();
    this.listeners = new Set();
    this.clients = new Set();
    this.clientByRaw = new Map();
    this.constructingAgents = new Set();
    this.rawAgents = new Set();
    this.chunkedEvent = undefined;
    this.agents = 0;
    this.voices = 0;
    this.turns = 0;
    this.results = 0;
    this.operations = 0;
    this.heartbeatMisses = 0;
    this.heartbeatSequence = undefined;
    this.heartbeatTimer = undefined;
    this.closed = false;
    this.closeError = undefined;
    this.hostToolProviders = new Map();
    this.hostToolCalls = new Map();
    for (const provider of options.hostToolProviders ?? []) {
      if (!provider || typeof provider.sourceId !== "string"
          || typeof provider.definitions !== "function"
          || typeof provider.resolve !== "function") {
        throw new TypeError("Worker Agent host tool provider is invalid");
      }
      if (this.hostToolProviders.has(provider.sourceId)) {
        throw new Error(`duplicate Worker Agent host tool provider: ${provider.sourceId}`);
      }
      const stop = provider.subscribe?.((definitions) => {
        if (this.closed) return;
        try {
          assertCloneable(definitions, `host tool provider ${provider.sourceId} definitions`);
          this.send({
            type: "host-tools.update",
            sourceId: provider.sourceId,
            definitions,
          });
        } catch (error) {
          this.fail(error);
        }
      });
      this.hostToolProviders.set(provider.sourceId, { provider, stop });
    }
    worker.onmessage = ({ data }) => this.receive(data);
    worker.onerror = (event) => this.fail(new Error(event?.message || "Nanocodex Agent Worker failed"));
    worker.onmessageerror = () => this.fail(new Error("Nanocodex Agent Worker returned an unreadable message"));
    this.scheduleHeartbeat();
  }

  boot(config) {
    const promise = this.pendingCall("boot");
    try { this.send({ type: "boot", config }); }
    catch (error) { this.rejectPending("boot", error); }
    return promise;
  }

  runtime() {
    return defineRuntime({
      key: "browser-worker-wasm",
      name: "Nanocodex Browser Worker WASM",
      type: "browser",
      create: (descriptor) => this.rawAgent(descriptor),
      adopt: () => this.assertOpen(),
      subscribe: (listener) => this.subscribe(listener),
      release: (raw) => {
        const client = this.clientByRaw.get(raw);
        if (client !== undefined) this.clients.delete(client);
        this.clientByRaw.delete(raw);
      },
      dispose: (raw) => {
        if (!raw.released) {
          raw.released = true;
          this.rawAgents.delete(raw);
          if (!this.constructingAgents.has(raw.handleId)) {
            this.sendBestEffort("agent.dispose", [raw.handleId]);
          }
          this.agents -= 1;
        }
        this.closeIfIdle();
      },
      decorate: (agent, raw) => {
        const decorated = agent.extend(agentActions());
        this.clients.add(decorated);
        this.clientByRaw.set(raw, decorated);
        return decorated;
      },
    });
  }

  async createClient(root, reservation) {
    const handleId = root?.handleId ?? root?.agentId;
    this.constructingAgents.add(handleId);
    try {
      return await createAgentClient(this.runtime(), root, reservation);
    } catch (error) {
      if (this.closed) throw error;
      try {
        await this.rpc("agent.dispose", [handleId]);
      } catch (rollbackError) {
        throw new AggregateError(
          [error, rollbackError],
          "Worker Agent client construction and boot rollback both failed",
        );
      }
      throw error;
    } finally {
      this.constructingAgents.delete(handleId);
      this.closeIfIdle();
    }
  }

  rawAgent(descriptor) {
    this.assertOpen();
    this.agents += 1;
    const connection = this;
    const handleId = descriptor.handleId ?? descriptor.agentId;
    const agentId = descriptor.handleId === undefined
      ? descriptor.sessionId
      : descriptor.agentId;
    const raw = {
      handleId,
      agentId,
      sessionId: descriptor.sessionId,
      released: false,
      prompt(input, id) { return connection.prompt(handleId, { input, ...(id === undefined ? {} : { id }) }); },
      promptContent(input, id) { return connection.prompt(handleId, { input: JSON.parse(input), ...(id === undefined ? {} : { id }) }); },
      fork: async () => connection.rawAgent(await connection.rpc("agent.fork", [handleId])),
      forkFrom: async (result) => {
        if (result?.connection !== connection) {
          throw new TypeError("historical forks require a result from the same Worker Agent");
        }
        return connection.rawAgent(await connection.rpc("agent.fork", [handleId, result.resultId]));
      },
      spawn: async () => connection.rawAgent(await connection.rpc("agent.spawn", [handleId])),
      compact: () => connection.rpc("agent.compact", [handleId]),
      context: async () => JSON.stringify(await connection.rpc("agent.context", [handleId])),
      setThinking: (value) => connection.rpc("agent.setThinking", [handleId, value]),
      setFastMode: (value) => connection.rpc("agent.setFastMode", [handleId, value]),
      appendDeveloperMessage: async (text) => JSON.stringify(await connection.rpc("agent.appendDeveloperMessage", [handleId, text])),
      startRealtimeConversation: async () => JSON.stringify(await connection.rpc("agent.realtime.start", [handleId])),
      endRealtimeConversation: async () => JSON.stringify(await connection.rpc("agent.realtime.end", [handleId])),
      realtimeDelegation: (input, transcript) => connection.rpc("agent.realtime.delegation", [handleId, input, transcript]),
      realtimeTailDelegation: (transcript) => connection.rpc("agent.realtime.tailDelegation", [handleId, transcript]),
      browserVoice: async (voice) => connection.rawVoice(
        await connection.rpc("agent.voice.create", [handleId, voice]),
      ),
      shutdown: () => connection.rpc("agent.shutdown", [handleId]),
      free() {},
    };
    this.rawAgents.add(raw);
    return raw;
  }

  rawVoice(voiceId) {
    this.assertOpen();
    this.voices += 1;
    const connection = this;
    let released = false;
    return {
      start: () => connection.rpc("voice.start", [voiceId]),
      callBody: (sdp) => connection.rpc("voice.callBody", [voiceId, sdp]),
      completeCall: (body, location) => connection.rpc(
        "voice.completeCall",
        [voiceId, body, location],
      ),
      sidebandUrl: (callId) => connection.rpc("voice.sidebandUrl", [voiceId, callId]),
      sidebandOpened: () => connection.rpc("voice.sidebandOpened", [voiceId]),
      sidebandClosed: (connectedMs) => connection.rpc(
        "voice.sidebandClosed",
        [voiceId, connectedMs],
      ),
      framesSent: (count) => connection.rpc("voice.framesSent", [voiceId, count]),
      requiresAgentAdmission: (payload) => connection.rpc(
        "voice.requiresAgentAdmission",
        [voiceId, payload],
      ),
      realtimeMessage: (payload) => connection.rpc("voice.realtimeMessage", [voiceId, payload]),
      agentEvent: (event) => connection.rpc("voice.agentEvent", [voiceId, event]),
      flush: (finalChunk) => connection.rpc("voice.flush", [voiceId, finalChunk]),
      stop: () => connection.rpc("voice.stop", [voiceId]),
      cancel: () => connection.rpc("voice.cancel", [voiceId]),
      preferredPhysicalInput: (current, labels) => connection.rpc(
        "voice.preferredPhysicalInput",
        [voiceId, current, labels],
      ),
      free() {
        if (released) return;
        released = true;
        connection.sendBestEffort("voice.dispose", [voiceId]);
        connection.voices -= 1;
        connection.closeIfIdle();
      },
    };
  }

  prompt(agentId, options) {
    this.assertOpen();
    const connection = this;
    assertCloneable(options, "turn prompt");
    const turnId = `turn-${this.nextTurn++}`;
    const accepted = this.pendingCall(turnId);
    void accepted.catch(() => {});
    this.turns += 1;
    try { this.send({ type: "prompt", id: turnId, agentId, turnId, options }); }
    catch (error) { this.rejectPending(turnId, error); }
    let result;
    let disposed = false;
    let retained = true;
    return {
      accepted() { return accepted; },
      result() {
        if (disposed && !result) {
          return Promise.reject(new Error("the Nanocodex turn has been disposed"));
        }
        result ||= accepted
          .then(() => connectionResult(thisConnection(), turnId))
          .finally(release);
        return result;
      },
      steer(input) { return accepted.then(() => thisConnection().rpc("turn.steer", [turnId, { input }])); },
      steerContent(input) { return accepted.then(() => thisConnection().rpc("turn.steer", [turnId, { input: JSON.parse(input) }])); },
      cancel() { return accepted.then(() => thisConnection().rpc("turn.cancel", [turnId])); },
      free() {
        if (disposed) return;
        disposed = true;
        if (result) return;
        thisConnection().sendBestEffort("turn.dispose", [turnId]);
        release();
      },
    };
    function thisConnection() { return connection; }
    function release() {
      if (!retained) return;
      retained = false;
      connection.turns -= 1;
      connection.closeIfIdle();
    }
  }

  rpc(method, args) {
    this.assertOpen();
    assertCloneable(args, method);
    const id = `rpc-${this.nextRpc++}`;
    const promise = this.pendingCall(id);
    this.operations += 1;
    try { this.send({ type: "rpc", id, method, args }); }
    catch (error) { this.rejectPending(id, error); }
    return promise.finally(() => {
      if (this.closed) return;
      this.operations -= 1;
      queueMicrotask(() => queueMicrotask(() => this.closeIfIdle()));
    });
  }

  sendBestEffort(method, args) {
    if (this.closed) return;
    const id = `rpc-${this.nextRpc++}`;
    try { this.send({ type: "rpc", id, method, args, noReply: true }); } catch {}
  }

  adoptResult() {
    this.assertOpen();
    this.results += 1;
  }

  releaseResult(resultId) {
    if (this.results === 0) return;
    this.sendBestEffort("result.dispose", [resultId]);
    this.results -= 1;
    this.closeIfIdle();
  }

  closeIfIdle() {
    if (
      this.constructingAgents.size === 0
      && this.agents === 0
      && this.voices === 0
      && this.turns === 0
      && this.results === 0
      && this.operations === 0
    ) {
      this.close(new Error("the Nanocodex Agent Worker has been disposed"));
    }
  }

  subscribe(listener) {
    this.assertOpen();
    const enable = this.listeners.size === 0;
    this.listeners.add(listener);
    if (enable) {
      try { this.send({ type: "events", enabled: true, noReply: true }); }
      catch (error) {
        this.listeners.delete(listener);
        throw error;
      }
    }
    let active = true;
    return () => {
      if (!active) return;
      active = false;
      this.listeners.delete(listener);
      if (this.listeners.size === 0 && !this.closed) {
        try { this.send({ type: "events", enabled: false, noReply: true }); } catch {}
      }
    };
  }

  pendingCall(id) {
    this.assertOpen();
    if (this.pending.size >= this.maxPending) {
      throw new RangeError(`Worker Agent exceeded its bound of ${this.maxPending} pending RPCs`);
    }
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }

  send(message) {
    this.assertOpen();
    this.worker.postMessage({ protocol: PROTOCOL, channel: this.channel, ...message });
  }

  receive(message) {
    if (this.closed || message?.protocol !== PROTOCOL || message.channel !== this.channel) return;
    if (message.type === "liveness.pong") {
      this.receiveHeartbeat(message.sequence);
      return;
    }
    if (message.type === "event.batch") {
      if (!Array.isArray(message.events)
        || message.events.length > WORKER_EVENT_BATCH_MAX_EVENTS
        || !Number.isSafeInteger(message.encodedBytes)
        || message.encodedBytes < 0
        || message.encodedBytes > WORKER_EVENT_BATCH_MAX_BYTES) {
        this.fail(new Error("Nanocodex Agent Worker returned an invalid event batch"));
        return;
      }
      let encodedBytes = 0;
      for (const entry of message.events) {
        if (
          !Number.isSafeInteger(entry?.encodedBytes)
          || entry.encodedBytes < 0
          || entry.encodedBytes > WORKER_EVENT_BATCH_MAX_BYTES
        ) {
          this.fail(new Error("Nanocodex Agent Worker returned an invalid event size"));
          return;
        }
        encodedBytes += entry.encodedBytes;
      }
      if (encodedBytes !== message.encodedBytes) {
        this.fail(new Error("Nanocodex Agent Worker returned inconsistent event batch bytes"));
        return;
      }
      for (const entry of message.events) this.emitEvent(entry.event, entry.encodedBytes);
      return;
    }
    if (message.type === "event.chunk") {
      this.receiveEventChunk(message);
      return;
    }
    if (message.type === "host-tool.call") {
      void this.executeHostTool(message);
      return;
    }
    if (message.type === "host-tool.cancel") {
      this.hostToolCalls.get(message.callId)?.abort(
        new Error("Worker Agent cancelled the host tool execution"),
      );
      return;
    }
    if (message.type === "ready") return this.resolvePending("boot", message.root);
    if (message.type === "fatal") return this.fail(decodeError(message.error));
    if (message.type === "resolve") return this.resolvePending(message.id, message.value);
    if (message.type === "reject") return this.rejectPending(message.id, decodeError(message.error));
  }

  async executeHostTool(message) {
    if (typeof message.callId !== "string" || !message.callId
        || typeof message.sourceId !== "string" || !message.sourceId
        || typeof message.name !== "string" || !message.name
        || this.hostToolCalls.has(message.callId)) {
      this.fail(new Error("Nanocodex Agent Worker requested an invalid host tool call"));
      return;
    }
    const entry = this.hostToolProviders.get(message.sourceId);
    const tool = entry?.provider.resolve(message.name);
    if (!tool || typeof tool.handler !== "function") {
      this.send({
        type: "host-tool.reject",
        callId: message.callId,
        error: encodeError(new Error(`unknown host tool: ${message.name}`)),
      });
      return;
    }
    const controller = new AbortController();
    this.hostToolCalls.set(message.callId, controller);
    try {
      const value = await tool.handler(message.input, {
        callId: message.context?.callId ?? message.callId,
        parentCallId: message.context?.parentCallId ?? "",
        sessionId: message.context?.sessionId ?? "default",
        signal: controller.signal,
      });
      if (this.closed || this.hostToolCalls.get(message.callId) !== controller) return;
      assertCloneable(value, `host tool ${message.name} result`);
      this.send({ type: "host-tool.resolve", callId: message.callId, value });
    } catch (error) {
      if (this.closed || this.hostToolCalls.get(message.callId) !== controller) return;
      this.send({ type: "host-tool.reject", callId: message.callId, error: encodeError(error) });
    } finally {
      if (this.hostToolCalls.get(message.callId) === controller) {
        this.hostToolCalls.delete(message.callId);
      }
    }
  }

  resolvePending(id, value) {
    const pending = this.pending.get(id);
    if (!pending) return;
    this.pending.delete(id);
    pending.resolve(value);
  }

  rejectPending(id, error) {
    const pending = this.pending.get(id);
    if (!pending) return;
    this.pending.delete(id);
    pending.reject(error);
  }

  scheduleHeartbeat() {
    if (this.closed) return;
    clearTimeout(this.heartbeatTimer);
    this.heartbeatTimer = setTimeout(
      () => this.sendHeartbeat(),
      WORKER_HEARTBEAT_INTERVAL_MS,
    );
  }

  sendHeartbeat() {
    if (this.closed) return;
    const sequence = this.nextHeartbeat++;
    this.heartbeatSequence = sequence;
    this.heartbeatTimer = setTimeout(
      () => this.missHeartbeat(sequence),
      WORKER_HEARTBEAT_TIMEOUT_MS,
    );
    try {
      this.send({ type: "liveness.ping", sequence });
    } catch (error) {
      this.fail(error);
    }
  }

  receiveHeartbeat(sequence) {
    if (sequence !== this.heartbeatSequence) return;
    clearTimeout(this.heartbeatTimer);
    this.heartbeatSequence = undefined;
    this.heartbeatMisses = 0;
    this.scheduleHeartbeat();
  }

  missHeartbeat(sequence) {
    if (this.closed || sequence !== this.heartbeatSequence) return;
    this.heartbeatMisses += 1;
    if (this.heartbeatMisses < WORKER_HEARTBEAT_MISSES) {
      this.sendHeartbeat();
      return;
    }
    const error = new Error(
      "Nanocodex Agent Worker stopped responding; retry to start a fresh Worker",
    );
    error.code = "worker_unresponsive";
    this.fail(error);
  }

  emitEvent(event, encodedBytes) {
    const immutable = freezeJson(event);
    for (const listener of this.listeners) listener(immutable, encodedBytes);
  }

  receiveEventChunk(message) {
    const chunk = message.chunk instanceof Uint8Array
      ? message.chunk
      : message.chunk instanceof ArrayBuffer ? new Uint8Array(message.chunk) : undefined;
    if (
      !chunk
      || chunk.byteLength > WORKER_EVENT_BATCH_MAX_BYTES
      || !Number.isSafeInteger(message.encodedBytes)
      || message.encodedBytes <= WORKER_EVENT_BATCH_MAX_BYTES
      || !Number.isSafeInteger(message.index)
      || message.index < 0
      || typeof message.id !== "string"
      || typeof message.last !== "boolean"
    ) {
      this.fail(new Error("Nanocodex Agent Worker returned an invalid chunked event"));
      return;
    }
    let entry = this.chunkedEvent;
    if (message.index === 0) {
      if (entry) {
        this.fail(new Error("Nanocodex Agent Worker interleaved chunked events"));
        return;
      }
      entry = { chunks: [], encodedBytes: message.encodedBytes, id: message.id, receivedBytes: 0 };
      this.chunkedEvent = entry;
    }
    if (
      !entry
      || entry.id !== message.id
      || entry.encodedBytes !== message.encodedBytes
      || entry.chunks.length !== message.index
      || entry.receivedBytes + chunk.byteLength > entry.encodedBytes
    ) {
      this.fail(new Error("Nanocodex Agent Worker returned an out-of-order chunked event"));
      return;
    }
    entry.chunks.push(chunk);
    entry.receivedBytes += chunk.byteLength;
    if (!message.last) return;
    if (entry.receivedBytes !== entry.encodedBytes) {
      this.fail(new Error("Nanocodex Agent Worker returned an incomplete chunked event"));
      return;
    }
    const encoded = new Uint8Array(entry.encodedBytes);
    let offset = 0;
    for (const part of entry.chunks) {
      encoded.set(part, offset);
      offset += part.byteLength;
    }
    this.chunkedEvent = undefined;
    try {
      this.emitEvent(JSON.parse(eventDecoder.decode(encoded)), entry.encodedBytes);
    } catch (error) {
      this.fail(new Error("Nanocodex Agent Worker returned an invalid encoded event", { cause: error }));
    }
  }

  assertOpen() {
    if (this.closed) {
      throw this.closeError ?? new Error("the Nanocodex Agent Worker has been disposed");
    }
  }

  fail(error) {
    if (this.closed) return;
    const onFailure = this.onFailure;
    this.onFailure = undefined;
    try { this.close(error); } catch (failure) { reportError(failure); }
    if (onFailure !== undefined) {
      try { onFailure(error); } catch (failure) { reportError(failure); }
    }
  }

  close(error) {
    if (this.closed) return;
    this.closed = true;
    this.closeError = error;
    clearTimeout(this.heartbeatTimer);
    this.heartbeatTimer = undefined;
    this.heartbeatSequence = undefined;
    const worker = this.worker;
    for (const raw of this.rawAgents) raw.released = true;
    this.rawAgents.clear();
    this.agents = 0;
    try {
      disposeWorker(worker);
    } finally {
      for (const client of [...this.clients]) {
        try { client.dispose(); } catch (failure) { reportError(failure); }
      }
      this.clients.clear();
      this.clientByRaw.clear();
      for (const pending of this.pending.values()) pending.reject(error);
      this.pending.clear();
      this.listeners.clear();
      this.chunkedEvent = undefined;
      this.onFailure = undefined;
      for (const controller of this.hostToolCalls.values()) controller.abort(error);
      this.hostToolCalls.clear();
      for (const { provider, stop } of this.hostToolProviders.values()) {
        try { stop?.(); } catch (failure) { reportError(failure); }
        try { provider.close?.(); } catch (failure) { reportError(failure); }
      }
      this.hostToolProviders.clear();
    }
  }
}

async function connectionResult(connection, turnId) {
  const result = await connection.rpc("turn.result", [turnId]);
  if (!result || typeof result.finalMessage !== "string" || typeof result.resultId !== "string") {
    if (typeof result?.resultId === "string") {
      connection.sendBestEffort("result.dispose", [result.resultId]);
    }
    throw new TypeError("Nanocodex Agent Worker returned an invalid turn result handle");
  }
  let released = false;
  connection.adoptResult();
  return {
    connection,
    finalMessage: result.finalMessage,
    resultId: result.resultId,
    snapshot() {
      if (released) return Promise.reject(new Error("the Nanocodex turn result has been disposed"));
      return connection.rpc("result.snapshot", [result.resultId]);
    },
    usage() {
      if (released) return Promise.reject(new Error("the Nanocodex turn result has been disposed"));
      return connection.rpc("result.usage", [result.resultId]);
    },
    free() {
      if (released) return;
      released = true;
      connection.releaseResult(result.resultId);
    },
  };
}

function createWorker(options) {
  const supplied = options.worker ?? options.workerFactory;
  if (supplied !== undefined) {
    const worker = typeof supplied === "function" ? supplied() : supplied;
    if (!worker || typeof worker.postMessage !== "function") throw new TypeError("worker must be Worker-like");
    return worker;
  }
  if (typeof Worker !== "function") throw new Error("this environment does not provide module Workers");
  return new Worker(new URL("./agent.worker.mjs", import.meta.url), { type: "module", name: "nanocodex-agent" });
}

function claimWorker(harness, module, options) {
  options.signal?.throwIfAborted();
  if (options.worker !== undefined || options.workerFactory !== undefined) {
    return createWorker(options);
  }
  const entry = prewarmedWorker;
  if (
    !entry
    || entry.key !== harnessKey(harness)
    || entry.module !== module
  ) return createWorker(options);
  prewarmedWorker = undefined;
  return entry.claim(options.signal);
}

function disposeWorker(worker) {
  const errors = [];
  for (const property of ["onmessage", "onerror", "onmessageerror"]) {
    try { worker[property] = null; } catch (error) { errors.push(error); }
  }
  try { worker.terminate?.(); } catch (error) { errors.push(error); }
  throwErrors(errors, "Nanocodex Agent Worker cleanup failed");
}

function appendErrors(errors, error) {
  if (error instanceof AggregateError) errors.push(...error.errors);
  else errors.push(error);
}

function throwErrors(errors, message) {
  if (errors.length === 1) throw errors[0];
  if (errors.length > 1) throw new AggregateError(errors, message);
}

function abortable(promise, signal, abort) {
  if (signal === undefined) return promise;
  if (signal.aborted) {
    abort(signal.reason);
    return Promise.reject(signal.reason);
  }
  return new Promise((resolve, reject) => {
    let settled = false;
    let stopAbort = () => {};
    stopAbort = listenForAbort(signal, (error) => {
      abort(error);
      settle(reject, error);
    });
    Promise.resolve(promise).then(
      (value) => settle(resolve, value),
      (error) => settle(reject, error),
    );

    function settle(complete, value) {
      if (settled) return;
      settled = true;
      stopAbort();
      complete(value);
    }
  });
}

function listenForAbort(signal, listener) {
  if (signal === undefined) return () => {};
  let listening = true;
  const onAbort = () => {
    if (!listening) return;
    listening = false;
    signal.removeEventListener("abort", onAbort);
    listener(signal.reason);
  };
  if (signal.aborted) onAbort();
  else signal.addEventListener("abort", onAbort, { once: true });
  return () => {
    if (!listening) return;
    listening = false;
    signal.removeEventListener("abort", onAbort);
  };
}

function resolveWebMcpProvider(value) {
  if (value === undefined || value === false) return undefined;
  if (!isWebMcpProvider(value)) {
    return createWebMcpProvider(value === true ? {} : value).then(settleProvider);
  }
  const settling = value.settled?.();
  return settling instanceof Promise
    ? settling.then(() => value, (error) => closeRejectedProvider(value, error))
    : value;
}

async function settleProvider(provider) {
  try {
    await provider.settled?.();
    return provider;
  } catch (error) { return closeRejectedProvider(provider, error); }
}

function closeRejectedProvider(provider, error) {
  try { provider.close?.(); }
  catch (cleanupError) {
    throw new AggregateError([error, cleanupError], "WebMCP discovery and cleanup failed");
  }
  throw error;
}

function createHostToolProxy(descriptor, call) {
  if (!descriptor || typeof descriptor !== "object" || Array.isArray(descriptor)
      || typeof descriptor.sourceId !== "string" || !descriptor.sourceId
      || !Array.isArray(descriptor.definitions)) {
    throw new TypeError("Worker Agent host tool provider descriptor is invalid");
  }
  let definitions = cloneDefinitions(descriptor.definitions, descriptor.sourceId);
  const provider = {
    sourceId: descriptor.sourceId,
    kind: descriptor.kind ?? "attached",
    mode: descriptor.mode ?? "attached-over-cloud",
    deferred: descriptor.deferred ?? true,
    definitions: () => definitions,
    resolve(name) {
      if (!definitions.some((definition) => definition.name === name)) return undefined;
      return Object.freeze({
        name,
        parallelSafe: false,
        handler: (input, context) => call(descriptor.sourceId, name, input, context),
      });
    },
    update(next) {
      definitions = cloneDefinitions(next, descriptor.sourceId);
    },
  };
  return Object.freeze(provider);
}

function cloneDefinitions(value, sourceId) {
  if (!Array.isArray(value)) {
    throw new TypeError(`host tool provider ${sourceId} definitions must be an array`);
  }
  const names = new Set();
  const definitions = JSON.parse(JSON.stringify(value));
  for (const definition of definitions) {
    if (!definition || typeof definition !== "object" || Array.isArray(definition)
        || typeof definition.name !== "string" || !definition.name
        || names.has(definition.name)) {
      throw new TypeError(`host tool provider ${sourceId} returned invalid definitions`);
    }
    names.add(definition.name);
  }
  return Object.freeze(definitions.map((definition) => Object.freeze(definition)));
}

function serializeConfig(options, webMcpProvider) {
  const config = { ...options };
  delete config.webMcp;
  if (webMcpProvider) {
    config.hostToolProviders = [{
      sourceId: webMcpProvider.sourceId,
      kind: webMcpProvider.kind,
      mode: webMcpProvider.mode,
      deferred: webMcpProvider.deferred,
      definitions: webMcpProvider.definitions(),
    }];
  }
  const workerDurability = config.durability !== false;
  if (!workerDurability) delete config.durability;
  const stableThreadId = nonEmptyString(options.threadId) ?? nonEmptyString(options.sessionId);
  const transport = options.transport;
  if (transport !== undefined) {
    let setup;
    try {
      setup = resolveResponsesTransport(transport);
    } catch {
      throw new TypeError("Worker Agent requires a Nanocodex Responses transport");
    }
    if (setup.subscription !== undefined || setup.mpp !== undefined) {
      throw new TypeError("Worker Agent does not support function-backed ChatGPT or MPP transports");
    }
    const {
      apiKey,
      hostAuth,
      hostManagedProtocol: _hostManagedProtocol,
      ...connection
    } = setup;
    if (hostAuth === true) {
      if (typeof connection.createWebSocket === "function"
        || typeof connection.WebSocketImpl === "function") {
        throw new TypeError("Worker Agent host-managed transport callbacks must live inside a custom Worker");
      }
      config.transport = { kind: "host-managed", options: connection };
    } else {
      config.transport = { kind: "openai", options: { apiKey, ...connection } };
    }
  }
  if (config.harness !== false) {
    config.harness = harnessDescriptor(config);
    if (
      workerDurability
      && stableThreadId !== undefined
      && config.durability === undefined
      && config.durabilityId === undefined
    ) config.workerDurabilityId = stableThreadId;
  }
  delete config.threadId;
  assertNoFunctions(config, "Agent options");
  assertCloneable(config, "Agent options");
  return config;
}

async function hydrateConfig(config, createDurabilityStore, hostToolProviders = []) {
  const { harness, workerDurabilityId, hostToolProviders: _hostToolProviders, ...options } = config;
  const [Transport, harnessRuntime] = await Promise.all([
    import("./Transport.mjs"),
    harness === false || harness === undefined
      ? undefined
      : import("./harness.mjs").then(({ createBrowserHarness }) => createBrowserHarness({
          ...harness,
          web: { headers: { "x-nanocodex-request": "1" } },
          images: { headers: { "x-nanocodex-request": "1" } },
          recentImages: (sessionId, count) =>
            (recentImages.get(sessionId) ?? []).slice(-count),
          rememberImage: (sessionId, imageUrl) => {
            const images = recentImages.get(sessionId) ?? [];
            images.push(imageUrl);
            if (images.length > 5) images.splice(0, images.length - 5);
            recentImages.set(sessionId, images);
          },
        })),
  ]);
  if (options.transport?.kind === "openai") {
    options.transport = Transport.openAi(options.transport.options);
  } else if (options.transport?.kind === "host-managed") {
    options.transport = Transport.hostManaged(options.transport.options);
  }
  if (
    workerDurabilityId !== undefined
    && options.durability === undefined
    && options.durabilityId === undefined
  ) {
    if (typeof workerDurabilityId !== "string" || !workerDurabilityId) {
      throw new TypeError("Worker-owned durability requires a stable thread ID");
    }
    options.durability = await createDurabilityStore();
    options.durabilityId = workerDurabilityId;
  }
  if (harnessRuntime) {
    Object.assign(options, {
      codeEvaluator: harnessRuntime.codeEvaluator,
      filesystem: harnessRuntime.filesystem,
      filesystemTools: false,
      instructions: options.instructions ?? harnessRuntime.instructions,
      tools: harnessRuntime.tools,
      executionEnvironment: options.executionEnvironment ?? harnessRuntime.executionEnvironment,
    });
  }
  if (hostToolProviders.length) {
    options[Symbol.for("nanocodex.browser.internalRuntime")] = {
      toolProviders: hostToolProviders,
    };
  }
  return options;
}

async function loadAgent(options) {
  const Agent = await import("./InlineAgent.mjs");
  if (typeof Agent.create !== "function") {
    throw new Error("browser/InlineAgent.mjs must expose create(options) for the package Worker entry");
  }
  return Agent.create(options);
}

/** @internal Starts independent Worker resources on the same critical path. */
export async function prewarmWorkerRuntime(
  harness,
  {
    module,
    loadAgent = () => import("./InlineAgent.mjs"),
    loadBrowser = () => import("../tools/browser/index.mjs"),
    loadEngine = () => import("./engine.mjs"),
  } = {},
) {
  const resources = [
    loadEngine().then(({ initializeBrowserEngine }) => initializeBrowserEngine({ module })),
    loadAgent(),
  ];
  if (harness !== false) {
    resources.push(loadBrowser().then(({ prepareBrowser }) => prepareBrowser(harness)));
  }
  await Promise.all(resources);
}

function assertNoFunctions(value, label, seen = new Set(), path = label) {
  if (typeof value === "function") throw new TypeError(`${path} cannot contain functions across the Worker boundary`);
  if (!value || typeof value !== "object" || seen.has(value)) return;
  seen.add(value);
  for (const key of Reflect.ownKeys(value)) {
    if (typeof key === "symbol") continue;
    assertNoFunctions(value[key], label, seen, `${path}.${key}`);
  }
}

function assertCloneable(value, label) {
  try { structuredClone(value); }
  catch (error) { throw new TypeError(`${label} must be structured-clone-safe`, { cause: error }); }
}

function eventBytes(event, encoded) {
  return utf8ByteLength(encoded ?? JSON.stringify(event));
}
function positiveInteger(value, label) {
  if (!Number.isSafeInteger(value) || value <= 0) throw new TypeError(`${label} must be a positive safe integer`);
  return value;
}
function randomId() { return globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random()}`; }
function nonEmptyString(value) { return typeof value === "string" && value ? value : undefined; }
function harnessDescriptor(options = {}, requireIdentity = false) {
  const threadId = nonEmptyString(options.threadId) ?? nonEmptyString(options.sessionId);
  if (requireIdentity && threadId === undefined) {
    throw new TypeError("preparing an Agent Worker requires a stable threadId or sessionId");
  }
  return {
    threadId: threadId ?? randomId(),
    origin: nonEmptyString(options.origin) ?? globalThis.location?.origin,
  };
}
function harnessKey(harness) { return `${harness?.origin ?? ""}\n${harness?.threadId ?? ""}`; }
function encodeError(error) { return { name: error?.name || "Error", message: error?.message || String(error), stack: error?.stack, ...(typeof error?.code === "string" ? { code: error.code } : {}) }; }
function decodeError(encoded = {}) { const error = encoded.name === "RangeError" ? new RangeError(encoded.message) : encoded.name === "TypeError" ? new TypeError(encoded.message) : new Error(encoded.message || "Worker Agent failed"); if (encoded.stack) error.stack = encoded.stack; if (typeof encoded.code === "string") error.code = encoded.code; return error; }
