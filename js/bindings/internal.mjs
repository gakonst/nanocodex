const AGENT_STATE = Symbol("nanocodex.agent");
const TURN_STATE = Symbol("nanocodex.turn");
const hostSessions = new Map();
const hostConnections = new Map();
let activeHost;
let nextHostConnection = 1;
let nextAgentUid = 1;

export function defineRuntime(definition) {
  if (!definition || typeof definition.create !== "function") {
    throw new TypeError("a Nanocodex runtime must define create(options)");
  }
  return Object.freeze({
    key: definition.key ?? "custom",
    name: definition.name ?? "Nanocodex Agent",
    type: definition.type ?? "custom",
    create: definition.create,
    dispose: definition.dispose || ((agent) => agent.free()),
    subscribe: definition.subscribe,
    adopt: definition.adopt,
    release: definition.release,
    decorate: definition.decorate,
  });
}

export async function createAgentClient(runtime, options = {}) {
  if (!runtime || typeof runtime.create !== "function") {
    throw new TypeError("createAgent requires a Nanocodex runtime");
  }
  return createAgent(await runtime.create(options), runtime);
}

export function prompt(agent, options) {
  const state = agentState(agent);
  const input = actionInput(options);
  const raw = typeof input === "string"
    ? state.raw.prompt(input)
    : state.raw.promptContent(JSON.stringify(input));
  return createTurn(raw, agent);
}

export function getTurnResult(turn) {
  const state = turnState(turn);
  state.result ||= Promise.resolve().then(() => state.raw.result());
  return state.result;
}

export function steer(turn, options) {
  const state = turnState(turn);
  const input = actionInput(options);
  return typeof input === "string"
    ? state.raw.steer(input)
    : state.raw.steerContent(JSON.stringify(input));
}

export function cancel(turn) {
  return turnState(turn).raw.cancel();
}

export async function fork(agent, options) {
  const state = agentState(agent);
  const at = options?.at;
  const raw = at === undefined
    ? await state.raw.fork()
    : await state.raw.forkFrom(turnState(at).raw);
  return createAgent(raw, state.runtime);
}

export async function spawn(agent) {
  const state = agentState(agent);
  return createAgent(await state.raw.spawn(), state.runtime);
}

export function setThinking(agent, thinking) {
  return agentState(agent).raw.setThinking(thinking);
}

export function setFastMode(agent, enabled) {
  return agentState(agent).raw.setFastMode(enabled);
}

export function subscribeAgentEvents(agent, listener, options = {}) {
  const state = agentState(agent);
  if (typeof state.runtime.subscribe !== "function") {
    throw new Error("this Nanocodex runtime does not expose agent events");
  }
  if (typeof listener !== "function") {
    throw new TypeError("watchAgentEvents requires a listener");
  }
  return state.runtime.subscribe((event) => {
    if (options.includeAllSessions || !event?.request_id || event.request_id === agent.sessionId) {
      listener(event);
    }
  });
}

export function toWasmConfig(options = {}) {
  const apiKey = options.apiKey;
  if (typeof apiKey !== "string" || !apiKey.trim()) {
    throw new TypeError("apiKey must be a non-empty string");
  }
  const config = { api_key: apiKey };
  copy(config, "thinking", options.thinking);
  copy(config, "reasoning_mode", options.reasoningMode);
  copy(config, "fast_mode", options.fastMode);
  copy(config, "websocket_url", options.websocketUrl);
  copy(config, "api_base_url", options.apiBaseUrl);
  copy(config, "instructions", options.instructions);
  copy(config, "session_id", options.sessionId);
  return config;
}

export function createEventChannel() {
  const listeners = new Set();
  return Object.freeze({
    emit(eventJson) {
      const event = typeof eventJson === "string" ? JSON.parse(eventJson) : eventJson;
      for (const listener of listeners) listener(event);
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  });
}

export function activateHost(host) {
  if (!host || typeof host.connect !== "function") {
    throw new TypeError("a Nanocodex host must define connect()");
  }
  activeHost = host;
  globalThis.nanocodexHost = hostBridge;
}

export function bindHostSession(host, sessionId) {
  const existing = hostSessions.get(sessionId);
  if (existing && existing !== host) {
    throw new Error(`Nanocodex session ID is already active: ${sessionId}`);
  }
  hostSessions.set(sessionId, host);
}

export function releaseHostSession(host, sessionId) {
  if (hostSessions.get(sessionId) === host) hostSessions.delete(sessionId);
}

const hostBridge = Object.freeze({
  async connect(endpoint, apiKey, sessionId) {
    const host = requiredSessionHost(sessionId);
    const result = JSON.parse(await host.connect(endpoint, apiKey, sessionId));
    const handle = nextHostConnection++;
    hostConnections.set(handle, { host, handle: result.handle });
    hostSessions.set(sessionId, host);
    return JSON.stringify({ ...result, handle });
  },
  send(handle, message) {
    const connection = hostConnections.get(handle);
    return connection
      ? connection.host.send(connection.handle, message)
      : Promise.resolve(JSON.stringify({ ok: false, reconnectable: true, error: "unknown WebSocket handle" }));
  },
  next(handle, timeoutMs) {
    const connection = hostConnections.get(handle);
    return connection
      ? connection.host.next(connection.handle, timeoutMs)
      : Promise.resolve(JSON.stringify({ kind: "closed", detail: "for an unknown WebSocket handle" }));
  },
  close(handle) {
    const connection = hostConnections.get(handle);
    if (!connection) return;
    hostConnections.delete(handle);
    connection.host.close(connection.handle);
  },
  sleep(milliseconds) {
    return new Promise((resolve) => setTimeout(resolve, milliseconds));
  },
  executeCode(source, sessionId, callId) {
    return requiredSessionHost(sessionId).executeCode(source, sessionId, callId);
  },
  toolDefinitions(sessionId) {
    // ModelRun builds its stable tool prefix inside the WASM constructor,
    // immediately before the returned session can be adopted. Runtime
    // factories activate their host directly around that synchronous step.
    return (hostSessions.get(sessionId) ?? requiredActiveHost()).toolDefinitions();
  },
  emitEvent(eventJson) {
    const event = JSON.parse(eventJson);
    requiredSessionHost(event.request_id).emitEvent(eventJson);
  },
});

function createAgent(raw, runtime) {
  if (!raw || typeof raw.prompt !== "function") {
    throw new TypeError("the runtime returned an invalid Nanocodex agent handle");
  }
  const state = {
    raw,
    runtime,
    disposed: false,
    sessionId: raw.sessionId,
    uid: `agent-${nextAgentUid++}`,
  };
  try {
    runtime.adopt?.(raw);
  } catch (error) {
    runtime.dispose(raw);
    throw error;
  }
  const agent = agentView(state, {});
  return runtime.decorate ? runtime.decorate(agent) : agent;
}

function agentView(state, extensions) {
  let agent;
  const base = {
    uid: state.uid,
    key: state.runtime.key,
    name: state.runtime.name,
    type: state.runtime.type,
    get sessionId() { return state.sessionId; },
    extend(fn) {
      if (typeof fn !== "function") throw new TypeError("agent.extend requires a decorator function");
      const value = fn(agent);
      if (!value || typeof value !== "object" || Array.isArray(value)) {
        throw new TypeError("an agent decorator must return an object");
      }
      const extension = { ...value };
      for (const key of Object.keys(base)) delete extension[key];
      return agentView(state, deepMerge(extensions, extension));
    },
    dispose() {
      if (state.disposed) return;
      state.disposed = true;
      state.runtime.release?.(state.raw);
      state.runtime.dispose(state.raw);
    },
  };
  agent = Object.assign(base, extensions);
  Object.defineProperty(agent, AGENT_STATE, { value: state });
  return agent;
}

function requiredSessionHost(sessionId) {
  const host = hostSessions.get(sessionId);
  if (!host) throw new Error(`no Nanocodex host is active for session: ${sessionId}`);
  return host;
}

function requiredActiveHost() {
  if (!activeHost) throw new Error("no Nanocodex host is active");
  return activeHost;
}

function createTurn(raw, agent) {
  if (!raw || typeof raw.result !== "function") {
    throw new TypeError("the runtime returned an invalid Nanocodex turn handle");
  }
  const state = { raw, agent, result: undefined, disposed: false };
  const turn = {
    get agent() { return state.agent; },
    result: () => getTurnResult(turn),
    steer: (input) => steer(turn, input),
    cancel: () => cancel(turn),
    dispose() {
      if (state.disposed) return;
      state.disposed = true;
      state.raw.free();
    },
  };
  Object.defineProperty(turn, TURN_STATE, { value: state });
  return Object.freeze(turn);
}

function agentState(agent) {
  const state = agent?.[AGENT_STATE];
  if (!state) throw new TypeError("expected a Nanocodex agent");
  if (state.disposed) throw new Error("the Nanocodex agent has been disposed");
  return state;
}

function turnState(turn) {
  const state = turn?.[TURN_STATE];
  if (!state) throw new TypeError("expected a Nanocodex turn");
  if (state.disposed) throw new Error("the Nanocodex turn has been disposed");
  return state;
}

function actionInput(options) {
  const input = options?.input;
  if (typeof input !== "string" && !Array.isArray(input)) {
    throw new TypeError("turn input must be a string or ordered content array");
  }
  return input;
}

function deepMerge(left, right) {
  const merged = { ...left };
  for (const [key, value] of Object.entries(right)) {
    merged[key] = isObject(merged[key]) && isObject(value)
      ? deepMerge(merged[key], value)
      : value;
  }
  return merged;
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function copy(target, key, value) {
  if (value !== undefined) target[key] = value;
}
