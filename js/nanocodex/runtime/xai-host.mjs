import { freezeJson } from '../internal.mjs';

const TOOL_RESULT = Symbol.for('nanocodex.toolResult');
const MEDIA = new Set(['input_text', 'input_image', 'input_file']);
const TOOL_KEYS = new Set(['name', 'description', 'handler', 'inputSchema', 'parameters']);
/** Explicit Xai-only catalog; never discovers or installs Codex tools. */
export function resolveXaiTools(tools = []) {
  if (!Array.isArray(tools)) throw new TypeError('Xai tools must be an explicit array');
  const handlers = new Map();
  const definitions = tools.map((tool) => {
    if (!tool || typeof tool !== 'object'
      || typeof tool.name !== 'string' || !/^[a-zA-Z0-9_-]{1,64}$/.test(tool.name)
      || typeof tool.description !== 'string' || typeof tool.handler !== 'function') {
      throw new TypeError('Xai tools require name, description and handler');
    }
    if (Object.keys(tool).some(key => !TOOL_KEYS.has(key))) throw new TypeError('unsupported Xai tool field');
    if (tool.inputSchema !== undefined && tool.parameters !== undefined) throw new TypeError('Xai tool schema aliases are mutually exclusive');
    if (handlers.has(tool.name)) throw new TypeError('duplicate Xai tool name');
    const schema = tool.inputSchema ?? tool.parameters ?? { type: 'object', properties: {} };
    if (!schema || typeof schema !== 'object' || Array.isArray(schema) || schema.type !== 'object') {
      throw new TypeError('Xai tool inputSchema must be an object schema');
    }
    handlers.set(tool.name, tool.handler);
    return { name: tool.name, description: tool.description, parameters: JSON.parse(JSON.stringify(schema)) };
  });
  return { handlers, definitions: freezeJson(definitions) };
}

/** Credentials remain in this host closure, never the WASM configuration. */
const responsesFetches = new Map();
let responsesFetchInstalled = false;
const MESSAGES_HOST_HEADER = 'x-nanocodex-xai-host';
// reqwest WASM resolves the isolate fetch. Multiplex only explicit Responses host
// capabilities; never replace arbitrary networking or retain a bearer in config.
function ownResponsesFetch(fetchImpl, endpoint) {
  if (typeof fetchImpl !== 'function' || typeof endpoint !== 'string') throw new TypeError('Xai fetch requires explicit endpoint');
  endpoint = new URL(endpoint).href;
  const id = globalThis.crypto.randomUUID();
  if (!responsesFetchInstalled) {
    const nativeFetch = globalThis.fetch.bind(globalThis);
    globalThis.fetch = (input, init) => {
      const request = new Request(input, init);
      const hostId = request.headers.get(MESSAGES_HOST_HEADER);
      if (hostId === null) return nativeFetch(request);
      const host = responsesFetches.get(hostId);
      if (!host || request.url !== host.endpoint || request.method !== 'POST') throw new Error('Xai Responses host unavailable');
      request.headers.delete(MESSAGES_HOST_HEADER);
      return Promise.resolve(host.fetch(request)).then(response => {
        if (!(response instanceof Response)) throw new TypeError('Xai fetch must return a Response');
        // Host transports may construct a Response directly. reqwest's WASM
        // adapter requires its final URL even when no network fetch occurred.
        if (!response.url) Object.defineProperty(response, 'url', { value: request.url });
        return response;
      });
    };
    responsesFetchInstalled = true;
  }
  responsesFetches.set(id, { fetch: fetchImpl, endpoint });
  return { id, release() { responsesFetches.delete(id); } };
}
export function createXaiHost({ auth, tools = [], onEvent = () => {}, fetch, endpoint, subagentSessions, subagentRouting }) {
  if (!auth || typeof auth !== 'object' || Array.isArray(auth)
    || Object.keys(auth).some((key) => !['apiKey', 'headers'].includes(key))
    || (auth.headers !== undefined && typeof auth.headers !== 'function')
    || ((typeof auth.apiKey === 'string') === (typeof auth.headers === 'function'))
    || (auth.apiKey !== undefined && (typeof auth.apiKey !== 'string' || !auth.apiKey.trim()))) {
    throw new TypeError('Xai auth requires exactly one apiKey or headers callback');
  }
  let apiKey = auth.apiKey;
  let headerProvider = auth.headers;
  const { handlers, definitions } = resolveXaiTools(tools);
  const sessions = new Map();
  const children = new Map();
  let disposed = false;
  const controller = (sessionId, turnId) => {
    let turns = sessions.get(sessionId);
    if (!turns) sessions.set(sessionId, turns = new Map());
    let value = turns.get(turnId);
    if (!value) turns.set(turnId, value = new AbortController());
    return value;
  };
  const abort = (sessionId, turnId) => {
    const turns = sessions.get(sessionId);
    if (turnId !== undefined) turns?.get(turnId)?.abort();
    else for (const value of turns?.values() ?? []) value.abort();
  };
  const responsesFetch = fetch === undefined ? undefined : ownResponsesFetch(fetch, endpoint);
  const host = {
    connect() { throw new Error('Xai uses Responses HTTP only'); },
    async xaiAuthHeaders() {
      if (disposed) throw new Error('Xai authentication unavailable');
      try {
        const headers = new Headers(apiKey === undefined ? await headerProvider() : { authorization: `Bearer ${apiKey}` });
        if (![...headers].length) throw new Error();
        if (responsesFetch) headers.set(MESSAGES_HOST_HEADER, responsesFetch.id);
        return JSON.stringify(Object.fromEntries(headers));
      } catch { throw new Error('Xai authentication unavailable'); }
    },
    toolDefinitions() { return JSON.stringify(definitions); },
    toolMode() { return 'direct'; },
    emitEvent: onEvent,
    sleep(_sessionId, milliseconds) { return new Promise((resolve) => setTimeout(resolve, milliseconds)); },
    cancelCodeTurn: abort,
    cancelCode: abort,
    routeSubagent(request) {
      if (!subagentRouting) throw new Error('subagent routing is not configured');
      return subagentRouting.resolve(request);
    },
    bindSubagentRoute(request) {
      if (!subagentRouting) throw new Error('subagent routing is not configured');
      return subagentRouting.bind(request);
    },
    bindSubagentSession(sessionId, descriptor, hostContextRef) {
      descriptor = subagentSessions?.bindingDescriptor?.(sessionId, descriptor, hostContextRef) ?? descriptor;
      subagentSessions?.bind?.(sessionId, descriptor, hostContextRef);
      children.set(sessionId, { descriptor, hostContextRef });
    },
    releaseSession(sessionId) {
      abort(sessionId);
      sessions.delete(sessionId);
      const retained = children.get(sessionId);
      if (retained) subagentSessions?.release?.(sessionId, retained.hostContextRef);
      children.delete(sessionId);
    },
    releaseTurn(sessionId, turnId) { sessions.get(sessionId)?.delete(turnId); },
    executeXaiTool(name, encodedInput, sessionId, callId, model, turnId) {
      const operation = (async () => {
        const value = await host.invokeTool(name, encodedInput, sessionId, callId, model, turnId);
        const wire = wireOutput(value);
        return JSON.stringify({ output: wire.output, success: wire.success, metadata: wire.metadata, structuredResult: wire.structured_result });
      })();
      // The WASM await's drop guard owns cancellation of this exact invocation.
      // Preserve the session and other turn controllers for queued/reusable work.
      Object.defineProperty(operation, 'cancel', { value: () => abort(sessionId, turnId) });
      return operation;
    },
    async executeTool(...args) { return JSON.stringify(wireOutput(await host.invokeTool(...args))); },
    async invokeTool(name, encodedInput, sessionId, callId, model, turnId) {
      if (disposed) throw new Error('Xai tool host is disposed');
      if (!sessionId || !turnId || !callId) throw new Error('Xai tools require session, turn and call identities');
      const handler = handlers.get(name);
      if (!handler) return failed('Xai tool is unavailable');
      try {
        const value = await handler(JSON.parse(encodedInput), Object.freeze({
          sessionId, turnId: children.get(sessionId)?.hostContextRef ?? turnId, callId, parentCallId: callId, model,
          ...(children.has(sessionId) ? { subagent: children.get(sessionId).descriptor } : {}),
          signal: controller(sessionId, turnId).signal,
        }));
        return value;
      } catch {
        // Arbitrary thrown host errors may contain credentials; no stack/body crosses this boundary.
        return failed('Xai tool execution failed');
      }
    },
    dispose() {
      disposed = true;
      responsesFetch?.release();
      apiKey = undefined;
      headerProvider = undefined;
      for (const sessionId of sessions.keys()) abort(sessionId);
      for (const sessionId of [...children.keys()]) host.releaseSession(sessionId);
      sessions.clear();
      handlers.clear();
    },
  };
  return host;
}

function failed(text) {
  return { output: text, success: false, structured_result: null, metadata: null, process_trace: null };
}
function outputBody(value) {
  if (Array.isArray(value) && value.every((item) => MEDIA.has(item?.type))) return value;
  if (typeof value === 'string') return value;
  return value === undefined ? 'undefined' : JSON.stringify(value);
}
function wireOutput(value) {
  if (value?.[TOOL_RESULT]) return {
    output: outputBody(value.output), success: value.success,
    structured_result: value.structuredResult ?? null, metadata: value.metadata ?? null, process_trace: null,
  };
  // The wire contract supports an explicit result without requiring Code Mode imports.
  if (value && typeof value === 'object' && Object.hasOwn(value, 'output') && typeof value.success === 'boolean') return {
    output: outputBody(value.output), success: value.success,
    structured_result: value.structuredResult ?? value.structured_result ?? null,
    metadata: value.metadata ?? null, process_trace: value.process_trace ?? null,
  };
  return { output: outputBody(value), success: true, structured_result: value ?? null, metadata: null, process_trace: null };
}
