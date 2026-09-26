/**
 * Opt-in wire-shape probe, NOT an auth client. Only a trusted host-managed adapter may
 * connect upstream. Fixtures exercise logic; they cannot establish provider acceptance.
 */
import { randomUUID } from 'node:crypto';
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';

export const RUNNING_OUTPUT = 'Tool call is still running. Its result arrives in a later turn: continue with independent work, or end your turn to wait for it.';
const MODEL = 'gpt-6-sol';
const TOOL = { type: 'function', name: 'canary_noop', description: 'Return a constant locally; never perform side effects.', parameters: { type: 'object', properties: {}, additionalProperties: false }, strict: true, async: true };
const PREFIX = [
  { type: 'additional_tools', id: 'at_canary', role: 'developer', tools: [TOOL] },
  { id: 'msg_canary_instructions', role: 'developer', content: [{ type: 'input_text', text: 'Provider protocol acceptance probe. Call canary_noop exactly once with no arguments; after the local result, reply ACK. No external actions.' }] },
  { id: 'msg_canary_user', role: 'user', content: [{ type: 'input_text', text: 'Call canary_noop once, then answer after its result.' }] },
];
const PATHS = [
  ['openai_api', 'http'], ['chatgpt_subscription', 'http'], ['chatgpt_subscription', 'websocket'],
];

// Mirrors Responses Lite: streamed SSE body for HTTP; response.create frame for WS.
// The runtime carries tools/instructions in the input prefix, not top-level fields.
function request(transport, sessionId, input) {
  if (!['http', 'websocket'].includes(transport)) throw new TypeError('invalid canary transport');
  const turn = { session_id: sessionId, thread_id: sessionId, turn_id: `${sessionId}:0`, request_kind: 'turn', tool_namespaces_info: {} };
  return {
    ...(transport === 'websocket' ? { type: 'response.create' } : {}),
    model: MODEL, input, tool_choice: 'auto', parallel_tool_calls: false,
    reasoning: { effort: 'low', context: 'all_turns' }, store: false, stream: true,
    include: ['reasoning.encrypted_content'], prompt_cache_key: sessionId,
    text: { verbosity: 'low' }, service_tier: 'default',
    client_metadata: {
      session_id: sessionId, thread_id: sessionId,
      ...(transport === 'websocket' ? { ws_request_header_x_openai_internal_codex_responses_lite: 'true' } : {}),
      'x-codex-turn-metadata': JSON.stringify(turn),
    },
  };
}
export function initialRequest(transport = 'http', sessionId = 'canary-session') {
  return request(transport, sessionId, structuredClone(PREFIX));
}
export function duplicateRequest(call, transport = 'http', sessionId = 'canary-session') {
  if (!call || call.type !== 'function_call' || call.name !== 'canary_noop'
    || typeof call.call_id !== 'string' || !call.call_id || typeof call.arguments !== 'string') {
    throw new Error('provider did not return the requested function call');
  }
  // Server item IDs are stripped during full replay by the runtime. Retain known
  // provider call fields (not arbitrary metadata), and never invent a call ID.
  const replayCall = { type: 'function_call', name: call.name, call_id: call.call_id, arguments: call.arguments };
  if (call.status === 'completed' || call.status === 'in_progress') replayCall.status = call.status;
  return request(transport, sessionId, [...structuredClone(PREFIX), replayCall,
    { type: 'function_call_output', call_id: call.call_id, output: RUNNING_OUTPUT },
    { type: 'function_call_output', call_id: call.call_id, output: 'canary_noop complete: ACK' },
  ]);
}

// No arbitrary upstream text or IDs cross this boundary: error bodies can contain secrets.
// HTTP status belongs to the SSE request; WS upgrade is 101, not a fabricated
// per-frame HTTP 200. Terminal events and response statuses must not be rewritten.
export function assessResult(result, provider, transport) {
  const httpStatus = transport === 'http' && Number.isInteger(result?.httpStatus) ? result.httpStatus : null;
  const upgradeStatus = transport === 'websocket' && Number.isInteger(result?.upgradeStatus) ? result.upgradeStatus : null;
  const event = ['response.completed', 'response.failed', 'response.incomplete', 'error'].includes(result?.terminalEvent) ? result.terminalEvent : null;
  const responseStatus = ['completed', 'failed', 'incomplete', 'in_progress', 'queued'].includes(result?.response?.status) ? result.response.status : null;
  const base = { httpStatus, upgradeStatus, terminalEvent: event, responseStatus };
  if (result?.provenance !== 'provider' || result.provider !== provider || result.transport !== transport) return { ...base, accepted: false, category: 'unattested' };
  if (transport === 'http' ? httpStatus !== 200 : upgradeStatus !== 101) return { ...base, accepted: false, category: 'transport_rejected' };
  if (event !== 'response.completed' || responseStatus !== 'completed') return { ...base, accepted: false, category: 'not_completed' };
  if (!Array.isArray(result.response.output)) return { ...base, accepted: false, category: 'invalid_response' };
  return { ...base, accepted: true, category: 'completed' };
}
export async function runCanary(adapter) {
  if (typeof adapter?.create !== 'function' || adapter.liveProvider !== true) throw new Error('trusted live provider adapter required');
  const outcomes = [];
  for (const [provider, transport] of PATHS) {
    const sessionId = `canary-${randomUUID()}`;
    const outcome = { provider, transport, initial: null, continuation: null, accepted: false };
    outcomes.push(outcome);
    // No retries or fallback: preserve each route's rejection and test independent routes.
    async function create(wire) {
      try { return assessResult(await adapter.create({ provider, transport, request: wire }), provider, transport); }
      catch { return { accepted: false, category: 'adapter_error', httpStatus: null, upgradeStatus: null, terminalEvent: null, responseStatus: null }; }
    }
    let first;
    try { first = await adapter.create({ provider, transport, request: initialRequest(transport, sessionId) }); }
    catch { outcome.initial = { accepted: false, category: 'adapter_error', httpStatus: null, upgradeStatus: null, terminalEvent: null, responseStatus: null }; continue; }
    outcome.initial = assessResult(first, provider, transport);
    if (!outcome.initial.accepted) continue;
    const calls = first.response.output.filter(item => item?.type === 'function_call' && item.name === 'canary_noop');
    if (calls.length !== 1) { outcome.initial = { ...outcome.initial, accepted: false, category: 'missing_single_call' }; continue; }
    let continuation;
    try { continuation = duplicateRequest(calls[0], transport, sessionId); }
    catch { outcome.initial = { ...outcome.initial, accepted: false, category: 'invalid_call' }; continue; }
    outcome.continuation = await create(continuation);
    outcome.accepted = outcome.continuation.accepted;
  }
  return outcomes;
}

if (process.argv[1] && resolve(process.argv[1]) === new URL(import.meta.url).pathname) {
  try {
    if (process.argv.length !== 4 || process.argv[2] !== '--live' || process.env.NANOCODEX_UNREAL_CANARY_APPROVED !== '1') throw new Error('requires --live ADAPTER_PATH and NANOCODEX_UNREAL_CANARY_APPROVED=1');
    const adapter = (await import(pathToFileURL(resolve(process.argv[3])).href)).default;
    const results = await runCanary(adapter);
    console.log(JSON.stringify({ liveProviderAcceptance: results.every(result => result.accepted), results }));
    if (results.some(result => !result.accepted)) process.exitCode = 1;
  } catch (error) {
    // Never print arbitrary provider/adapter exceptions or response IDs.
    console.error(error instanceof Error && /^(trusted live|requires --live)/.test(error.message) ? error.message : 'provider canary failed (consult trusted adapter telemetry)');
    process.exitCode = 1;
  }
}
