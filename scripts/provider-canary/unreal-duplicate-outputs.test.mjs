import { test } from 'node:test';
import assert from 'node:assert/strict';
import { assessResult, duplicateRequest, initialRequest, runCanary, RUNNING_OUTPUT } from './unreal-duplicate-outputs.mjs';

const providerCall = { type: 'function_call', name: 'canary_noop', call_id: 'call_from_provider', arguments: '{}' };
function attested(provider, transport, output = []) {
  return { provenance: 'provider', provider, transport,
    ...(transport === 'websocket' ? { upgradeStatus: 101 } : { httpStatus: 200 }),
    terminalEvent: 'response.completed', response: { status: 'completed', id: 'test', output } };
}

test('streaming HTTP and WS frames retain ordered same-ID outputs and no output statuses', () => {
  const http = duplicateRequest(providerCall, 'http', 'session-1');
  const ws = duplicateRequest(providerCall, 'websocket', 'session-1');
  assert.equal(initialRequest().stream, true);
  assert.equal(initialRequest('websocket').type, 'response.create');
  assert.equal(http.type, undefined);
  assert.equal(ws.type, 'response.create');
  assert.equal(http.tool_choice, 'auto');
  assert.equal(http.store, false);
  assert.deepEqual(http.input.slice(-3), [providerCall,
    { type: 'function_call_output', call_id: providerCall.call_id, output: RUNNING_OUTPUT },
    { type: 'function_call_output', call_id: providerCall.call_id, output: 'canary_noop complete: ACK' },
  ]);
  assert.equal(http.input.length, initialRequest().input.length + 3);
  assert.equal(JSON.stringify(http).includes('previous_response_id'), false);
  assert.equal(http.input.at(-1).status, undefined);
});

test('fixtures and mismatched statuses never count as provider acceptance', async () => {
  await assert.rejects(() => runCanary({ create: async () => ({ provenance: 'fixture' }) }), /trusted live provider adapter required/);
  const fake = { liveProvider: true, create: async ({ provider, transport }) => ({
    ...attested(provider, transport), provenance: 'fixture',
  }) };
  const outcomes = await runCanary(fake);
  assert.equal(outcomes.length, 3);
  assert(outcomes.every(outcome => !outcome.accepted && outcome.initial.category === 'unattested'));
  assert.equal(assessResult({ ...attested('openai_api', 'http'), httpStatus: 400 }, 'openai_api', 'http').category, 'transport_rejected');
  assert.equal(assessResult({ ...attested('chatgpt_subscription', 'websocket'), terminalEvent: 'response.failed' }, 'chatgpt_subscription', 'websocket').category, 'not_completed');
  assert.equal(assessResult({ ...attested('chatgpt_subscription', 'websocket'), upgradeStatus: 200 }, 'chatgpt_subscription', 'websocket').category, 'transport_rejected');
});

test('all three routes require provider-issued call before duplicate continuation', async () => {
  const calls = [];
  const adapter = { liveProvider: true, async create({ provider, transport, request }) {
    calls.push({ provider, transport, request });
    const first = request.input.length === initialRequest(transport).input.length;
    return attested(provider, transport, first ? [providerCall] : []);
  } };
  const outcomes = await runCanary(adapter);
  assert.deepEqual(outcomes.map(({ provider, transport }) => [provider, transport]), [
    ['openai_api', 'http'], ['chatgpt_subscription', 'http'], ['chatgpt_subscription', 'websocket'],
  ]);
  assert(outcomes.every(outcome => outcome.accepted));
  assert.equal(calls.length, 6);
  assert.deepEqual(calls.map(x => x.transport), ['http', 'http', 'http', 'http', 'websocket', 'websocket']);
  assert(calls.every(({ request }) => request.stream === true));
  // Structural fixtures are not upstream evidence, regardless of their tags.
});
