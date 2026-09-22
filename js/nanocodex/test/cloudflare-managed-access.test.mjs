import assert from 'node:assert/strict';
import test from 'node:test';
import { MANAGED_ACCESS_HEADER, MANAGED_ACCESS_TTL_MS, createManagedAccessClaims, signManagedAccessClaims,
  readManagedAccess, handRequestFailure, handBrokerRequest, isHandViewerUpgrade } from 'nanocodex/cloudflare/managed-access';
const env = { NANOCODEX_ACCESS_SECRET: 'fixture-secret-with-at-least-thirty-two-bytes' };
const principal = { kind: 'api_key', userId: 'owner', organizationId: 'org', teamId: 'team', authorizationEpoch: 7,
  capabilities: ['agents:read', 'tools:use'] };
const source = new Request('https://account.test/v1/account/hands/screens', { headers: { authorization: 'Bearer fixture' } });
const now = 1_000_000;
async function token(identity = principal, request = source, issued = now) {
  return signManagedAccessClaims(await createManagedAccessClaims(request, identity, issued), env);
}
function viewer(snapshot, extra = {}, url = 'https://account.test/v1/account/hands/view?surface_id=display&generation=one&frame_window=2') {
  return new Request(url, { headers: { authorization: 'Bearer fixture', upgrade: 'websocket', [MANAGED_ACCESS_HEADER]: snapshot, ...extra } });
}
test('the public shared verifier preserves the managed wire contract and exact credential binding', async () => {
  const snapshot = await token();
  assert.match(snapshot, /^ncx_access_v1\./);
  assert.equal(snapshot.includes('Bearer fixture'), false);
  assert.deepEqual(await readManagedAccess(viewer(snapshot), env, now), principal);
  for (const request of [viewer(snapshot + 'x'), viewer(snapshot, { authorization: 'Bearer changed' }),
    viewer(snapshot, { cookie: 'nanocodex_account=added' }), viewer(snapshot, { 'x-nanocodex-connect-user': 'forged' }),
    viewer(snapshot, {}, 'https://other.test/v1/account/hands/view')]) {
    assert.equal(await readManagedAccess(request, env, now), undefined);
  }
  assert.equal(await readManagedAccess(viewer(snapshot), env, now + MANAGED_ACCESS_TTL_MS), undefined);
  assert.equal(await readManagedAccess(viewer(await token(principal, source, now + 1)), env, now), undefined);
  for (const secret of [undefined, 'short', 'rotated-secret-with-at-least-thirty-two-bytes']) {
    assert.equal(await readManagedAccess(viewer(snapshot), { NANOCODEX_ACCESS_SECRET: secret }, now), undefined);
  }
});
test('session cookies retain first-cookie semantics and require the public Origin for viewers', async () => {
  const identity = { ...principal, kind: 'account_session' };
  const browser = new Request(source, { headers: { authorization: 'Bearer fixture', cookie: 'nanocodex_account=session' } });
  const snapshot = await token(identity, browser);
  const request = viewer(snapshot, { cookie: 'nanocodex_account = session', origin: 'https://account.test' });
  assert.deepEqual(await readManagedAccess(request, env, now), identity);
  assert.equal(handRequestFailure(request, identity), undefined);
  for (const origin of ['', 'https://other.test', 'https://account.test/']) {
    assert.equal(handRequestFailure(viewer(snapshot, { cookie: 'nanocodex_account=session', origin }), identity), 'forbidden_origin');
  }
  assert.equal(await readManagedAccess(viewer(snapshot, { cookie: 'nanocodex_account=changed; nanocodex_account=session' }), env, now), undefined);
});
test('shared Hand policy preserves capabilities, Connect restrictions, publisher writes and viewer scope', () => {
  const request = viewer('unused');
  assert.equal(isHandViewerUpgrade(request), true);
  assert.equal(handRequestFailure(request, principal), undefined);
  for (const identity of [{ ...principal, capabilities: ['agents:read'] },
    { ...principal, capabilities: ['tools:use'] }, { ...principal, connectGrant: { grantId: 'grant' } }]) {
    assert.equal(handRequestFailure(request, identity), 'forbidden');
  }
  for (const suffix of ['host', 'renew', 'screens', 'ice']) {
    const other = viewer('unused', {}, `https://account.test/v1/account/hands/${suffix}`);
    assert.equal(isHandViewerUpgrade(other), false);
  }
  assert.equal(isHandViewerUpgrade(new Request(request, { method: 'POST' })), false);
  assert.equal(handRequestFailure(viewer('unused', {}, 'https://account.test/v1/account/hands/host'), principal), 'forbidden');
});
test('the broker request replaces ownership assertions and removes forged publisher authority', () => {
  const request = viewer('unused', {
    'x-nanocodex-remote-vm': 'forged-vm', 'x-nanocodex-owner-id': 'forged-owner',
    'x-nanocodex-session-organization-id': 'forged-org', 'x-nanocodex-session-team-id': 'forged-team',
    'x-nanocodex-authorization-epoch': '999', 'x-nanocodex-capabilities': '["agents:write"]',
    'x-nanocodex-connect-user': 'forged', 'x-nanocodex-connect-grant-id': 'forged',
    'x-nanocodex-connect-capabilities': 'forged', 'x-nanocodex-connect-connectors': 'forged',
    'x-nanocodex-connect-connector-connections': 'forged', 'x-nanocodex-connect-mcp-ids': 'forged',
    'x-nanocodex-connect-app-tool-catalog-digest': 'forged',
  });
  const forwarded = handBrokerRequest(request, principal);
  assert.equal(forwarded.url, 'https://account-tools.internal/hands/view?surface_id=display&generation=one&frame_window=2');
  for (const [name, value] of Object.entries({ 'x-nanocodex-owner-id': 'owner', 'x-nanocodex-session-organization-id': 'org',
    'x-nanocodex-session-team-id': 'team', 'x-nanocodex-authorization-epoch': '7',
    'x-nanocodex-capabilities': JSON.stringify(principal.capabilities) })) assert.equal(forwarded.headers.get(name), value);
  assert.equal(forwarded.headers.has('x-nanocodex-remote-vm'), false);
  assert.equal([...forwarded.headers].some(([name]) => name.startsWith('x-nanocodex-connect-')), false);
});
