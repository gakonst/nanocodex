import { createWorkerEvaluator } from './worker-evaluator.mjs';
import { freezeJson } from '../internal.mjs';
import { createCodeRuntime, toolResult } from './code-runtime.mjs';
import { createCodeEffectIdentity } from './code-effect-identity.mjs';

const TOOL_RESULT = Symbol.for('nanocodex.toolResult');
const MEDIA = new Set(['input_text', 'input_image', 'input_audio', 'encrypted_content']);
// Reserve harness-owned operations. Explicit execution capabilities share the
// exec_command/write_stdin contracts across Codex and Claude.
const TOOL_KEYS = new Set(['name', 'description', 'handler', 'inputSchema', 'parameters', 'strict', 'deferLoading', 'defer_loading', 'supportsParallelToolCalls']);
const CODEX_TOOL_NAMES = new Set([
  'exec', 'wait', 'tool_search', 'apply_patch',
  'view_image', 'update_plan', 'web__run', 'image_gen__imagegen',
]);
// Shared platform operations are installed by the owned task-tree runtime.
// Caller tools must not replace their lifecycle or authorization handlers.
const PLATFORM_SUBAGENT_NAMES = new Set([
  'spawn_agent', 'send_agent_message', 'list_agents', 'wait_agent',
  'interrupt_agent', 'close_agent', 'submit_result',
]);

/** Explicit host-owned catalog; never discovers or installs tools implicitly. */
export function resolveClaudeTools(tools = []) {
  if (!Array.isArray(tools)) throw new TypeError('Claude tools must be an explicit array');
  const handlers = new Map();
  const parallelSafe = [];
  const definitions = tools.map((tool) => {
    if (!tool || typeof tool !== 'object'
      || typeof tool.name !== 'string' || !/^[a-zA-Z0-9_-]{1,64}$/.test(tool.name)
      || typeof tool.description !== 'string' || typeof tool.handler !== 'function') {
      throw new TypeError('Claude tools require name, description and handler');
    }
    if (Object.keys(tool).some(key => !TOOL_KEYS.has(key))) throw new TypeError('unsupported Claude tool field');
    if (tool.inputSchema !== undefined && tool.parameters !== undefined) throw new TypeError('Claude tool schema aliases are mutually exclusive');
    if (tool.deferLoading !== undefined && tool.defer_loading !== undefined) throw new TypeError('Claude tool deferLoading aliases are mutually exclusive');
    for (const key of ['strict', 'deferLoading', 'defer_loading', 'supportsParallelToolCalls']) if (tool[key] !== undefined && typeof tool[key] !== 'boolean') throw new TypeError('Claude tool flags must be boolean');
    if (CODEX_TOOL_NAMES.has(tool.name)) throw new TypeError('Codex tool definitions are not accepted by the Claude catalog');
    if (PLATFORM_SUBAGENT_NAMES.has(tool.name)) throw new TypeError('Nanocodex subagent tools are installed by the shared runtime');
    if (handlers.has(tool.name)) throw new TypeError('duplicate Claude tool name');
    const schema = tool.inputSchema ?? tool.parameters ?? { type: 'object', properties: {} };
    if (!schema || typeof schema !== 'object' || Array.isArray(schema) || schema.type !== 'object') {
      throw new TypeError('Claude tool inputSchema must be an object schema');
    }
    handlers.set(tool.name, tool.handler);
    // Scheduling metadata only; never part of the model-visible definition.
    if (tool.supportsParallelToolCalls === true) parallelSafe.push(tool.name);
    return { name: tool.name, description: tool.description, input_schema: JSON.parse(JSON.stringify(schema)),
      ...(tool.strict === undefined ? {} : { strict: tool.strict }),
      ...(tool.deferLoading === undefined && tool.defer_loading === undefined ? {} : { defer_loading: tool.deferLoading ?? tool.defer_loading }),
    };
  });
  return { handlers, definitions: freezeJson(definitions), parallelSafe: Object.freeze(parallelSafe) };
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
export function createClaudeHost({ auth, tools = [], onEvent = () => {}, fetch, endpoint, subagentSessions, subagentRouting, toolMode = 'code-only', codeEvaluator, codeEffectJournal, traceTool }) {
  if (!auth || typeof auth !== 'object' || Array.isArray(auth)
    || Object.keys(auth).some((key) => !['apiKey', 'headers'].includes(key))
    || (auth.headers !== undefined && typeof auth.headers !== 'function')
    || ((typeof auth.apiKey === 'string') === (typeof auth.headers === 'function'))
    || (auth.apiKey !== undefined && (typeof auth.apiKey !== 'string' || !auth.apiKey.trim()))) {
    throw new TypeError('Claude auth requires exactly one apiKey or headers callback');
  }
  if (toolMode !== 'code-only') throw new TypeError('Claude toolMode must be code-only');
  if (codeEvaluator === undefined && typeof globalThis.Worker === 'function') codeEvaluator = createWorkerEvaluator();
  if (typeof codeEvaluator !== 'function') throw new TypeError('Claude Code Mode requires a Worker or explicit codeEvaluator');
  let apiKey = auth.apiKey;
  let headerProvider = auth.headers;
  const { handlers, definitions, parallelSafe } = resolveClaudeTools(tools);
  const sessions = new Map();
  const children = new Map();
  let disposed = false;
  const codeTurns = new Map();
  const codeTurnOrdinals = new Map();
  const effectIdentity = createCodeEffectIdentity(codeEffectJournal);
  const code = createCodeRuntime(Object.fromEntries(definitions.map(definition => [definition.name, {
    description: definition.description, parameters: definition.input_schema,
    async handler(input, context) {
      if (disposed) throw new Error('Claude tool host is disposed');
      try {
        const value = await handlers.get(definition.name)(input, Object.freeze({ ...context,
          turnId: children.get(context.sessionId)?.hostContextRef ?? context.turnId,
          ...(children.has(context.sessionId) ? { subagent: children.get(context.sessionId).descriptor } : {}),
        }));
        if (value && typeof value === 'object' && Object.hasOwn(value, 'content')) {
          const output = typeof value.content === 'string' ? value.content : JSON.stringify(value.content);
          return toolResult(output, value.structuredResult ?? value.content, { success: !value.isError,
            metadata: value.metadata ?? null, value: value.structuredResult ?? value.content });
        }
        if (value && typeof value === 'object' && Object.hasOwn(value, 'output') && typeof value.success === 'boolean') {
          const wire = wireOutput(value);
          return toolResult(wire.output, wire.structured_result, { success: wire.success, metadata: wire.metadata,
            value: wire.structured_result ?? wire.output });
        }
        return value;
      } catch (error) {
        if (error?.code === 'host_interrupted') throw error;
        // Code Mode rejects with an Error built from this value: keep the
        // handler's code so guest catch blocks can branch on it.
        const code = typeof error?.code === 'string' || typeof error?.code === 'number' ? error.code : undefined;
        return toolResult(errorText(error), null, { success: false,
          value: code === undefined ? errorText(error) : { message: errorText(error), code } });
      }
    },
  }])), { evaluate: codeEvaluator, effectJournal: codeEffectJournal,
    effectIdentity: codeEffectJournal ? effectIdentity.resolve : undefined, traceTool });
  const modelDefinitions = codeDefinitions(definitions);
  function beginCodeTurn(sessionId, turnId) {
    let turns = codeTurns.get(sessionId);
    if (!turns) codeTurns.set(sessionId, turns = new Map());
    if (!turns.has(turnId)) {
      const ordinal = (codeTurnOrdinals.get(sessionId) ?? 0) + 1;
      codeTurnOrdinals.set(sessionId, ordinal);
      turns.set(turnId, ordinal);
      code.beginTurn(sessionId);
    }
  }
  const controller = (sessionId, turnId) => {
    let turns = sessions.get(sessionId);
    if (!turns) sessions.set(sessionId, turns = new Map());
    let value = turns.get(turnId);
    if (!value) turns.set(turnId, value = new AbortController());
    return value;
  };
  const abort = (sessionId, turnId) => {
    if (code) {
      if (turnId === undefined) code.cancel(sessionId);
      else {
        const ordinal = codeTurns.get(sessionId)?.get(turnId);
        if (ordinal !== undefined) code.cancel(sessionId, ordinal);
      }
    }
    const turns = sessions.get(sessionId);
    if (turnId !== undefined) turns?.get(turnId)?.abort();
    else for (const value of turns?.values() ?? []) value.abort();
  };
  const messagesFetch = fetch === undefined ? undefined : ownMessagesFetch(fetch, endpoint);
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
    toolDefinitions() { return JSON.stringify(modelDefinitions); },
    // Code Mode exposes only exec/wait, which keep the serial default.
    parallelSafeTools() { return code ? [] : [...parallelSafe]; },
    toolMode() { return toolMode; },
    emitEvent(event, ...args) {
      // Claude's native model-call cursor is zero-based. The shared effect
      // journal uses one-based indexes; retain the native public event unchanged.
      const parsed = typeof event === 'string' ? JSON.parse(event) : event;
      effectIdentity.observe(parsed?.type === 'tool.call' && Number.isSafeInteger(parsed.payload?.model_call_index)
        ? { ...parsed, payload: { ...parsed.payload, model_call_index: parsed.payload.model_call_index + 1 } }
        : parsed);
      return onEvent(event, ...args);
    },
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
    subagentStatus(sessionId, status) {
      subagentSessions?.status?.(sessionId, status);
    },
    bindSubagentSession(sessionId, descriptor, hostContextRef) {
      descriptor = subagentSessions?.bindingDescriptor?.(sessionId, descriptor, hostContextRef) ?? descriptor;
      subagentSessions?.bind?.(sessionId, descriptor, hostContextRef);
      children.set(sessionId, { descriptor, hostContextRef });
    },
    releaseSession(sessionId, options) {
      abort(sessionId);
      sessions.delete(sessionId);
      code?.releaseSession(sessionId, options);
      codeTurns.delete(sessionId);
      codeTurnOrdinals.delete(sessionId);
      effectIdentity.release(sessionId);
      const retained = children.get(sessionId);
      if (retained) subagentSessions?.release?.(sessionId, retained.hostContextRef, options);
      children.delete(sessionId);
    },
    releaseTurn(sessionId, turnId) {
      abort(sessionId, turnId);
      sessions.get(sessionId)?.delete(turnId);
      codeTurns.get(sessionId)?.delete(turnId);
    },
    executeClaudeTool(name, encodedInput, sessionId, callId, model, turnId, localDefinitions, executeLocalTool) {
      const operation = (async () => {
        let value;
        if (disposed) throw new Error('Claude tool host is disposed');
        if (!sessionId || !turnId || !callId) throw new Error('Claude tools require session, turn and call identities');
        beginCodeTurn(sessionId, turnId);
        // Live consumers drain updates while the cell runs. The final receipt
        // covers any tail still queued when execution settles; discard that
        // tail so delivered payloads cannot stay retained for the session.
        try {
          if (name === 'exec') {
            const input = JSON.parse(encodedInput);
            value = JSON.parse(await code.executeCodeObserved(input.code, sessionId, callId, model, turnId, localDefinitions, executeLocalTool));
          } else if (name === 'wait') value = JSON.parse(await code.waitCodeObserved(encodedInput, sessionId, callId));
          else value = failed('Claude tool is unavailable in Code Mode');
        } finally { if (name === 'exec' || name === 'wait') code.discardCodeUpdates(sessionId, callId); }
        if (value && typeof value === 'object' && Object.hasOwn(value, 'content')) {
          if (typeof value.content !== 'string' && !Array.isArray(value.content)) throw new TypeError('invalid Claude native tool content');
          if (value.isError !== undefined && typeof value.isError !== 'boolean') throw new TypeError('invalid Claude tool error flag');
          return JSON.stringify({ content: value.content, isError: value.isError ?? false, metadata: value.metadata ?? null, structuredResult: value.structuredResult ?? null });
        }
        const wire = wireOutput(value);
        if (Array.isArray(value?.nested_calls)) {
          wire.metadata = { ...wire.metadata, _nanocodex_code: { calls: value.nested_calls.map(nestedEventCall),
            origin_call_id: value.cell?.origin_call_id ?? callId, running: value.cell?.running === true } };
        }
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
    // Live nested starts/results of the exec/wait observation started by
    // executeClaudeTool for this call. Resolves null once that observation
    // closes; the final _nanocodex_code receipt stays authoritative.
    async nextClaudeCodeUpdate(sessionId, callId) {
      const encoded = await code.nextCodeUpdate(sessionId, callId);
      if (typeof encoded !== 'string') return null;
      const update = JSON.parse(encoded);
      if (update?.type === 'nested_call_completed') return JSON.stringify({ ...update, call: nestedEventCall(update.call) });
      return update?.type === 'nested_call_started' ? encoded : JSON.stringify({ type: 'ignored' });
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
      } catch (error) {
        return failed(errorText(error));
      }
    },
    dispose() {
      disposed = true;
      messagesFetch?.release();
      apiKey = undefined;
      headerProvider = undefined;
      for (const sessionId of sessions.keys()) abort(sessionId);
      for (const sessionId of [...children.keys()]) host.releaseSession(sessionId, { detach: true });
      sessions.clear();
      handlers.clear();
      codeTurns.clear();
      codeTurnOrdinals.clear();
      effectIdentity.reset();
      return code?.reset();
    },
  };
  return host;
}

// Nested receipts in _nanocodex_code are event-only metadata: the Claude core
// republishes them as tool.result events that are archived and broadcast. Large
// outputs (and their duplicated structured form) never cross into WASM memory;
// the model-visible content and durable effect receipts are unchanged.
const NESTED_EVENT_RESULT_BYTES = 32 * 1024;
const NESTED_EVENT_PREVIEW_CHARS = 16 * 1024;
const utf8 = new TextEncoder();
function nestedEventCall(call) {
  if (!call || typeof call !== 'object') return call;
  const output = typeof call.output === 'string' ? call.output : JSON.stringify(call.output ?? null) ?? '';
  const structured = call.structured_result == null ? '' : JSON.stringify(call.structured_result) ?? '';
  const metadata = call.metadata == null ? '' : JSON.stringify(call.metadata) ?? '';
  const size = output.length + (structured === output ? 0 : structured.length) + metadata.length;
  if (size <= NESTED_EVENT_RESULT_BYTES) return call;
  const bytes = utf8.encode(output).byteLength + (structured === output ? 0 : utf8.encode(structured).byteLength)
    + utf8.encode(metadata).byteLength;
  const reference = call.structured_result && typeof call.structured_result === 'object' && !Array.isArray(call.structured_result)
    ? Object.fromEntries(['image_url', 'file_id'].flatMap((key) => typeof call.structured_result[key] === 'string'
      ? [[key, call.structured_result[key]]] : []))
    : {};
  return { ...call, output: (output || structured).slice(0, NESTED_EVENT_PREVIEW_CHARS),
    structured_result: Object.keys(reference).length ? reference : null,
    metadata: metadata.length <= 4096 ? call.metadata : null,
    event_truncated: true, event_original_bytes: bytes };
}

function errorText(error) {
  return error instanceof Error ? (error.message || error.name) : String(error);
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

function codeDefinitions(definitions) {
  return freezeJson([
    { name: 'exec', description: 'Run JavaScript in the configured isolated Code Mode evaluator. Call capabilities through tools and inspect ALL_TOOLS for every callable name and schema, including built-in agent tools. Await tool calls; they resolve to parsed values. Use text(...values) (values joined by spaces), image(value), store(key, value), load(key), and yield_control(). A first-line // @exec: {"yield_time_ms": 10000, "max_output_tokens": 10000} controls observation. Output has no token budget unless explicitly supplied. If a cell is running, continue it with wait. Native capabilities: ' + JSON.stringify(definitions),
      input_schema: { type: 'object', properties: { code: { type: 'string', description: 'JavaScript source to evaluate.' } }, required: ['code'], additionalProperties: false } },
    { name: 'wait', description: 'Continue a running exec cell. Use only the cell_id returned by exec.',
      input_schema: { type: 'object', properties: { cell_id: { type: 'string' }, yield_time_ms: { type: 'integer', minimum: 0 }, max_tokens: { type: 'integer', minimum: 0 }, terminate: { type: 'boolean' } }, required: ['cell_id'], additionalProperties: false } },
  ]);
}
