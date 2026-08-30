import * as ManagedAgent from "../managed/Agent.mjs";
import { registerManagedAgentAlias } from "../managed/internal.mjs";
import { reportError } from "../internal.mjs";
import { AttachmentRejectedError } from "../tools/attachment.mjs";
import {
  providerSource,
  toolRouterBrand,
  toolRouterRuntime,
  toolRuntimeLifecycle,
} from "./tool-router.mjs";
import { createTools } from "../tools/Tools.mjs";
import { createProvider as createWebMcpProvider, isProvider as isWebMcpProvider } from "../webmcp/WebMcp.mjs";

const MANAGED_AGENT_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const TRANSPORT_OPTIONS = new Set(["agent", "apiKey", "baseUrl", "fetch", "toolsTransport"]);
const CREATE_OPTIONS = new Set(["tools", "transport", "webMcp"]);
const ATTACHMENT_BACKOFF_MS = [250, 500, 1_000, 2_000, 5_000];
const managedTransports = new WeakMap();
let nextManagedAgent = 1;

/** Defines an account-authenticated durable Agent transport without exposing its credentials. */
export function createManagedTransport(options) {
  validateTransportOptions(options);
  const identity = Object.hasOwn(options.agent, "create")
    ? Object.freeze({ kind: "create" })
    : Object.freeze({ kind: "open", id: options.agent.id });
  const transport = Object.freeze({});
  const client = { ...options };
  delete client.agent;
  managedTransports.set(transport, Object.freeze({
    identity,
    client: Object.freeze(client),
  }));
  return transport;
}

export function managedTransportOptions(transport) {
  return transport && typeof transport === "object"
    ? managedTransports.get(transport)
    : undefined;
}

/** Creates the common Agent lifecycle over an account-owned durable Agent. */
export async function createManagedAgent(options) {
  validateCreateOptions(options);
  const setup = managedTransportOptions(options.transport);
  if (!setup) throw new TypeError("Agent.create requires a managed transport");
  let tools = options.tools;
  if (tools !== undefined
      && (!tools?.[toolRouterBrand] || typeof tools.attach !== "function")) {
    throw new TypeError("managed Agent tools must be created by createTools()");
  }
  tools?.[toolRuntimeLifecycle].available();
  let automaticTools = false;
  let addedWebMcpSource = false;
  let webMcpProvider;
  try {
    if (options.webMcp !== undefined && options.webMcp !== false) {
      webMcpProvider = isWebMcpProvider(options.webMcp)
        ? options.webMcp
        : await createWebMcpProvider(options.webMcp === true ? {} : options.webMcp);
      if (tools === undefined) {
        tools = await createTools({ providers: [webMcpProvider] });
        automaticTools = true;
      } else {
        const router = tools[toolRouterRuntime];
        router.addSource(providerSource(webMcpProvider.sourceId, webMcpProvider, {
          kind: webMcpProvider.kind,
          mode: webMcpProvider.mode,
          deferred: webMcpProvider.deferred,
        }));
        addedWebMcpSource = true;
        await webMcpProvider.settled?.();
      }
    }
    const managed = setup.identity.kind === "create"
      ? await ManagedAgent.create(setup.client)
      : ManagedAgent.open(setup.identity.id, setup.client);
    const managedState = await managed.state();
    if (managedState.agent_id !== managed.id
        || typeof managedState.session_id !== "string"
        || !MANAGED_AGENT_ID.test(managedState.session_id)) {
      throw new Error("managed Agent state returned inconsistent agent or session identity");
    }
    tools?.[toolRuntimeLifecycle].claim();
    return managedAgentView(managed, managedState.agent_id, managedState.session_id, tools);
  } catch (error) {
    const cleanup = [];
    if (automaticTools) cleanup.push(tools.close());
    else if (addedWebMcpSource) {
      cleanup.push((async () => {
        await tools[toolRouterRuntime].detachSource(webMcpProvider.sourceId);
        await webMcpProvider.close?.();
      })());
    } else if (webMcpProvider) cleanup.push(Promise.resolve(webMcpProvider.close?.()));
    const settled = await Promise.allSettled(cleanup);
    const failures = settled.filter((result) => result.status === "rejected").map((result) => result.reason);
    if (failures.length) {
      throw new AggregateError([error, ...failures], "managed Agent creation and WebMCP cleanup failed");
    }
    throw error;
  }
}

function managedAgentView(managed, agentId, sessionId, tools) {
  const state = {
    managed,
    tools,
    connector: undefined,
    attachmentAbort: new AbortController(),
    attachmentSupervisor: undefined,
    closed: false,
    closing: undefined,
    agentId,
    sessionId,
    turns: new Set(),
    uid: `managed-agent-${nextManagedAgent++}`,
    watchers: new Set(),
  };
  const agent = agentView(state, {});
  registerManagedAgentAlias(agent, managed);
  if (tools) {
    state.attachmentSupervisor = superviseAttachment(state).catch((error) => {
      if (!state.closed) reportError(error);
    });
  }
  return agent;
}

async function superviseAttachment(state) {
  const target = state.managed.toolsTarget();
  for (let attempt = 0; ; attempt += 1) {
    if (state.attachmentAbort.signal.aborted) return;
    const connector = state.tools.attach(target);
    state.connector = connector;
    try {
      await connector.connect();
      return;
    } catch (error) {
      connector.close();
      if (state.connector === connector) state.connector = undefined;
      if (error instanceof AttachmentRejectedError) throw error;
      if (state.attachmentAbort.signal.aborted) return;
      if (attempt === ATTACHMENT_BACKOFF_MS.length - 1) {
        reportError(new Error(
          "managed tool attachment remains unavailable; cloud tools continue while retrying",
        ));
      }
      await attachmentBackoff(
        ATTACHMENT_BACKOFF_MS[Math.min(attempt, ATTACHMENT_BACKOFF_MS.length - 1)],
        state.attachmentAbort.signal,
      );
    }
  }
}

function attachmentBackoff(milliseconds, signal) {
  if (signal.aborted) return Promise.resolve();
  return new Promise((resolve) => {
    const timer = setTimeout(done, milliseconds);
    signal.addEventListener("abort", done, { once: true });
    function done() {
      clearTimeout(timer);
      signal.removeEventListener("abort", done);
      resolve();
    }
  });
}

function agentView(state, extensions) {
  let agent;
  const base = {
    uid: state.uid,
    key: "managed-durable",
    name: "Nanocodex Managed Durable Agent",
    type: "managed",
    get agentId() { return state.agentId; },
    get sessionId() { return state.sessionId; },
    extend(decorator) {
      if (typeof decorator !== "function") throw new TypeError("agent.extend requires a decorator function");
      const value = decorator(agent);
      if (!value || typeof value !== "object" || Array.isArray(value)) {
        throw new TypeError("an agent decorator must return an object");
      }
      const extension = { ...value };
      for (const key of Object.keys(base)) delete extension[key];
      return agentView(state, mergeExtensions(extensions, extension));
    },
    dispose() {
      if (state.closed) return;
      closeManagedAgent(state);
    },
  };
  const lifecycle = {
    events: {
      watch: (options) => {
        requireOpen(state);
        const watcher = managedEventWatcher(
          state.managed,
          state.sessionId,
          options,
          () => state.watchers.delete(watcher),
        );
        state.watchers.add(watcher);
        return watcher;
      },
    },
    session: {
      shutdown: () => shutdownManagedAgent(state),
    },
    turn: {
      prompt: (options) => {
        requireOpen(state);
        const controller = new AbortController();
        const active = { controller, turn: undefined };
        const turn = state.managed.turn.prompt({
          ...promptOptions(options),
          signal: controller.signal,
        });
        active.turn = turn;
        state.turns.add(active);
        return managedTurn(agent, turn, () => state.turns.delete(active));
      },
    },
  };
  agent = Object.assign(base, mergeExtensions(lifecycle, extensions));
  return agent;
}

function managedTurn(agent, turn, settled) {
  let result;
  return Object.freeze({
    agent,
    accepted: async () => {
      await turn.accepted();
      return turn.idempotencyKey;
    },
    result: () => result ??= turn.result()
      .then((completed) => managedTurnResult(completed))
      .finally(settled),
    steer: async (options) => { await turn.steer(options); },
    cancel: async () => { await turn.cancel(); },
    dispose() {},
  });
}

function managedTurnResult(result) {
  let disposed = false;
  return Object.freeze({
    finalMessage: result.finalMessage,
    usage: () => disposed
      ? Promise.reject(new Error("the Nanocodex turn result has been disposed"))
      : Promise.resolve(result.usage),
    dispose() { disposed = true; },
  });
}

function managedEventWatcher(managed, sessionId, options = {}, onClose) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("managed Agent event options must be an object");
  }
  const unsupported = Object.keys(options).find((key) => key !== "includeAllSessions");
  if (unsupported) throw new TypeError(`managed Agent events do not accept ${unsupported}`);
  const listeners = new Set();
  const readers = new Set();
  let observation;
  let stream;
  let pumping;
  let closed = false;

  const start = () => {
    if (closed || pumping) return;
    const controller = new AbortController();
    const activeStream = managed.events.watch({ cursor: "latest", signal: controller.signal });
    observation = controller;
    stream = activeStream;
    pumping = (async () => {
      try {
        for await (const envelope of activeStream) {
          if (closed) return;
          const event = projectManagedEvent(envelope, sessionId);
          for (const listener of listeners) {
            try { listener(event); } catch (error) { reportError(error); }
          }
          for (const reader of readers) reader.push(event);
        }
        for (const reader of readers) reader.end();
      } catch (error) {
        if (closed || controller.signal.aborted) return;
        for (const reader of readers) reader.fail(error);
        reportError(error);
      } finally {
        if (stream === activeStream) {
          observation = undefined;
          stream = undefined;
          pumping = undefined;
        }
      }
    })();
  };
  const stop = () => {
    observation?.abort();
    observation = undefined;
    void stream?.return?.();
    stream = undefined;
    pumping = undefined;
  };
  const stopIfIdle = () => {
    if (!listeners.size && !readers.size) stop();
  };
  const watcher = {
    onEvent(listener) {
      if (typeof listener !== "function") throw new TypeError("events.watch.onEvent requires a listener");
      if (closed) return () => {};
      listeners.add(listener);
      start();
      return () => {
        listeners.delete(listener);
        stopIfIdle();
      };
    },
    off() {
      if (closed) return;
      closed = true;
      stop();
      listeners.clear();
      for (const reader of readers) reader.end();
      readers.clear();
      onClose();
    },
    [Symbol.asyncIterator]() {
      if (closed) return emptyIterator();
      const reader = eventReader(() => {
        readers.delete(reader);
        stopIfIdle();
      });
      readers.add(reader);
      start();
      return reader;
    },
  };
  return Object.freeze(watcher);
}

function eventReader(onClose) {
  const queue = [];
  const pending = [];
  let ended = false;
  let failure;
  const reader = {
    push(value) {
      if (ended || failure) return;
      const waiter = pending.shift();
      if (waiter) waiter.resolve({ done: false, value });
      else if (queue.length >= 4_096) reader.fail(new RangeError("managed event iterator exceeded 4096 buffered events"));
      else queue.push(value);
    },
    fail(error) {
      if (ended || failure) return;
      failure = error;
      for (const waiter of pending.splice(0)) waiter.reject(error);
      onClose();
    },
    end() {
      if (ended) return;
      ended = true;
      for (const entry of pending.splice(0)) entry.resolve({ done: true, value: undefined });
      queue.length = 0;
      onClose();
    },
    next() {
      if (queue.length) return Promise.resolve({ done: false, value: queue.shift() });
      if (failure) return Promise.reject(failure);
      if (ended) return Promise.resolve({ done: true, value: undefined });
      return new Promise((resolve, reject) => pending.push({ resolve, reject }));
    },
    return() { reader.end(); return Promise.resolve({ done: true, value: undefined }); },
    [Symbol.asyncIterator]() { return this; },
  };
  return reader;
}

function projectManagedEvent(envelope, sessionId) {
  const observed = envelope.data?.type === "event" ? envelope.data.event : undefined;
  if (observed && typeof observed === "object" && !Array.isArray(observed)
      && typeof observed.type === "string" && observed.payload
      && typeof observed.payload === "object" && !Array.isArray(observed.payload)) {
    return Object.freeze({
      ...observed,
      request_id: sessionId,
      seq: eventSequence(envelope.cursor, observed.seq),
      payload: Object.freeze({ ...observed.payload }),
    });
  }
  return Object.freeze({
    protocol_version: 1,
    request_id: sessionId,
    seq: eventSequence(envelope.cursor),
    type: `managed.${envelope.data?.type ?? envelope.type ?? "event"}`,
    payload: Object.freeze({ ...(envelope.data ?? {}) }),
  });
}

function eventSequence(cursor, fallback = 0) {
  const parsed = Number(cursor);
  return Number.isSafeInteger(parsed) && parsed >= 0
    ? parsed
    : Number.isSafeInteger(fallback) && fallback >= 0 ? fallback : 0;
}

function emptyIterator() {
  return {
    next: () => Promise.resolve({ done: true, value: undefined }),
    return: () => Promise.resolve({ done: true, value: undefined }),
    [Symbol.asyncIterator]() { return this; },
  };
}

function promptOptions(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("Agent prompt options must be an object");
  }
  const unsupported = Object.keys(options).find((key) => key !== "id" && key !== "input");
  if (unsupported) throw new TypeError(`managed Agent prompts do not accept ${unsupported}`);
  return options.id === undefined
    ? { input: options.input }
    : { idempotencyKey: options.id, input: options.input };
}

function closeManagedAgent(state) {
  state.closed = true;
  for (const watcher of [...state.watchers]) watcher.off();
  state.watchers.clear();
  state.attachmentAbort.abort();
  for (const active of state.turns) active.controller.abort(new Error("managed Agent disposed"));
  state.turns.clear();
  state.connector?.close();
  if (state.tools) void state.tools.close().catch(reportError);
}

function shutdownManagedAgent(state) {
  if (state.closing) return state.closing;
  if (state.closed) return Promise.resolve();
  state.closed = true;
  for (const watcher of [...state.watchers]) watcher.off();
  state.watchers.clear();
  state.attachmentAbort.abort();
  state.connector?.close();
  state.closing = (async () => {
    const activeTurns = [...state.turns];
    await Promise.allSettled(activeTurns.map(async (active) => {
      try { await active.turn.cancel(); }
      finally { active.controller.abort(new Error("managed Agent shut down")); }
    }));
    for (const active of activeTurns) state.turns.delete(active);
    await state.attachmentSupervisor;
    await state.tools?.close();
  })();
  return state.closing;
}

function requireOpen(state) {
  if (state.closed) throw new Error("the Nanocodex agent has been disposed");
}

function validateTransportOptions(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("managed transport options must be an object");
  }
  const unsupported = Object.keys(options).find((key) => !TRANSPORT_OPTIONS.has(key));
  if (unsupported) throw new TypeError(`managed transport does not accept ${unsupported}`);
  const identity = options.agent;
  if (!identity || typeof identity !== "object" || Array.isArray(identity)) {
    throw new TypeError("managed transport requires agent: { create: true } or agent: { id }");
  }
  const keys = Object.keys(identity);
  if (keys.length !== 1) {
    throw new TypeError("managed transport requires exactly one explicit create or existing agent identity");
  }
  if (keys[0] === "create" && identity.create === true) return;
  if (keys[0] === "id" && typeof identity.id === "string" && MANAGED_AGENT_ID.test(identity.id)) return;
  throw new TypeError("managed transport requires agent: { create: true } or agent: { id: uuidV7 }");
}

function validateCreateOptions(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("Agent.create options must be an object");
  }
  const unsupported = Object.keys(options).find((key) => !CREATE_OPTIONS.has(key));
  if (unsupported) throw new TypeError(`managed Agent.create does not accept ${unsupported}`);
}

function mergeExtensions(left, right) {
  const merged = { ...left };
  for (const [key, value] of Object.entries(right)) {
    merged[key] = value && typeof value === "object" && !Array.isArray(value)
      && merged[key] && typeof merged[key] === "object" && !Array.isArray(merged[key])
      ? mergeExtensions(merged[key], value)
      : value;
  }
  return merged;
}
