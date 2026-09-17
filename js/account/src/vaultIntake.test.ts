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
