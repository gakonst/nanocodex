import assert from 'node:assert/strict';
import test from 'node:test';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { createServicesClient, createHostedRequest, readHostedResult, ServiceError } from 'nanocodex/services';

const vaultId = 'vault_synthetic_item_123456789';
const operationId = '11111111-1111-4111-8111-111111111111';
const numberId = '22222222-2222-4222-8222-222222222222';

// A real HTTP boundary exercises the shipped SDK; the remote account service is a synthetic fixture.
async function server(t, handler) {
  const requests = [];
  const http = createServer(async (req, res) => {
    let body = ''; for await (const chunk of req) body += chunk;
    const record = { method: req.method, url: req.url, authorization: req.headers.authorization, origin: req.headers.origin, body: body ? JSON.parse(body) : undefined };
    requests.push(record);
    try { await handler(record, res); } catch (error) { res.writeHead(500); res.end(JSON.stringify({ error: error.message })); }
  });
  http.listen(0, '127.0.0.1'); await once(http, 'listening');
  t.after(() => new Promise(resolve => { http.closeAllConnections(); http.close(resolve); }));
  return { url: `http://127.0.0.1:${http.address().port}`, requests };
}
function json(res, value, status = 200) { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(value)); }

test('public service SDK carries direct authorization over HTTP and never exposes Vault secret fields', async t => {
  const { url, requests } = await server(t, (req, res) => {
    if (req.authorization !== 'Bearer synthetic-account-key') return json(res, { error: 'unauthorized' }, 401);
    if (req.method === 'POST') return json(res, { status: 201, ok: true, body: 'must-not-escape', token: 'must-not-escape' });
    const entry = { id: vaultId, name: 'Example', kind: 'totp', issuer: 'Example', account: 'test@example.com', origin: 'https://example.com', algorithm: 'SHA1', digits: 6, period: 30, created_at: 1, seed: 'must-not-escape', code: 'must-not-escape' };
    return json(res, req.url.endsWith(vaultId) ? { entry } : { vault: [entry] });
  });
  const client = createServicesClient({ apiKey: 'synthetic-account-key', baseUrl: url });
  assert.deepEqual((await client.vault.list()).vault, [(await client.vault.get(vaultId)).entry]);
  const metadata = (await client.vault.get(vaultId)).entry;
  assert.equal(metadata.seed, undefined); assert.equal(metadata.code, undefined);
  const receipt = await client.vault.request({ vault_id: vaultId, url: 'https://example.com/login', method: 'POST', body_encoding: 'json', body: '{"code":"{{NANOCODEX_VAULT_TOTP}}"}' });
  assert.deepEqual(receipt, { status: 201, ok: true });
  assert.equal(requests.at(-1).url, '/v1/services/vault/request');
  assert.equal(requests.at(-1).body.vault_id, vaultId);
  await assert.rejects(createServicesClient({ apiKey: 'invalid', baseUrl: url }).vault.list(), e => e instanceof ServiceError && e.status === 401 && e.code === 'unauthorized');
  t.diagnostic(JSON.stringify({ transport: 'node:http + native fetch', paths: requests.map(r => `${r.method} ${r.url}`), vaultSecretsExported: false }));
});

test('Connect scoped phone HTTP journey preserves operation UUIDs, pagination and unknown outcomes without retries', async t => {
  const request = { operation_id: operationId, approval_request_id: numberId, kind: 'purchase', status: 'pending_approval', phone_number: '+12025550101', created_at: '2026-10-05T00:00:00Z' };
  const { url, requests } = await server(t, (req, res) => {
    if (!req.url.startsWith('/v1/grants/grant_123/services/') || req.authorization !== 'Bearer synthetic-grant-token' || req.origin !== 'https://app.example') return json(res, { error: 'forbidden' }, 403);
    if (req.method === 'DELETE') return json(res, { error: 'phone_operation_outcome_unknown' }, 502);
    if (req.url.includes('/requests/') || req.method === 'POST') return json(res, { request }, req.method === 'POST' ? 202 : 200);
    if (req.url.includes('/messages')) return json(res, { messages: [{ id: 'sms_1', body: 'Synthetic inbox message' }], next_cursor: 'next+page=' });
    return json(res, { numbers: [{ id: numberId, phone_number: '+12025550101', country: 'US' }] });
  });
  const connect = { appOrigin: 'https://app.example', fetch(path, init) { return fetch(new URL(path, url), { ...init, headers: { ...init.headers, authorization: 'Bearer synthetic-grant-token' } }); } };
  const client = createServicesClient({ connect, grantId: 'grant_123' });
  await client.phone.available({ country: 'US', area_code: '202' });
  await client.phone.list();
  const intent = (await client.phone.provision({ operation_id: operationId, phone_number: '+12025550101', country: 'US' })).request;
  assert.equal(intent.status, 'pending_approval');
  assert.equal(intent.operation_id, operationId);
  assert.equal(intent.approval_request_id, numberId);
  const approval = createHostedRequest({ service: 'phone', operationId: intent.approval_request_id ?? intent.operation_id, appOrigin: connect.appOrigin });
  assert.equal(new URL(approval.url).searchParams.get('operation_id'), numberId);
  assert.equal((await client.phone.requests.get(operationId)).request.operation_id, operationId);
  const sms = await client.phone.messages(numberId, { cursor: 'next+page=', limit: 10 });
  assert.equal(sms.next_cursor, 'next+page=');
  assert.match(requests.at(-1).url, /cursor=next%2Bpage%3D&limit=10$/);
  await assert.rejects(client.phone.release(numberId, { operation_id: operationId }), e => e.outcomeUnknown && e.status === 502);
  assert.equal(requests.filter(r => r.method === 'DELETE').length, 1);
  assert.equal(requests.at(-1).body.operation_id, operationId);
  assert.throws(() => client.phone.provision({ phone_number: '+12025550101', country: 'US' }), /operation_id/);
  t.diagnostic(JSON.stringify({ paths: requests.map(r => `${r.method} ${r.url}`), writes: requests.filter(r => r.method !== 'GET').length, releaseDispatches: 1 }));
});

test('a disconnected HTTP mutation reports ambiguity once and never follows redirects', async t => {
  let destinations = 0;
  const { url, requests } = await server(t, (req, res) => {
    if (req.url.endsWith('/destination')) { destinations++; return json(res, { status: 200, ok: true }); }
    if (req.body.url.endsWith('/drop')) return res.destroy();
    res.writeHead(307, { location: '/destination' }); res.end();
  });
  const client = createServicesClient({ apiKey: 'synthetic-key', baseUrl: url });
  for (const path of ['drop', 'redirect']) await assert.rejects(client.vault.request({ vault_id: vaultId, url: `https://example.com/${path}` }), e => e.outcomeUnknown);
  assert.equal(requests.length, 2); assert.equal(destinations, 0);
});

test('hosted enrollment validates source, exact origin, state and returns metadata only', () => {
  const request = createHostedRequest({ appOrigin: 'https://app.example', state: operationId });
  const source = {};
  const data = { type: 'nanocodex:service-enrollment', service: 'vault', state: operationId, vault_id: vaultId, kind: 'totp', name: 'Example', origin: 'https://example.com', seed: 'must-not-escape', code: 'must-not-escape' };
  const event = { origin: request.origin, source, data };
  assert.equal(new URL(request.url).searchParams.get('enrollment_origin'), 'https://app.example');
  for (const bad of [{ ...event, origin: 'https://attacker.example' }, { ...event, source: {} }, { ...event, data: { ...data, state: 'wrong' } }]) assert.equal(readHostedResult(bad, request, source), undefined);
  const result = readHostedResult(event, request, source);
  assert.equal(result.vault_id, vaultId); assert.equal(result.seed, undefined); assert.equal(result.code, undefined);
});

test('Connect grants standalone services over HTTP without an agent and rejects substituted scopes', async t => {
  const { Client, Dialog, Transport } = await import('nanocodex/connect');
  const services = { vault: { ids: [vaultId], origins: ['https://example.com'], request: true } };
  let walletRequest;
  let substitute = false;
  const { url, requests } = await server(t, (req, res) => {
    if (req.url === '/v1/connections') return json(res, {
      account_address: '0x8ba1f109551bd432803012645ac136ddd64dba72', authorization_mode: 'hosted', grant_token: 'synthetic-session',
      grant: { id: '0x1234', permission: 'services.use', status: 'active', expires_at: Math.floor(Date.now()/1000)+3600,
        capabilities: [], mcp_connections: [], services: substitute ? { vault: { ...services.vault, ids: ['substituted_id'] } } : services },
    });
    if (req.url === '/v1/grants/0x1234/services/vault' && req.authorization === 'Bearer synthetic-session') return json(res, { vault: [] });
    return json(res, { error: 'unexpected_path' }, 404);
  });
  const client = Client.create({ appId: 'services-test', appOrigin: 'https://app.example', dialog: Dialog.memory(), session: false,
    auth: { resources: ['urn:nanocodex:agent:run', 'urn:nanocodex:mpp:stale'] },
    transport: Transport.http(url), provider: { async request(input) {
      walletRequest = input;
      return { accounts: [{ address: '0x8ba1f109551bd432803012645ac136ddd64dba72', capabilities: { auth: { approval_id: 'synthetic-human-approval' } } }] };
    } },
  });
  const connection = await client.connection.connect({ capabilities: { services } });
  assert.equal(connection.agentId, undefined);
  assert.equal(connection.mpp, undefined);
  assert.equal(connection.grant.permission, 'services.use');
  assert.deepEqual(connection.grant.services, services);
  const resources = walletRequest.params[0].capabilities.auth.resources;
  assert.equal(resources.some(r => r.startsWith('urn:nanocodex:agent:') || r.startsWith('urn:nanocodex:mpp:')), false);
  assert.equal(walletRequest.params[0].capabilities.authorizeAccessKey, undefined);
  assert.equal(resources.filter(r => r.startsWith('urn:nanocodex:services:')).length, 1);
  assert.deepEqual(await createServicesClient({ connect: client, grantId: connection.grant.id }).vault.list(), { vault: [] });
  await assert.rejects(client.agent.create({ connection }), /no agent authority/);
  substitute = true;
  await assert.rejects(client.connection.connect({ capabilities: { services } }), /outside the exact approved request/);
  assert.equal(requests.some(r => /agents|threads/.test(r.url)), false);
  t.diagnostic(JSON.stringify({ paths: requests.map(r => r.url), permission: connection.grant.permission, agentId: null, signedResources: resources }));
});

test('hosted phone approval is tied to its exact operation and terminal status', () => {
  const request = createHostedRequest({ service: 'phone', operationId, appOrigin: 'https://app.example', state: 'unique-state-for-phone' });
  assert.equal(new URL(request.url).pathname, '/services/phone');
  assert.equal(new URL(request.url).searchParams.get('operation_id'), operationId);
  const source = {};
  const event = { origin: request.origin, source, data: { type: 'nanocodex:service-enrollment', service: 'phone', state: request.state, operation_id: operationId, status: 'complete', body: 'do not forward' } };
  assert.deepEqual(readHostedResult(event, request, source), { type: event.data.type, service: 'phone', state: request.state, operation_id: operationId, status: 'complete' });
  for (const data of [{ ...event.data, operation_id: numberId }, { ...event.data, status: 'outcome_unknown' }, { ...event.data, status: 'pending_approval' }]) {
    assert.equal(readHostedResult({ ...event, data }, request, source), undefined);
  }
});

test('Connect reconciles a lost phone intent response by its original caller UUID over HTTP', async t => {
  let receipt;
  const { url, requests } = await server(t, (req, res) => {
    if (req.method === 'POST') {
      receipt = { operation_id: req.body.operation_id, approval_request_id: numberId, kind: 'purchase', status: 'pending_approval', phone_number: req.body.phone_number, created_at: '2026-10-05T00:00:00Z' };
      return res.destroy();
    }
    if (req.url === `/v1/grants/grant_123/services/phone/requests/${operationId}`) return json(res, { request: receipt });
    return json(res, { error: 'not_found' }, 404);
  });
  const connect = { appOrigin: 'https://app.example', fetch(path, init) { return fetch(new URL(path, url), init); } };
  const client = createServicesClient({ connect, grantId: 'grant_123' });
  await assert.rejects(client.phone.provision({ operation_id: operationId, phone_number: '+12025550101', country: 'US' }), e => e.outcomeUnknown && e.code === 'outcome_unknown');
  const recovered = (await client.phone.requests.get(operationId)).request;
  assert.equal(recovered.operation_id, operationId);
  assert.equal(recovered.approval_request_id, numberId);
  assert.equal(recovered.status, 'pending_approval');
  assert.equal(requests.filter(r => r.method === 'POST').length, 1);
  assert.equal(new URL(createHostedRequest({ service: 'phone', operationId: recovered.approval_request_id, appOrigin: connect.appOrigin }).url).searchParams.get('operation_id'), numberId);
  t.diagnostic(JSON.stringify({ paths: requests.map(r => `${r.method} ${r.url}`), recovered, dispatches: 1 }));
});

test('pre-grant hosted selection returns only one explicit item identity and cannot be confused with enrollment', () => {
  const request = createHostedRequest({ service: 'vault', action: 'select', appOrigin: 'https://app.example', state: operationId });
  assert.equal(new URL(request.url).pathname, '/vault');
  assert.equal(new URL(request.url).searchParams.get('service'), 'select');
  const source = {};
  const data = { type: 'nanocodex:service-enrollment', service: 'vault', action: 'select', state: request.state, vault_id: vaultId, kind: 'api_key', name: 'Existing key', secret: 'private', username: 'private', origin: 'https://example.com' };
  const event = { origin: request.origin, source, data };
  assert.deepEqual(readHostedResult(event, request, source), { type: data.type, service: 'vault', action: 'select', state: request.state, vault_id: vaultId, kind: 'api_key', name: 'Existing key' });
  for (const bad of [{ ...event, origin: 'https://attacker.example' }, { ...event, source: {} }, { ...event, data: { ...data, state: 'wrong' } }, { ...event, data: { ...data, action: undefined } }, { ...event, data: { ...data, kind: 'secret' } }]) assert.equal(readHostedResult(bad, request, source), undefined);
  const enrollment = createHostedRequest({ appOrigin: 'https://app.example', state: operationId });
  assert.equal(readHostedResult({ ...event, data: { ...data, kind: 'totp' } }, enrollment, source), undefined);
  assert.equal(readHostedResult({ ...event, data: { ...data, action: undefined, kind: 'totp' } }, request, source), undefined);
});
