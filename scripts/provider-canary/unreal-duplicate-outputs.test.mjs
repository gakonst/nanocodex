import { test } from 'node:test';
import assert from 'node:assert/strict';
import { duplicateRequest, initialRequest, runCanary, RUNNING_OUTPUT } from './unreal-duplicate-outputs.mjs';

test('wire continuation is exact ordered two outputs under provider call ID; no statuses', () => {
  const call = { type: 'function_call', name: 'canary_noop', call_id: 'call_from_provider', arguments: '{}' };
  const request = duplicateRequest(call);
  assert.equal(initialRequest().store, false);
  assert.equal(request.store, false);
  assert.deepEqual(request.input.slice(2), [
    { type: 'function_call_output', call_id: call.call_id, output: RUNNING_OUTPUT },
    { type: 'function_call_output', call_id: call.call_id, output: 'canary_noop complete: ACK' },
  ]);
  assert.equal(request.input.length, 4);
  assert.equal(JSON.stringify(request).includes('previous_response_id'), false);
});

test('no synthetic fixture can establish live acceptance', async () => {
  await assert.rejects(() => runCanary({ create: async () => ({ provenance: 'fixture' }) }), /trusted live provider adapter required/);
  const adapter = { liveProvider: true, create: async () => ({ provenance: 'fixture', httpStatus: 200, provider: 'chatgpt_subscription', response: { status: 'completed', output: [] } }) };
  await assert.rejects(() => runCanary(adapter), /not attested\/completed/);
});

test('both paths require a provider call before duplicate continuation', async () => {
  const modes = [];
  const adapter = { liveProvider: true, async create({ transport, request }) {
    modes.push([transport, request]);
    const first = request.input.length === 1;
    return { provenance: 'provider', provider: 'chatgpt_subscription', httpStatus: 200, response: { status: 'completed', id: 'test', output: first ? [{ type: 'function_call', name: 'canary_noop', call_id: 'provider-id', arguments: '{}' }] : [] } };
  } };
  const outcomes = await runCanary(adapter);
  assert.deepEqual(outcomes.map(x => x.transport), ['http', 'websocket']);
  assert.equal(modes.length, 4);
  assert.deepEqual(modes.map(x => x[0]), ['http', 'http', 'websocket', 'websocket']);
  // This fixture tests sequencing only; no live-provider claim follows from the test.
});
