import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { ToolActivity } from 'nanocodex-react/agent';
import { decodeVaultIntake, vaultIntakeReceipt } from './vaultIntake.ts';
const id = 'a'.repeat(22);
const request = (value: object) => ({ name: 'tools.request_vault_intake', status: 'completed', output: JSON.stringify({ type: 'vault_intake', status: 'input_required', ...value }) }) as ToolActivity;
test('all intake kinds decode and secret-bearing outputs fail closed', () => {
  for (const kind of ['login', 'api_key', 'card', 'address', 'phone']) {
    assert.equal(decodeVaultIntake(request({ kind }))?.kind, kind);
    assert.equal(decodeVaultIntake(request({ kind, password: 'secret' })), undefined);
  }
});
test('origin authorization requires login, opaque reference and exact HTTPS origin', () => {
  const valid = { operation: 'authorize_origin', kind: 'login', vault_id: id, origin: 'https://example.com' };
  assert.equal(decodeVaultIntake(request(valid))?.operation, 'authorize_origin');
  for (const patch of [{ vault_id: 'bad' }, { kind: 'card' }, { origin: undefined }, { origin: 'http://example.com' }, { origin: 'https://example.com/' }, { origin: 'https://user:pass@example.com' }, { operation: 'unknown' }]) {
    assert.equal(decodeVaultIntake(request({ ...valid, ...patch })), undefined);
  }
});
test('receipts whitelist metadata and bind approval to the requested item and origin', () => {
  const intake = { operation: 'authorize_origin' as const, kind: 'login' as const, vault_id: id, origin: 'https://example.com' };
  const entry = { id, kind: 'login', name: 'Actual name', created_at: 1, browser_origin: intake.origin, password: 'secret', token: 'secret' };
  const receipt = JSON.parse(vaultIntakeReceipt(entry, intake));
  assert.deepEqual(receipt, { type: 'vault_intake_receipt', operation: 'authorize_origin', status: 'saved', id, kind: 'login', name: 'Actual name', browser_origin: intake.origin });
  for (const patch of [{ id: 'b'.repeat(22) }, { kind: 'card' }, { browser_origin: undefined }, { browser_origin: 'https://other.com' }]) assert.throws(() => vaultIntakeReceipt({ ...entry, ...patch }, intake));
});
test('browser verification hints are bound and secret-free', () => {
  const valid = { operation: 'browser_verification', kind: 'login', vault_id: id, origin: 'https://example.com', challenge_id: id, agent_id: 'agent_1' };
  assert.equal(decodeVaultIntake(request(valid))?.challenge_id, id);
  for (const patch of [{ challenge_id: undefined }, { agent_id: '../other' }, { code: '123456' }, { origin: undefined }, { operation: 'create' }]) assert.equal(decodeVaultIntake(request({ ...valid, ...patch })), undefined);
});
test('verification uses single direct no-store request and fixed transcript receipt', async () => {
  const { submitBrowserVerification } = await import('./vaultIntake.ts');
  const intake = decodeVaultIntake(request({ operation: 'browser_verification', kind: 'login', vault_id: id, origin: 'https://example.com', challenge_id: id, agent_id: 'agent_1' }))!;
  let calls = 0;
  const receipt = await submitBrowserVerification(intake, '123456', async (url, options) => {
    calls++;
    assert.equal(url, '/v1/agents/agent_1/browser-vault/challenge');
    assert.equal(options?.cache, 'no-store'); assert.equal(options?.redirect, 'error');
    assert.equal(options?.credentials, 'same-origin');
    assert.deepEqual(JSON.parse(options?.body as string), { challenge_id: id, code: '123456' });
    return Response.json({ type: 'browser_vault_challenge_receipt', status: 'submitted', challenge_id: id });
  });
  assert.equal(calls, 1); assert.equal(receipt.includes('123456'), false);
  await assert.rejects(submitBrowserVerification(intake, '123456', async () => Response.json({ status: 'submitted', code: '123456' })));
  calls = 0;
  await assert.rejects(submitBrowserVerification(intake, '123456', async () => { calls++; return new Response('secret body', {status: 500}); }), /Verification could not be confirmed/);
  assert.equal(calls, 1);
});

test('dedicated browser challenge tool accepts only the exact safe hint', () => {
  const hint = { type: 'browser_vault_challenge', status: 'input_required', challenge_id: id, agent_id: 'agent_1', origin: 'https://example.com', expires_at: 9999999999999 };
  const tool = { name: 'browser_vault_request_challenge', status: 'completed', output: JSON.stringify(hint) } as ToolActivity;
  assert.equal(decodeVaultIntake(tool)?.operation, 'browser_verification');
  for (const patch of [{ code: '123456' }, { expires_at: 'tomorrow' }, { origin: 'https://user:pass@example.com' }]) assert.equal(decodeVaultIntake({ ...tool, output: JSON.stringify({ ...hint, ...patch }) }), undefined);
});

test('takeover screenshots stay in direct response and only strict frames are accepted', async () => {
  const { browserTakeover } = await import('./vaultIntake.ts');
  const hint = { type: 'browser_vault_takeover', status: 'input_required', challenge_id: id, agent_id: 'agent_1', origin: 'https://example.com', expires_at: 9999999999999 };
  const intake = decodeVaultIntake({ name: 'browser_vault_request_takeover', status: 'completed', output: JSON.stringify(hint) } as ToolActivity)!;
  assert.equal(intake.operation, 'browser_takeover');
  const frame = { status: 'active', image: 'data:image/png;base64,iVBORw0KGgo=', width: 800, height: 600 };
  const result = await browserTakeover(intake, { action: 'type', text: 'private-text' }, async (url, options) => {
    assert.equal(url, '/v1/agents/agent_1/browser-vault/takeover');
    assert.equal(options?.cache, 'no-store'); assert.equal(options?.redirect, 'error');
    assert.deepEqual(JSON.parse(options?.body as string), { challenge_id: id, action: 'type', text: 'private-text' });
    return Response.json(frame);
  });
  assert.deepEqual(result, frame);
  await assert.rejects(browserTakeover(intake, { action: 'observe' }, async () => Response.json({ ...frame, image: 'https://provider.example/private' })));
  await assert.rejects(browserTakeover(intake, { action: 'observe' }, async () => Response.json({ ...frame, secret: 'no' })));
  assert.deepEqual(await browserTakeover(intake, { action: 'finish' }, async () => Response.json({ status: 'finished' })), { status: 'finished' });
});
