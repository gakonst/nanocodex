const agentStates = new WeakMap();
const turnStates = new WeakMap();
const resultStates = new WeakMap();
const resultFinalizer = typeof FinalizationRegistry === "function"
  ? new FinalizationRegistry((raw) => {
      try { raw.free(); } catch (error) { reportError(error); }
    })
  : undefined;
const hostSessions = new Map();
const cloudflareHostReservations = new WeakMap();
const activeAgentSessions = new Map();
const pendingCloudflareAgentSessions = new Map();
const cloudflareAgentSessions = new WeakSet();
const hostConnections = new Map();
const definitionHosts = new Map();
let nextHostConnection = 1;
let nextDefinitionHost = 1;
let nextAgentUid = 1;

export const CLOUDFLARE_SESSION_RESERVATION = Symbol("nanocodex.cloudflare.sessionReservation");

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
    reserveSessions: definition.reserveSessions !== false,
  });
}

export async function createAgentClient(runtime, options = {}, requestedReservation) {
  if (!runtime || typeof runtime.create !== "function") {
    throw new TypeError("createAgent requires a Nanocodex runtime");
  }
  const reserveSession = runtime.reserveSessions !== false;
  const reservation = requestedReservation ?? (
    !reserveSession || options.sessionId === undefined
      ? undefined
      : reserveAgentSession(options.sessionId)
  );
  let raw;
  try {
    raw = await runtime.create(options);
  } catch (error) {
    releaseAgentSession(reservation);
    throw error;
  }
  return createAgent(raw, runtime, reservation, reserveSession);
}

/** Internal adapter seam: observes completion of the owned Agent release path. */
export function observeAgentRelease(agent, listener) {
  if (typeof listener !== "function") throw new TypeError("Agent release observer must be a function");
  const state = knownAgentState(agent);
  if (state.released) {
    listener({ graceful: state.shutdownPromise !== undefined });
    return () => {};
  }
  state.releaseObservers.add(listener);
  return () => state.releaseObservers.delete(listener);
}

/** Creates the stable UUIDv7 identity reserved before a WASM Agent is constructed. */
export function createSessionId() {
  if (typeof globalThis.crypto?.getRandomValues !== "function") {
    throw new Error("Nanocodex Agent creation requires crypto.getRandomValues()");
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

export function prompt(agent, options) {
  const state = agentState(agent);
  const input = actionInput(options);
  const operationId = options?.id;
  const cancelOnAdmission = options?.cancelOnAdmission === true;
  const raw = typeof input === "string"
    ? state.raw.prompt(input, operationId, cancelOnAdmission)
    : state.raw.promptContent(JSON.stringify(input), operationId, cancelOnAdmission);
  return createTurn(raw, agent);
}

/** Internal live-input seam: atomically steers the active turn or starts one. */
export async function routePrompt(agent, options) {
  const state = agentState(agent);
  const input = actionInput(options);
  if (typeof input !== "string") {
    throw new TypeError("live routed input must be text");
  }
  const raw = await state.raw.routePrompt(input);
  return raw === undefined ? undefined : createTurn(raw, agent);
}

export function getTurnResult(turn) {
  const state = turnState(turn);
  if (!state.result) {
    try {
      state.result = Promise.resolve(state.raw.result()).then(createTurnResult);
    } catch (error) {
      state.result = Promise.reject(error);
    }
  }
  return state.result;
}

export function awaitTurnAcceptance(turn) {
  const state = turnState(turn);
  if (!state.acceptance) {
    try {
      state.acceptance = typeof state.raw.accepted === "function"
        ? Promise.resolve(state.raw.accepted())
        : Promise.resolve(undefined);
    } catch (error) {
      state.acceptance = Promise.reject(error);
    }
  }
  return state.acceptance;
}

export function getTurnSnapshot(result) {
  return resultState(result).snapshot();
}

export function getTurnUsage(result) {
  return resultState(result).usage();
}

/** Internal Worker seam: preserves the Rust-owned encoding until it reaches its consumer. */
export function getEncodedTurnSnapshot(result) {
  return encodedTurnResultValue(resultState(result), "snapshot");
}

/** Internal Worker seam: preserves the Rust-owned encoding until it reaches its consumer. */
export function getEncodedTurnUsage(result) {
  return encodedTurnResultValue(resultState(result), "usage");
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
    : await state.raw.forkFrom(resultState(at).raw);
  return createAgent(raw, state.runtime);
}

export async function spawn(agent) {
  const state = agentState(agent);
  return createAgent(await state.raw.spawn(), state.runtime);
}

export async function spawnSubagent(agent, options) {
  return JSON.parse(await agentState(agent).raw.spawnSubagent(JSON.stringify(options)));
}

export async function spawnSubagents(agent, options) {
  return JSON.parse(await agentState(agent).raw.spawnSubagents(JSON.stringify(options)));
}

export async function waitSubagents(agent, options) {
  return JSON.parse(await agentState(agent).raw.waitSubagents(JSON.stringify(options)));
}

export async function listSubagents(agent, options) {
  return JSON.parse(await agentState(agent).raw.listSubagents(JSON.stringify(options ?? {})));
}

export async function sendSubagentMessage(agent, options) {
  return JSON.parse(await agentState(agent).raw.sendSubagentMessage(JSON.stringify(options)));
}

export async function interruptSubagent(agent, agentId) {
  return JSON.parse(await agentState(agent).raw.interruptSubagent(JSON.stringify({ agentId })));
}

export async function closeSubagent(agent, agentId) {
  return JSON.parse(await agentState(agent).raw.closeSubagent(JSON.stringify({ agentId })));
}

export function setThinking(agent, thinking) {
  return agentState(agent).raw.setThinking(thinking);
}

export function setModel(agent, model) {
  return agentState(agent).raw.setModel(model);
}

export function setFastMode(agent, enabled) {
  return agentState(agent).raw.setFastMode(enabled);
}

export function compact(agent) {
  return agentState(agent).raw.compact();
}

export async function context(agent) {
  return parseSessionContext(await agentState(agent).raw.context());
}

export async function appendDeveloperMessage(agent, text) {
  if (typeof text !== "string" || !text.trim()) {
    throw new TypeError("non-empty string");
  }
  return parseSessionContext(await agentState(agent).raw.appendDeveloperMessage(text));
}

export async function startRealtimeConversation(agent) {
  return parseSessionContext(await agentState(agent).raw.startRealtimeConversation());
}

export async function endRealtimeConversation(agent) {
  return parseSessionContext(await agentState(agent).raw.endRealtimeConversation());
}

export async function realtimeDelegation(agent, input, transcript = []) {
  return agentState(agent).raw.realtimeDelegation(input, JSON.stringify(transcript));
}

export async function realtimeTailDelegation(agent, transcript) {
  return agentState(agent).raw.realtimeTailDelegation(JSON.stringify(transcript));
}

/** Internal browser seam over the Rust-owned voice controller. */
export function createBrowserVoice(agent, voice) {
  return agentState(agent).raw.browserVoice(voice);
}

export async function shutdown(agent) {
  const state = knownAgentState(agent);
  if (state.shutdownPromise) return state.shutdownPromise;
  if (state.disposed) throw new Error("the Nanocodex agent has been disposed");
  if (typeof state.raw.shutdown !== "function") {
    throw new Error("this Nanocodex runtime does not expose graceful shutdown");
  }
  state.disposed = true;
  state.shutdownPromise = joinAgentShutdown(state);
  return state.shutdownPromise;
}

function parseSessionContext(context) {
  return JSON.parse(context);
}

export function subscribeAgentEvents(agent, listener, options = {}, onRelease) {
  const state = agentState(agent);
  if (typeof state.runtime.subscribe !== "function") {
    throw new Error("this Nanocodex runtime does not expose agent events");
  }
  if (typeof listener !== "function") {
    throw new TypeError("watchAgentEvents requires a listener");
  }
  const unsubscribe = state.runtime.subscribe((event, encodedLength, encodedEvent, agentId) => {
    if (options.includeAllSessions || !event?.request_id || event.request_id === agent.sessionId) {
      listener(event, encodedLength, encodedEvent, agentId);
    }
  });
  let active = true;
  const subscription = {
    close(notify) {
      if (!active) return;
      active = false;
      state.subscriptions.delete(subscription);
      const errors = [];
      runCleanup(errors, () => unsubscribe?.());
      if (notify) runCleanup(errors, () => onRelease?.());
      throwCleanupErrors(errors);
    },
  };
  state.subscriptions.add(subscription);
  return () => subscription.close(false);
}

export function toWasmConfig(options = {}) {
  const apiKey = options.apiKey;
  if (typeof apiKey !== "string" || !apiKey.trim()) {
    throw new TypeError("apiKey must be a non-empty string");
  }
  const config = { api_key: apiKey };
  copy(config, "model", options.model);
  copy(config, "thinking", options.thinking);
  copy(config, "reasoning_mode", options.reasoningMode);
  copy(config, "fast_mode", options.fastMode);
  copy(config, "websocket_warmup", options.websocketWarmup);
  copy(config, "websocket_url", options.websocketUrl);
  copy(config, "api_base_url", options.apiBaseUrl);
  copy(config, "instructions", options.instructions);
  copy(config, "session_id", options.sessionId);
  copy(config, "workspace", options.workspace);
  if (options.executionEnvironment !== undefined) {
    const environment = options.executionEnvironment;
    if (!environment || typeof environment !== "object" || Array.isArray(environment)) {
      throw new TypeError("executionEnvironment must be an object");
    }
    config.execution_environment = {
      current_date: environment.currentDate,
      timezone: environment.timezone,
    };
    copy(
      config.execution_environment,
      "project_instructions",
      environment.projectInstructions,
    );
  }
  copy(config, "resume", options.resume);
  copy(config, "durability_id", options.durabilityId);
  copy(config, "durability_host_id", options.durabilityHostId);
  copy(config, "terminal_receipt_retention", options.terminalReceiptRetention);
  copy(config, "subagents", options.subagents);
  copy(config, "host_definition_id", options.hostDefinitionId);
  return config;
}

export function createEventChannel() {
  const listeners = new Set();
  const sources = new Set();
  return Object.freeze({
    emit(eventJson, encodedBytes, agentId) {
      if (!listeners.size) return;
      const event = freezeJson(typeof eventJson === "string" ? JSON.parse(eventJson) : eventJson);
      const encodedLength = Number.isSafeInteger(encodedBytes) && encodedBytes >= 0
        ? encodedBytes
        : undefined;
      for (const listener of listeners) listener(event, encodedLength, eventJson, agentId);
    },
    subscribe(listener) {
      const activate = listeners.size === 0;
      listeners.add(listener);
      if (activate) {
        for (const source of sources) source.setEventForwarding?.(true);
      }
      let active = true;
      return () => {
        if (!active) return;
        active = false;
        listeners.delete(listener);
        if (listeners.size === 0) {
          for (const source of sources) source.setEventForwarding?.(false);
        }
      };
    },
    addSource(source) {
      sources.add(source);
      if (listeners.size) source.setEventForwarding?.(true);
    },
    removeSource(source) {
      if (!sources.delete(source)) return;
      source.setEventForwarding?.(false);
    },
  });
}

export function activateHost(host) {
  if (!host || typeof host.connect !== "function") {
    throw new TypeError("a Nanocodex host must define connect()");
  }
  installHostBridge();
}

export function installHostBridge() {
  globalThis.nanocodexHost = hostBridge;
}

export function loadDurabilityRuntime() {
  return import("./runtime/durability.mjs");
}

export function reportError(error) {
  try {
    if (typeof globalThis.reportError === "function") globalThis.reportError(error);
    else globalThis.console?.error?.(error);
  } catch {}
}

export function bindHostSession(host, sessionId, reservation) {
  const existing = hostSessions.get(sessionId);
  const replacementOwner = cloudflareSessionOwner(reservation, sessionId);
  if (existing && existing !== host) {
    if (replacementOwner === undefined
      || cloudflareHostReservations.get(existing)?.ownerId !== replacementOwner) {
      throw new Error(`Nanocodex session ID is already active: ${sessionId}`);
    }
  }
  hostSessions.set(sessionId, host);
  if (replacementOwner !== undefined) {
    reservation.host = host;
  }
}

export function releaseHostSession(host, sessionId) {
  if (hostSessions.get(sessionId) !== host) return;
  hostSessions.delete(sessionId);
}

export function registerDefinitionHost(host, cloudflareReservation) {
  const id = nextDefinitionHost++;
  definitionHosts.set(id, host);
  if (cloudflareReservation !== undefined) {
    if (!cloudflareAgentSessions.has(cloudflareReservation)
      || cloudflareReservation.released) {
      definitionHosts.delete(id);
      throw new Error("Cloudflare Agent session reservation is no longer active");
    }
    cloudflareHostReservations.set(host, cloudflareReservation);
  }
  return id;
}

export function releaseDefinitionHost(id) {
  definitionHosts.delete(id);
}

const hostBridge = Object.freeze({
  async connect(endpoint, apiKey, accountId, fedramp, sessionId, threadId, turnState) {
    const host = requiredSessionHost(threadId);
    let result;
    try {
      result = JSON.parse(await host.connect(endpoint, apiKey, sessionId, {
        accountId: accountId ?? undefined,
        fedramp,
        threadId,
        turnState: turnState ?? undefined,
      }));
    } catch (error) {
      throw JSON.stringify(connectFailure(error));
    }
    const handle = nextHostConnection++;
    hostConnections.set(handle, { host, handle: result.handle });
    hostSessions.set(threadId, host);
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
  sleep(sessionId, milliseconds) {
    const host = requiredSessionHost(sessionId);
    if (typeof host.sleep !== "function") {
      throw new TypeError("the selected Nanocodex host must define sleep(milliseconds)");
    }
    return host.sleep(milliseconds);
  },
  bindSubagentSession(
    hostDefinitionId,
    rootSessionId,
    sessionId,
    contextJson,
    hostContextRef,
  ) {
    let host;
    if (contextJson === undefined) {
      contextJson = sessionId;
      sessionId = rootSessionId;
      rootSessionId = hostDefinitionId;
      host = requiredSessionHost(rootSessionId);
    } else {
      host = requiredDefinitionHost(hostDefinitionId);
    }
    if (!cloudflareHostMayBindSubagent(host)) return;
    const existing = hostSessions.get(sessionId);
    if (existing && existing !== host) {
      const ownerId = cloudflareHostReservations.get(host)?.ownerId;
      if (ownerId === undefined
        || cloudflareHostReservations.get(existing)?.ownerId !== ownerId) {
        throw new Error(`Nanocodex subagent session ID is already active: ${sessionId}`);
      }
    }
    host.bindSubagentSession(sessionId, JSON.parse(contextJson), hostContextRef);
    hostSessions.set(sessionId, host);
  },
  releaseSubagentSession(hostDefinitionId, rootSessionId, sessionId) {
    let host;
    if (sessionId !== undefined) {
      host = definitionHosts.get(hostDefinitionId);
    } else if (rootSessionId !== undefined) {
      sessionId = rootSessionId;
      host = hostSessions.get(hostDefinitionId);
    } else {
      sessionId = hostDefinitionId;
      host = hostSessions.get(sessionId);
    }
    if (!host || hostSessions.get(sessionId) !== host) return;
    host.releaseSession(sessionId);
    if (hostSessions.get(sessionId) === host) hostSessions.delete(sessionId);
  },
  executeCode(source, sessionId, callId, model) {
    return requiredSessionHost(sessionId).executeCode(source, sessionId, callId, model);
  },
  nextCodeUpdate(sessionId, callId) {
    return requiredSessionHost(sessionId).nextCodeUpdate(sessionId, callId);
  },
  executeTool(name, input, sessionId, callId, model) {
    return requiredSessionHost(sessionId).executeTool(name, input, sessionId, callId, model);
  },
  cancelCode(sessionId) {
    hostSessions.get(sessionId)?.cancelCode?.(sessionId);
  },
  readWorkspaceFile(path, sessionId) {
    return requiredSessionHost(sessionId).readWorkspaceFile(path);
  },
  async listWorkspace(path, sessionId) {
    return JSON.stringify(await requiredSessionHost(sessionId).listWorkspace(path));
  },
  writeWorkspaceFile(path, contents, sessionId) {
    return requiredSessionHost(sessionId).writeWorkspaceFile(path, contents);
  },
  removeWorkspaceFile(path, sessionId) {
    return requiredSessionHost(sessionId).removeWorkspaceFile(path);
  },
  async subscriptionLoad(subscriptionId) {
    return (await loadSubscriptionRuntime()).load(subscriptionId);
  },
  async subscriptionCompareAndSwap(subscriptionId, expectedRevision, payload) {
    return (await loadSubscriptionRuntime()).compareAndSwap(
      subscriptionId,
      expectedRevision,
      payload,
    );
  },
  async subscriptionRequest(subscriptionId, request) {
    return (await loadSubscriptionRuntime()).request(subscriptionId, request);
  },
  toolMode(definitionHostId, sessionId) {
    return requiredDefinitionHost(definitionHostId).toolMode(sessionId);
  },
  toolDefinitions(definitionHostId, sessionId) {
    // ModelRun builds its stable tool prefix inside the WASM constructor,
    // before the returned session can be adopted. The private definition host
    // keeps that lookup instance-scoped for roots and Rust-spawned children.
    return requiredDefinitionHost(definitionHostId).toolDefinitions(sessionId);
  },
  async durabilityAcquire(routeId, stateId, ownerId) {
    return (await loadDurabilityRuntime()).acquire(routeId, stateId, ownerId);
  },
  async durabilityReplace(
    routeId,
    stateId,
    ownerId,
    fence,
    expectedRevision,
    payload,
  ) {
    return (await loadDurabilityRuntime()).replace(
      routeId,
      stateId,
      ownerId,
      fence,
      expectedRevision,
      payload,
    );
  },
  emitEvent(sessionId, eventJson, encodedBytes, encodedAgentId) {
    requiredSessionHost(sessionId).emitEvent(
      eventJson,
      encodedBytes,
      parseSubagentAgentId(encodedAgentId),
    );
  },
});

/** Decodes the string ABI without changing JavaScript's numeric AgentId contract. */
export function parseSubagentAgentId(encoded) {
  if (encoded === undefined) return undefined;
  if (typeof encoded !== "string" || !/^[1-9][0-9]*$/.test(encoded)) {
    throw new TypeError("subagent agent ID must be a canonical positive decimal string");
  }
  const agentId = Number(encoded);
  if (!Number.isSafeInteger(agentId)) {
    throw new RangeError("subagent agent ID exceeds JavaScript's safe integer range");
  }
  return agentId;
}

export function loadSubscriptionRuntime() {
  return import("./runtime/chatgpt-subscription.mjs");
}

function createAgent(
  raw,
  runtime,
  requestedReservation,
  reserveSession = runtime.reserveSessions !== false,
) {
  let reservation = requestedReservation;
  if (!raw || typeof raw.prompt !== "function") {
    releaseAgentSession(reservation);
    throw new TypeError("the runtime returned an invalid Nanocodex agent handle");
  }
  try {
    if (reserveSession) {
      reservation ??= reserveAgentSession(raw.sessionId);
      adoptAgentSession(reservation, raw.sessionId);
    }
  } catch (error) {
    const errors = [error];
    runCleanup(errors, () => runtime.dispose(raw));
    runCleanup(errors, () => releaseAgentSession(reservation));
    throwCleanupErrors(errors);
  }
  const state = {
    raw,
    runtime,
    reservation,
    releaseObservers: new Set(),
    disposed: false,
    released: false,
    shutdownPromise: undefined,
    subscriptions: new Set(),
    agentId: typeof raw.agentId === "string" ? raw.agentId : raw.sessionId,
    sessionId: raw.sessionId,
    uid: `agent-${nextAgentUid++}`,
  };
  try {
    runtime.adopt?.(raw);
  } catch (error) {
    const errors = [error];
    runCleanup(errors, () => runtime.dispose(raw));
    runCleanup(errors, () => releaseAgentSession(reservation));
    throwCleanupErrors(errors);
  }
  const agent = agentView(state, {});
  try {
    return runtime.decorate ? runtime.decorate(agent, raw) : agent;
  } catch (error) {
    const errors = [error];
    runCleanup(errors, () => releaseAgentState(state));
    throwCleanupErrors(errors);
  }
}

function agentView(state, extensions) {
  let agent;
  let reservedKeys;
  const base = {
    uid: state.uid,
    key: state.runtime.key,
    name: state.runtime.name,
    type: state.runtime.type,
    get agentId() { return state.agentId; },
    get sessionId() { return state.sessionId; },
    extend(fn) {
      if (typeof fn !== "function") throw new TypeError("agent.extend requires a decorator function");
      const value = fn(agent);
      if (!value || typeof value !== "object" || Array.isArray(value)) {
        throw new TypeError("an agent decorator must return an object");
      }
      const extension = { ...value };
      for (const key of reservedKeys) delete extension[key];
      return agentView(state, deepMerge(extensions, extension));
    },
    dispose() {
      if (state.shutdownPromise) return;
      releaseAgentState(state);
    },
  };
  reservedKeys = Object.keys(base);
  agent = Object.assign(base, extensions);
  agentStates.set(agent, state);
  return agent;
}

function requiredSessionHost(sessionId) {
  const host = hostSessions.get(sessionId);
  if (!host || !cloudflareHostMayBindSubagent(host)) {
    throw new Error(`no Nanocodex host is active for session: ${sessionId}`);
  }
  return host;
}

function requiredDefinitionHost(id) {
  const host = definitionHosts.get(id);
  if (!host) throw new Error(`no Nanocodex definition host is active: ${id}`);
  return host;
}

function releaseAgentState(state) {
  if (state.released) return;
  state.disposed = true;
  state.released = true;
  const errors = [];
  for (const subscription of [...state.subscriptions]) {
    runCleanup(errors, () => subscription.close(true));
  }
  runCleanup(errors, () => state.runtime.release?.(state.raw));
  runCleanup(errors, () => state.runtime.dispose(state.raw));
  runCleanup(errors, () => releaseAgentSession(state.reservation));
  for (const observer of [...state.releaseObservers]) {
    runCleanup(errors, () => observer({ graceful: state.shutdownPromise !== undefined }));
  }
  state.releaseObservers.clear();
  throwCleanupErrors(errors);
}

/** Internal adapter seam: reserves a stable identity before starting its runtime. */
export function reserveAgentSession(sessionId) {
  if (typeof sessionId !== "string" || !sessionId) {
    throw new TypeError("sessionId must be a non-empty string");
  }
  if (activeAgentSessions.has(sessionId) || pendingCloudflareAgentSessions.has(sessionId)) {
    throw new Error(`Nanocodex session ID is already active: ${sessionId}`);
  }
  const reservation = { sessionId, adopted: false, released: false };
  activeAgentSessions.set(sessionId, reservation);
  return reservation;
}

/** Internal Cloudflare seam: prepares, but does not activate, a reconstructed DO owner. */
export function prepareCloudflareAgentSession(sessionId, ownerId) {
  if (typeof sessionId !== "string" || !sessionId) {
    throw new TypeError("sessionId must be a non-empty string");
  }
  if (typeof ownerId !== "string" || !ownerId) {
    throw new TypeError("Cloudflare Durable Object owner ID must be a non-empty string");
  }
  const existing = activeAgentSessions.get(sessionId);
  if (pendingCloudflareAgentSessions.has(sessionId)
    || (existing !== undefined
      && (!cloudflareAgentSessions.has(existing)
        || existing.ownerId !== ownerId
        || !existing.committed))) {
    throw new Error(`Nanocodex session ID is already active: ${sessionId}`);
  }
  const reservation = {
    sessionId,
    ownerId,
    adopted: false,
    committed: false,
    predecessor: undefined,
    predecessorHost: undefined,
    host: undefined,
    released: false,
  };
  cloudflareAgentSessions.add(reservation);
  pendingCloudflareAgentSessions.set(sessionId, reservation);
  return reservation;
}

/** @internal Whether this exact Cloudflare generation may publish child descriptors. */
export function mayBindCloudflareSubagentSession(reservation) {
  return cloudflareAgentSessions.has(reservation)
    && !reservation.released
    && (pendingCloudflareAgentSessions.get(reservation.sessionId) === reservation
      || activeAgentSessions.get(reservation.sessionId) === reservation);
}

/** @internal Whether this committed current generation may delete child descriptors. */
export function mayReleaseCloudflareSubagentSession(reservation) {
  return cloudflareAgentSessions.has(reservation)
    && !reservation.released
    && reservation.committed
    && activeAgentSessions.get(reservation.sessionId) === reservation;
}

function cloudflareHostMayBindSubagent(host) {
  const reservation = cloudflareHostReservations.get(host);
  return reservation === undefined || mayBindCloudflareSubagentSession(reservation);
}

/** Internal Cloudflare seam: activates a prepared owner after raw construction acquires its durable fence. */
export function activateCloudflareAgentSession(reservation) {
  if (!cloudflareAgentSessions.has(reservation) || reservation.released) {
    throw new Error("Cloudflare Agent session reservation is no longer active");
  }
  if (activeAgentSessions.get(reservation.sessionId) === reservation) return;
  if (pendingCloudflareAgentSessions.get(reservation.sessionId) !== reservation) {
    throw new Error("Cloudflare Agent session reservation is no longer pending");
  }
  const existing = activeAgentSessions.get(reservation.sessionId);
  if (existing !== undefined
      && (!cloudflareAgentSessions.has(existing)
        || existing.ownerId !== reservation.ownerId
        || !existing.committed)) {
    throw new Error(`Nanocodex session ID is already active: ${reservation.sessionId}`);
  }
  reservation.predecessor = existing;
  reservation.predecessorHost = hostSessions.get(reservation.sessionId);
  activeAgentSessions.set(reservation.sessionId, reservation);
  pendingCloudflareAgentSessions.delete(reservation.sessionId);
}

/** Internal Cloudflare seam: publishes a reconstructed owner after adapter setup succeeds. */
export function commitCloudflareAgentSession(reservation) {
  if (!cloudflareAgentSessions.has(reservation)
    || reservation.released
    || !reservation.adopted
    || activeAgentSessions.get(reservation.sessionId) !== reservation) {
    throw new Error("Cloudflare Agent session reservation is not ready to commit");
  }
  reservation.committed = true;
  reservation.predecessor = undefined;
  reservation.predecessorHost = undefined;
}

function adoptAgentSession(reservation, sessionId) {
  if (reservation.released || activeAgentSessions.get(reservation.sessionId) !== reservation) {
    throw new Error(`Nanocodex session reservation is no longer active: ${sessionId}`);
  }
  if (reservation.sessionId !== sessionId) {
    throw new Error(
      `Nanocodex runtime changed reserved session ID ${reservation.sessionId} to ${sessionId}`,
    );
  }
  if (reservation.adopted) {
    throw new Error(`Nanocodex session reservation was already adopted: ${sessionId}`);
  }
  reservation.adopted = true;
}

/** Internal adapter seam: rolls back a runtime reservation that was not adopted. */
export function releaseAgentSession(reservation) {
  if (!reservation || reservation.released) return;
  reservation.released = true;
  if (pendingCloudflareAgentSessions.get(reservation.sessionId) === reservation) {
    pendingCloudflareAgentSessions.delete(reservation.sessionId);
  }
  if (activeAgentSessions.get(reservation.sessionId) === reservation) {
    if (cloudflareAgentSessions.has(reservation)
      && !reservation.committed
      && reservation.predecessor !== undefined
      && !reservation.predecessor.released) {
      activeAgentSessions.set(reservation.sessionId, reservation.predecessor);
      if (reservation.predecessorHost !== undefined) {
        hostSessions.set(reservation.sessionId, reservation.predecessorHost);
      }
    } else {
      activeAgentSessions.delete(reservation.sessionId);
      if (hostSessions.get(reservation.sessionId) === reservation.host) {
        hostSessions.delete(reservation.sessionId);
      }
    }
  }
}

function cloudflareSessionOwner(reservation, sessionId) {
  if (!cloudflareAgentSessions.has(reservation)
    || reservation.released
    || reservation.sessionId !== sessionId
    || activeAgentSessions.get(sessionId) !== reservation) {
    return undefined;
  }
  return reservation.ownerId;
}

async function joinAgentShutdown(state) {
  await Promise.resolve();
  let shutdownFailed = false;
  let shutdownError;
  try {
    await state.raw.shutdown();
  } catch (error) {
    shutdownFailed = true;
    shutdownError = error;
  }

  let cleanupFailed = false;
  let cleanupError;
  try {
    releaseAgentState(state);
  } catch (error) {
    cleanupFailed = true;
    cleanupError = error;
  }

  if (shutdownFailed && cleanupFailed) {
    const cleanupErrors = cleanupError instanceof AggregateError
      ? cleanupError.errors
      : [cleanupError];
    throw new AggregateError(
      [shutdownError, ...cleanupErrors],
      "Nanocodex driver shutdown and resource release both failed",
    );
  }
  if (shutdownFailed) throw shutdownError;
  if (cleanupFailed) throw cleanupError;
}

function connectFailure(error) {
  if (typeof error === "string") {
    try {
      const encoded = JSON.parse(error);
      if (encoded?.kind === "handshake_rejected" || encoded?.kind === "transport") {
        return encoded;
      }
    } catch {}
  }
  const status = Number(error?.status);
  if (Number.isInteger(status) && status >= 100 && status <= 599) {
    const retryAfter = Number(error?.retryAfter);
    return {
      kind: "handshake_rejected",
      status,
      body: typeof error?.body === "string" ? error.body : errorDetail(error),
      ...(Number.isFinite(retryAfter) && retryAfter >= 0 ? { retry_after: retryAfter } : {}),
    };
  }
  return {
    kind: "transport",
    detail: errorDetail(error),
    reconnectable: true,
  };
}

function errorDetail(error) {
  return error && (error.stack || error.message) || String(error);
}

function createTurn(raw, agent) {
  if (!raw || typeof raw.result !== "function") {
    throw new TypeError("the runtime returned an invalid Nanocodex turn handle");
  }
  const state = { raw, agent, acceptance: undefined, result: undefined, disposed: false };
  const turn = {
    get agent() { return state.agent; },
    accepted: () => awaitTurnAcceptance(turn),
    result: () => getTurnResult(turn),
    steer: (input) => steer(turn, input),
    cancel: () => cancel(turn),
    dispose() {
      if (state.disposed) return;
      state.disposed = true;
      state.raw.free();
    },
  };
  turnStates.set(turn, state);
  return Object.freeze(turn);
}

function createTurnResult(raw) {
  if (
    !raw
    || typeof raw.finalMessage !== "string"
    || typeof raw.snapshot !== "function"
    || typeof raw.usage !== "function"
    || typeof raw.free !== "function"
  ) {
    raw?.free?.();
    throw new TypeError("the runtime returned an invalid Nanocodex turn result");
  }
  const state = {
    disposed: false,
    raw,
    snapshotPromise: undefined,
    usagePromise: undefined,
    snapshot() {
      if (state.disposed) return Promise.reject(new Error("the Nanocodex turn result has been disposed"));
      state.snapshotPromise ||= materializeTurnResultValue(state, "snapshot");
      return state.snapshotPromise;
    },
    usage() {
      if (state.disposed) return Promise.reject(new Error("the Nanocodex turn result has been disposed"));
      state.usagePromise ||= materializeTurnResultValue(state, "usage");
      return state.usagePromise;
    },
  };
  const result = {
    finalMessage: raw.finalMessage,
    snapshot: () => state.snapshot(),
    usage: () => state.usage(),
    dispose() {
      if (state.disposed) return;
      state.disposed = true;
      resultFinalizer?.unregister(result);
      state.raw.free();
    },
  };
  resultStates.set(result, state);
  resultFinalizer?.register(result, raw, result);
  return Object.freeze(result);
}

function materializeTurnResultValue(state, method) {
  let encoded;
  try {
    encoded = state.raw[method]();
  } catch (error) {
    return Promise.reject(error);
  }
  return Promise.resolve(encoded).then((value) => {
    if (typeof value !== "string") {
      throw new TypeError(`the runtime returned an invalid encoded turn ${method}`);
    }
    return freezeJson(JSON.parse(value));
  });
}

async function encodedTurnResultValue(state, method) {
  const encoded = await state.raw[method]();
  if (typeof encoded !== "string") {
    throw new TypeError(`the runtime returned an invalid encoded turn ${method}`);
  }
  return encoded;
}

function agentState(agent) {
  const state = knownAgentState(agent);
  if (state.disposed) throw new Error("the Nanocodex agent has been disposed");
  return state;
}

function knownAgentState(agent) {
  const state = agentStates.get(agent);
  if (!state) throw new TypeError("expected a Nanocodex agent");
  return state;
}

function turnState(turn) {
  const state = turnStates.get(turn);
  if (!state) throw new TypeError("expected a Nanocodex turn");
  if (state.disposed) throw new Error("the Nanocodex turn has been disposed");
  return state;
}

function resultState(result) {
  const state = resultStates.get(result);
  if (!state) throw new TypeError("expected a completed Nanocodex turn result");
  if (state.disposed) throw new Error("the Nanocodex turn result has been disposed");
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

function runCleanup(errors, cleanup) {
  try {
    cleanup();
  } catch (error) {
    errors.push(error);
  }
}

function throwCleanupErrors(errors) {
  if (errors.length === 1) throw errors[0];
  if (errors.length > 1) {
    throw new AggregateError(errors, "multiple Nanocodex resources failed to release");
  }
}

export function freezeJson(value) {
  if (!value || typeof value !== "object") return value;
  const pending = [value];
  while (pending.length) {
    const current = pending.pop();
    if (Object.isFrozen(current)) continue;
    for (const child of Object.values(current)) {
      if (child && typeof child === "object" && !Object.isFrozen(child)) pending.push(child);
    }
    Object.freeze(current);
  }
  return value;
}
