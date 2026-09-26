/**
 * Opt-in provider acceptance canary. No credential, URL, or subscription file handling lives here.
 * The operator supplies a trusted host-managed adapter (see README.md). A fixture adapter tests
 * request construction only; its result can never be reported as live provider acceptance.
 */
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';

export const RUNNING_OUTPUT = 'Tool call is still running. Its result arrives in a later turn: continue with independent work, or end your turn to wait for it.';
const MODEL = 'gpt-6-sol';
const TOOL = { type: 'function', name: 'canary_noop', description: 'Return a constant locally; never perform side effects.', parameters: { type: 'object', properties: {}, additionalProperties: false }, strict: true };
const MESSAGE = { role: 'user', content: 'Call canary_noop exactly once with no arguments, then answer in one word after its result.' };
const INSTRUCTIONS = 'Provider protocol acceptance probe. Never request external actions. Once the local canary result arrives, reply ACK.';

export function initialRequest() {
  return { model: MODEL, instructions: INSTRUCTIONS, input: [MESSAGE], tools: [TOOL], tool_choice: { type: 'function', name: 'canary_noop' }, parallel_tool_calls: false, store: false, stream: false, max_output_tokens: 128 };
}
export function duplicateRequest(call) {
  if (!call || call.type !== 'function_call' || call.name !== 'canary_noop' || typeof call.call_id !== 'string' || !call.call_id || typeof call.arguments !== 'string') throw new Error('provider did not return the requested function call');
  // Full replay avoids relying on provider response storage; exactly TWO output items for one ID.
  // No `status` property on either output and no client-generated call ID.
  return { model: MODEL, instructions: INSTRUCTIONS, input: [MESSAGE,
    { type: 'function_call', name: call.name, call_id: call.call_id, arguments: call.arguments },
    { type: 'function_call_output', call_id: call.call_id, output: RUNNING_OUTPUT },
    { type: 'function_call_output', call_id: call.call_id, output: 'canary_noop complete: ACK' },
  ], tools: [TOOL], tool_choice: 'none', store: false, stream: false, max_output_tokens: 128 };
}
function checkResult(result, phase, provider) {
  if (!result || result.provenance !== 'provider' || result.provider !== provider || ![200, 201].includes(result.httpStatus) || result.response?.status !== 'completed' || !Array.isArray(result.response.output)) {
    throw new Error(`${phase}: provider response not attested/completed (status ${Number.isInteger(result?.httpStatus) ? result.httpStatus : 'unknown'})`);
  }
  return result.response;
}
export async function runCanary(adapter) {
  if (typeof adapter?.create !== 'function' || adapter.liveProvider !== true) throw new Error('trusted live provider adapter required');
  const outcomes = [];
  for (const [provider, transport] of [
    ['openai_api', 'http'], ['chatgpt_subscription', 'http'], ['chatgpt_subscription', 'websocket'],
  ]) {
    const phase = `${provider}/${transport}`;
    const first = await adapter.create({ provider, transport, request: initialRequest() });
    const response = checkResult(first, `${phase} initial`, provider);
    const calls = response.output.filter(item => item.type === 'function_call' && item.name === 'canary_noop');
    if (calls.length !== 1) throw new Error(`${phase}: expected exactly one provider-origin canary_noop call`);
    const continuation = duplicateRequest(calls[0]);
    const second = await adapter.create({ provider, transport, request: continuation });
    checkResult(second, `${phase} duplicate continuation`, provider);
    outcomes.push({ provider, transport, accepted: true, providerRequestId: second.requestId ?? null, responseId: second.response?.id ?? null });
  }
  return outcomes;
}

if (process.argv[1] && resolve(process.argv[1]) === new URL(import.meta.url).pathname) {
  try {
    if (process.argv.length !== 4 || process.argv[2] !== '--live' || process.env.NANOCODEX_UNREAL_CANARY_APPROVED !== '1') throw new Error('requires --live ADAPTER_PATH and NANOCODEX_UNREAL_CANARY_APPROVED=1');
    const adapter = (await import(pathToFileURL(resolve(process.argv[3])).href)).default;
    const results = await runCanary(adapter);
    // This is only transport acceptance, not a durable runtime end-to-end proof.
    console.log(JSON.stringify({ liveProviderAcceptance: true, results }));
  } catch (error) {
    // Never print arbitrary provider errors: they may contain request bodies or authorization.
    console.error(error instanceof Error && /^(trusted live|requires --live|provider did not|openai_api\/|chatgpt_subscription\/)/.test(error.message) ? error.message : 'provider canary failed (consult trusted adapter telemetry)');
    process.exitCode = 1;
  }
}
