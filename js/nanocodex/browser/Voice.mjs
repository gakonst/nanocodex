import { createBrowserVoice } from "../internal.mjs";
import {
  managedBrowserVoiceTransport,
  observeManagedAgentEvents,
} from "../managed/internal.mjs";
import { createManagedBrowserVoice } from "../managed/Voice.mjs";
import { BrowserVoiceSession } from "./VoiceSession.mjs";

export { VoiceError } from "./VoiceSession.mjs";

export const voices = Object.freeze([
  "juniper", "maple", "spruce", "ember", "vale", "breeze", "arbor", "sol", "cove",
]);
export const defaultVoice = "cove";

const IDLE_SNAPSHOT = Object.freeze({
  error: undefined,
  muted: false,
  microphoneLevel: 0,
  speakerLevel: 0,
  status: "idle",
  statusText: undefined,
  transcripts: Object.freeze([]),
  voice: undefined,
});
const activeResources = new WeakMap();

/** Creates a thin browser binding over the Rust-owned Codex voice controller. */
export function create(agent, options = {}) {
  validateAgent(agent);
  validateOptions(options);
  const listeners = new Set();
  const eventListeners = new Set();
  const managed = agent.type === "managed" || agent.type === "connect";
  const managedTransport = managed ? managedBrowserVoiceTransport(agent) : undefined;
  if (agent.type === "managed" && managedTransport?.sameOrigin) {
    const browserOrigin = browserLocationOrigin();
    if (browserOrigin !== undefined && browserOrigin !== managedTransport.origin) {
      throw new TypeError("Voice.create requires a same-origin managed Agent host; use Connect for cross-origin agents");
    }
  }
  const sessionId = managed ? agent.id : agent.sessionId;
  const target = Object.freeze({ pane: "main", branchId: sessionId });
  let snapshot = IDLE_SNAPSHOT;
  let session;
  let startPromise;
  let stopPromise;
  let watcher;
  let releaseEvents;
  let destroyed = false;
  let generation = 0;
  let resource;

  function publish(next) {
    snapshot = Object.freeze({
      error: next.error,
      muted: next.muted ?? snapshot.muted,
      microphoneLevel: next.microphoneLevel ?? snapshot.microphoneLevel,
      speakerLevel: next.speakerLevel ?? snapshot.speakerLevel,
      status: next.status,
      statusText: next.statusText,
      transcripts: next.transcripts ?? snapshot.transcripts,
      voice: next.voice,
    });
    for (const listener of listeners) listener();
  }

  function emit(event) {
    for (const listener of eventListeners) listener(event);
  }

  function cleanupWatcher() {
    releaseEvents?.();
    releaseEvents = undefined;
    watcher?.off();
    watcher = undefined;
  }

  function observeAgentEvents(active) {
    if (managed) {
      releaseEvents = observeManagedAgentEvents(agent, ({ event, turnId, cursor }) => (
        active.observe({ type: "event", target, event, turnId, cursor })
      ));
      return;
    }
    watcher = agent.events.watch({ includeAllSessions: false });
    releaseEvents = watcher.onEvent((event) => active.observe({ type: "event", target, event }));
  }

  async function start(parameters = {}, attempt = 0) {
    if (destroyed) throw new Error("voice resource is destroyed");
    const selectedVoice = parameters.voice ?? options.voice ?? defaultVoice;
    if (!voices.includes(selectedVoice)) throw new TypeError(`unsupported ChatGPT voice: ${selectedVoice}`);
    if (session) return startPromise;
    if (stopPromise) await stopPromise.catch(() => {});
    if (destroyed) throw new Error("voice resource is destroyed");
    if (session) return startPromise;
    const previous = activeResources.get(agent);
    if (previous && previous !== resource) await previous.stop().catch(() => {});
    activeResources.set(agent, resource);
    const current = ++generation;
    publish({
      error: undefined,
      muted: attempt > 0 && snapshot.muted, microphoneLevel: 0, speakerLevel: 0,
      status: "connecting",
      statusText: undefined,
      transcripts: snapshot.transcripts,
      voice: selectedVoice,
    });
    emit(Object.freeze({ type: "connecting", voice: selectedVoice }));

    const core = Promise.resolve().then(() => managed
      ? createManagedBrowserVoice(agent, selectedVoice)
      : createBrowserVoice(agent, selectedVoice));
    const transport = managedTransport;
    let transcriptSequence = 0;
    const next = new BrowserVoiceSession({
      core,
      sessionId,
      voice: selectedVoice,
      settings: voiceSettings({ ...options, ...parameters, voice: selectedVoice }),
      ...(transport?.call === undefined ? {} : { call: transport.call }),
      ...(transport?.sidebandUrl === undefined ? {} : { sidebandUrl: transport.sidebandUrl }),
      ...(options.callUrl === undefined ? {} : { callUrl: options.callUrl }),
      ...(options.sidebandUrl === undefined ? {} : { sidebandUrl: options.sidebandUrl }),
      ...(options.captureMicrophone === undefined ? {} : { captureMicrophone: options.captureMicrophone }),
      ...(options.beforeAgentTurn === undefined ? {} : { beforeAgentTurn: options.beforeAgentTurn }),
      onStatus(text) {
        if (session === next && generation === current && snapshot.status !== "idle" && snapshot.status !== "error") {
          publish({ ...snapshot, statusText: text });
        }
      },
      onTranscript(speaker, text, metadata = {}) {
        if (session !== next || generation !== current) return;
        const id = `${current}:${speaker}:${metadata.id ?? `entry-${++transcriptSequence}`}`;
        const index = snapshot.transcripts.findIndex((entry) => entry.id === id);
        if (!text.trim() && index < 0) return;
        const entry = Object.freeze({ speaker, text, id, isPartial: metadata.is_partial === true });
        const transcripts = [...snapshot.transcripts];
        if (index < 0) transcripts.push(entry); else transcripts[index] = entry;
        publish({ ...snapshot, transcripts: Object.freeze(transcripts.slice(-200)) });
        emit(Object.freeze({ type: metadata.is_partial ? "transcript.delta" : "transcript", ...entry }));
      },
      onLevels({ microphone, speaker, muted }) {
        if (session === next && generation === current) publish({ ...snapshot, muted, microphoneLevel: microphone, speakerLevel: speaker });
      },
      onUndeliveredAnswer(text) {
        if (destroyed || !text.trim()) return;
        const transcripts = [...snapshot.transcripts];
        const normalized = text.trim().replace(/\s+/g, " ");
        const index = transcripts.findLastIndex((entry) => entry.speaker === "assistant"
          && !entry.recovered && entry.id?.startsWith(`${current}:`)
          && entry.text.trim().replace(/\s+/g, " ") === normalized);
        const id = index < 0 ? `${current}:recovered:${++transcriptSequence}` : transcripts[index].id;
        const entry = Object.freeze({ speaker: "assistant", text, id, isPartial: false, recovered: true });
        if (index < 0) transcripts.push(entry); else transcripts[index] = entry;
        publish({ ...snapshot, transcripts: Object.freeze(transcripts.slice(-200)) });
        emit(Object.freeze({ type: "answer.recovered", ...entry }));
      },
      onTerminated(message) {
        if (session !== next || destroyed || generation !== current) return;
        generation += 1;
        session = undefined;
        cleanupWatcher();
        const closing = next.close().catch(() => next.abort()).finally(() => {
          if (stopPromise === closing) stopPromise = undefined;
        });
        stopPromise = closing;
        if (activeResources.get(agent) === resource) activeResources.delete(agent);
        const error = new Error(message);
        publish({ ...snapshot, error, microphoneLevel: 0, speakerLevel: 0, status: "error", statusText: message });
        emit(Object.freeze({ type: "error", error }));
      },
    });
    session = next;
    next.setMuted(snapshot.muted);
    observeAgentEvents(next);
    startPromise = next.start().then(() => {
      if (destroyed || session !== next || generation !== current) return;
      publish({ ...snapshot, error: undefined, status: "active", statusText: `Voice active (${selectedVoice})` });
      emit(Object.freeze({ type: "started", voice: selectedVoice }));
    }).catch(async (cause) => {
      if (session === next) session = undefined;
      if (activeResources.get(agent) === resource) activeResources.delete(agent);
      cleanupWatcher();
      await next.close().catch(() => next.abort());
      if (destroyed || generation !== current) return;
      if (attempt === 0 && cause?.code === "peer_connection_timeout") return start(parameters, 1);
      const error = cause instanceof Error ? cause : new Error(String(cause));
      publish({ ...snapshot, error, status: "error", statusText: error.message });
      emit(Object.freeze({ type: "error", error }));
      throw error;
    }).finally(() => {
      if (generation === current) startPromise = undefined;
    });
    return startPromise;
  }

  async function stop() {
    generation += 1;
    if (stopPromise) return stopPromise;
    const active = session;
    session = undefined;
    cleanupWatcher();
    if (activeResources.get(agent) === resource) activeResources.delete(agent);
    if (!active) {
      if (snapshot.status !== "idle") publish({ ...IDLE_SNAPSHOT, transcripts: snapshot.transcripts });
      return;
    }
    if (!destroyed) {
      publish({ ...IDLE_SNAPSHOT, transcripts: Object.freeze(snapshot.transcripts.map((entry) => Object.freeze({ ...entry, isPartial: false }))) });
      emit(Object.freeze({ type: "stopped" }));
    }
    stopPromise = active.close().catch((cause) => {
      active.abort();
      if (!destroyed) {
        const error = cause instanceof Error ? cause : new Error(String(cause));
        publish({ ...IDLE_SNAPSHOT, transcripts: snapshot.transcripts, error, status: "error", statusText: error.message });
        emit(Object.freeze({ type: "error", error }));
      }
      throw cause;
    }).finally(() => { stopPromise = undefined; });
    return stopPromise;
  }

  async function destroy() {
    if (destroyed) return;
    destroyed = true;
    generation += 1;
    const active = session;
    session = undefined;
    cleanupWatcher();
    if (activeResources.get(agent) === resource) activeResources.delete(agent);
    if (active) await active.close().catch(() => active.abort());
    else if (stopPromise) await stopPromise.catch(() => {});
    listeners.clear();
    eventListeners.clear();
  }

  function command(method, ...args) {
    if (!session || snapshot.status !== "active") return Promise.reject(new Error("voice is not active"));
    return session.command(method, ...args);
  }

  resource = Object.freeze({
    cancel: async () => {
      if (!session) return false;
      if (snapshot.status === "connecting") {
        await stop();
        return true;
      }
      return session.cancel();
    },
    destroy,
    setMuted(muted) {
      if (typeof muted !== "boolean") throw new TypeError("voice muted must be a boolean");
      if (!session) throw new Error("voice is not active");
      session.setMuted(muted);
    },
    toggleMuted() { resource.setMuted(!snapshot.muted); },
    noteTypedInput: () => session?.noteTypedInput() ?? Promise.resolve(),
    speak: (text) => command("appendSpeech", text),
    appendText: (text, { role = "user" } = {}) => command("appendText", role, text),
    appendContext: (text) => command("appendContext", text),
    getSnapshot: () => snapshot,
    onEvent(listener) {
      if (typeof listener !== "function") throw new TypeError("voice event listener must be a function");
      eventListeners.add(listener);
      return () => eventListeners.delete(listener);
    },
    start,
    stop,
    subscribe(listener) {
      if (typeof listener !== "function") throw new TypeError("voice listener must be a function");
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    toggle: (parameters) => session ? stop() : start(parameters),
  });
  return resource;
}

function validateAgent(agent) {
  const local = typeof agent?.sessionId === "string";
  const managed = (agent?.type === "managed" || agent?.type === "connect") && typeof agent.id === "string"
    && typeof agent.turn?.prompt === "function";
  if (!agent || typeof agent !== "object" || (!local && !managed) || typeof agent.events?.watch !== "function") {
    throw new TypeError("Voice.create requires a Nanocodex Agent");
  }
}

function validateOptions(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("Voice.create options must be an object");
  }
  if (options.captureMicrophone !== undefined && typeof options.captureMicrophone !== "function") {
    throw new TypeError("voice captureMicrophone must be a function");
  }
  if (options.beforeAgentTurn !== undefined && typeof options.beforeAgentTurn !== "function") {
    throw new TypeError("voice beforeAgentTurn must be a function");
  }
}

function browserLocationOrigin() {
  try {
    const origin = globalThis.location?.origin;
    return typeof origin === "string" && origin !== "null" ? origin : undefined;
  } catch {
    return undefined;
  }
}

function voiceSettings(options) {
  return Object.fromEntries(["voice", "instructions", "pace", "updates", "handoffMode", "acknowledgements"]
    .filter((key) => options[key] !== undefined).map((key) => [key, options[key]]));
}
