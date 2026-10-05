import { createRequestPolicyHost } from "./request-policy-host.mjs";
import { freezeJson } from '../internal.mjs';

const TOOL_RESULT = Symbol.for('nanocodex.toolResult');
const MEDIA = new Set(['input_text', 'input_image', 'input_audio', 'encrypted_content']);
// These are Codex runtime contracts, not Claude capabilities. Never reinterpret
// a namedTool() from the existing default catalog as a native Claude definition.
const TOOL_KEYS = new Set(['name', 'description', 'handler', 'inputSchema', 'parameters', 'strict', 'deferLoading', 'defer_loading']);
const CODEX_TOOL_NAMES = new Set([
  'exec', 'wait', 'tool_search', 'exec_command', 'write_stdin', 'apply_patch',
  'view_image', 'update_plan', 'web__run', 'image_gen__imagegen',
  'spawn_agent', 'send_agent_message', 'list_agents', 'wait_agent',
  'interrupt_agent', 'close_agent', 'submit_result',
]);

/** Explicit Claude-only catalog; never discovers or installs Codex tools. */
export function resolveClaudeTools(tools = []) {
  if (!Array.isArray(tools)) throw new TypeError('Claude tools must be an explicit array');
  const handlers = new Map();
  const definitions = tools.map((tool) => {
    if (!tool || typeof tool !== 'object'
      || typeof tool.name !== 'string' || !/^[a-zA-Z0-9_-]{1,64}$/.test(tool.name)
      || typeof tool.description !== 'string' || typeof tool.handler !== 'function') {
      throw new TypeError('Claude tools require name, description and handler');
    }
    if (Object.keys(tool).some(key => !TOOL_KEYS.has(key))) throw new TypeError('unsupported Claude tool field');
    if (tool.inputSchema !== undefined && tool.parameters !== undefined) throw new TypeError('Claude tool schema aliases are mutually exclusive');
    if (tool.deferLoading !== undefined && tool.defer_loading !== undefined) throw new TypeError('Claude tool deferLoading aliases are mutually exclusive');
    for (const key of ['strict', 'deferLoading', 'defer_loading']) if (tool[key] !== undefined && typeof tool[key] !== 'boolean') throw new TypeError('Claude tool flags must be boolean');
    if (CODEX_TOOL_NAMES.has(tool.name)) throw new TypeError('Codex tool definitions are not accepted by the Claude catalog');
    if (handlers.has(tool.name)) throw new TypeError('duplicate Claude tool name');
    const schema = tool.inputSchema ?? tool.parameters ?? { type: 'object', properties: {} };
    if (!schema || typeof schema !== 'object' || Array.isArray(schema) || schema.type !== 'object') {
      throw new TypeError('Claude tool inputSchema must be an object schema');
    }
    handlers.set(tool.name, tool.handler);
    return { name: tool.name, description: tool.description, input_schema: JSON.parse(JSON.stringify(schema)),
      ...(tool.strict === undefined ? {} : { strict: tool.strict }),
      ...(tool.deferLoading === undefined && tool.defer_loading === undefined ? {} : { defer_loading: tool.deferLoading ?? tool.defer_loading }),
    };
  });
  return { handlers, definitions: freezeJson(definitions) };
}

/** Credentials remain in this host closure, never the WASM configuration. */
const messagesFetches = new Map();
let messagesFetchInstalled = false;
const MESSAGES_HOST_HEADER = 'x-nanocodex-claude-host';
// reqwest WASM resolves the isolate fetch. Multiplex only explicit Messages host
// capabilities; never replace arbitrary networking or retain a bearer in config.
function ownMessagesFetch(fetchImpl, endpoint) {
  if (typeof fetchImpl !== 'function' || typeof endpoint !== 'string') throw new TypeError('Claude fetch requires explicit endpoint');
  const id = globalThis.crypto.randomUUID();
  if (!messagesFetchInstalled) {
    const nativeFetch = globalThis.fetch.bind(globalThis);
    globalThis.fetch = (input, init) => {
      const request = new Request(input, init);
      const hostId = request.headers.get(MESSAGES_HOST_HEADER);
      if (hostId === null) return nativeFetch(request);
      const host = messagesFetches.get(hostId);
      if (!host || request.url !== host.endpoint || request.method !== 'POST') throw new Error('Claude Messages host unavailable');
      request.headers.delete(MESSAGES_HOST_HEADER);
      return host.fetch(request);
    };
    messagesFetchInstalled = true;
  }
  messagesFetches.set(id, { fetch: fetchImpl, endpoint });
  return { id, release() { messagesFetches.delete(id); } };
}
export function createClaudeHost({ auth, tools = [], onEvent = () => {}, fetch, endpoint, subagentSessions, subagentRouting, requestPolicy, sessionId }) {
  if (!auth || typeof auth !== 'object' || Array.isArray(auth)
    || Object.keys(auth).some((key) => !['apiKey', 'headers'].includes(key))
    || (auth.headers !== undefined && typeof auth.headers !== 'function')
    || ((typeof auth.apiKey === 'string') === (typeof auth.headers === 'function'))
    || (auth.apiKey !== undefined && (typeof auth.apiKey !== 'string' || !auth.apiKey.trim()))) {
    throw new TypeError('Claude auth requires exactly one apiKey or headers callback');
  }
  let apiKey = auth.apiKey;
  let headerProvider = auth.headers;
  const { handlers, definitions } = resolveClaudeTools(tools);
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
  const policyHost = createRequestPolicyHost(requestPolicy);
  policyHost.bind(sessionId);
  const governedFetch = requestPolicy === undefined ? fetch
    : (input, init) => policyHost.fetch(sessionId, fetch ?? globalThis.fetch.bind(globalThis), "claude", input, init);
  const messagesFetch = governedFetch === undefined ? undefined : ownMessagesFetch(governedFetch, endpoint ?? "https://api.anthropic.com/v1/messages");
  const host = {
    connect() { throw new Error('Claude uses Messages HTTP only'); },
    async claudeAuthHeaders() {
      if (disposed) throw new Error('Claude authentication unavailable');
      try {
        const headers = new Headers(apiKey === undefined ? await headerProvider() : { 'x-api-key': apiKey });
        if (![...headers].length) throw new Error();
        if (messagesFetch) headers.set(MESSAGES_HOST_HEADER, messagesFetch.id);
        return JSON.stringify(Object.fromEntries(headers));
      } catch { throw new Error('Claude authentication unavailable'); }
    },
    toolDefinitions() { return JSON.stringify(definitions); },
    toolMode() { return 'direct'; },
    emitEvent(event, ...args) { policyHost.observe(event); return onEvent(event, ...args); },
    bindRequestPolicy: id => policyHost.bind(id),
    forkRequestPolicy: (sourceId, id, at) => policyHost.fork(sourceId, id, at),
    requestPolicyFor: id => policyHost.policy(id),
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
    executeClaudeTool(name, encodedInput, sessionId, callId, model, turnId) {
      const operation = (async () => {
        const value = await host.invokeTool(name, encodedInput, sessionId, callId, model, turnId);
        if (value && typeof value === 'object' && Object.hasOwn(value, 'content')) {
          if (typeof value.content !== 'string' && !Array.isArray(value.content)) throw new TypeError('invalid Claude native tool content');
          if (value.isError !== undefined && typeof value.isError !== 'boolean') throw new TypeError('invalid Claude tool error flag');
          return JSON.stringify({ content: value.content, isError: value.isError ?? false, metadata: value.metadata ?? null, structuredResult: value.structuredResult ?? null });
        }
        const wire = wireOutput(value);
        const content = typeof wire.output === 'string' ? wire.output : wire.output.map((item) => {
          if (item.type === 'input_text') return { type: 'text', text: item.text };
          if (item.type === 'input_image') {
            const match = /^data:(image\/[a-zA-Z0-9.+-]+);base64,(.+)$/.exec(item.image_url);
            if (match) return { type: 'image', source: { type: 'base64', media_type: match[1], data: match[2] } };
            if (/^https?:\/\//.test(item.image_url)) return { type: 'image', source: { type: 'url', url: item.image_url } };
            throw new Error('unsupported Claude image output');
          }
          // Messages has no shared input_audio/encrypted_content representation: fail closed.
          throw new Error('unsupported Claude tool media output');
        });
        return JSON.stringify({ content, isError: !wire.success, metadata: wire.metadata, structuredResult: wire.structured_result });
      })();
      // The WASM await's drop guard owns cancellation of this exact invocation.
      // Preserve the session and other turn controllers for queued/reusable work.
      Object.defineProperty(operation, 'cancel', { value: () => abort(sessionId, turnId) });
      return operation;
    },
    async executeTool(...args) { return JSON.stringify(wireOutput(await host.invokeTool(...args))); },
    async invokeTool(name, encodedInput, sessionId, callId, model, turnId) {
      if (disposed) throw new Error('Claude tool host is disposed');
      if (!sessionId || !turnId || !callId) throw new Error('Claude tools require session, turn and call identities');
      const handler = handlers.get(name);
      if (!handler) return failed('Claude tool is unavailable');
      try {
        const value = await handler(JSON.parse(encodedInput), Object.freeze({
          sessionId, turnId: children.get(sessionId)?.hostContextRef ?? turnId, callId, parentCallId: callId, model,
          ...(children.has(sessionId) ? { subagent: children.get(sessionId).descriptor } : {}),
          signal: controller(sessionId, turnId).signal,
        }));
        return value;
      } catch {
        // Arbitrary thrown host errors may contain credentials; no stack/body crosses this boundary.
        return failed('Claude tool execution failed');
      }
    },
    dispose() {
      disposed = true;
      messagesFetch?.release();
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
