import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { build } from 'esbuild';
const require = createRequire(import.meta.url);
const { Miniflare, convertV4MiniflareOptions, Log, LogLevel } = createRequire(require.resolve('wrangler/package.json'))('miniflare');
const root = new URL('../', import.meta.url).pathname;
const origin = 'https://recipient.example.com';
const seed = 'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ';
function base32(text) {
  let bits = 0, value = 0, output = '';
  for (const byte of Buffer.from(text)) {
    value = (value << 8) | byte; bits += 8;
    while (bits >= 5) { bits -= 5; output += 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'[(value >>> bits) & 31]; }
  }
  if (bits) output += 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'[(value << (5 - bits)) & 31];
  return output;
}
// Production routes and encrypted Durable Object storage run in workerd. The test
// entrypoint sets a deterministic clock; a DO subclass exposes only storage flags.
// These narrow test fixtures make RFC vectors and at-rest encryption observable.
const fixture = `
import worker, { UserCredentialBroker } from './src/egress.ts';
import { CredentialVault } from './src/credential-vault.ts';
import { handleManagedEgress } from '../managed/src/managed-egress.ts';
export * from './src/egress.ts';
export class InspectedBroker extends UserCredentialBroker {
  constructor(ctx, env) { super(ctx, env); this.inspectionState = ctx; this.inspectionEnv = env; }
  async fetch(request) {
    if (new URL(request.url).pathname === '/__storage') {
      const state = this.inspectionState, entries = await state.storage.list();
      const raw = JSON.stringify([...entries]);
      const metadata = entries.get('credential-state');
      const vault = new CredentialVault(this.inspectionEnv, 'user/' + state.id.toString());
      const opened = await vault.open(metadata.envelope);
      const records = [...entries.keys()].filter(key => key.startsWith('vault-entry:'));
      return Response.json({encrypted: [...entries.values()].every(row => !!row.envelope?.ciphertext),
        metadata_seed_absent: !JSON.stringify(opened.value).includes('seed'),
        plaintext_seed_absent: !raw.includes('${seed}'), records: records.length});
    }
    return super.fetch(request);
  }
}
const realNow = Date.now;
export default class extends worker { async fetch(request) {
  const env = this.env;
  Date.now = request.headers.has('x-fixture-time') ? () => Number(request.headers.get('x-fixture-time')) * 1000 : realNow;
  if (new URL(request.url).pathname === '/__storage') return env.USER_CREDENTIALS.getByName('totp-owner').fetch(request);
  if (new URL(request.url).pathname === '/__model') return handleManagedEgress(new Request('https://browser-vault.internal/v1/totp', request), { fetch: request => super.fetch(request) }, 'T'.repeat(43));
  return super.fetch(request);
}}
`;
test('TOTP intake, RFC 6238 injection, encrypted persistence and revocation through HTTP', { timeout: 120_000 }, async () => {
  const bundled = await build({ stdin: { contents: fixture, resolveDir: root }, bundle: true, write: false,
    format: 'esm', platform: 'node', external: ['cloudflare:*', 'node:*'],
    alias: { 'node-rsa': root + '../nanocodex/tools/browser/unsupportedNodeRsa.mjs' },
    plugins: [{ name: 'external-transports', setup(b) {
      b.onResolve({ filter: /^nanocodex\/wasm$/ }, () => ({ path: './nanocodex.wasm', external: true }));
      b.onResolve({ filter: /^\.\/whatsapp-runtime$/ }, () => ({ path: root + 'test/whatsapp/runtime.fixture.ts' }));
    } }],
  });
  const logs = [], trace = [], privateValues = [seed];
  class CapturedLog extends Log { logWithLevel(level, message) { logs.push(String(message)); } }
  let expectedCode, calls = 0, remoteStatus = 201;
  const mf = new Miniflare(convertV4MiniflareOptions({ log: new CapturedLog(LogLevel.DEBUG),
    handleStructuredLogs: entry => logs.push(JSON.stringify(entry)),
    workers: [{ name: 'totp-journey', modules: [
      { type: 'ESModule', path: root + 'output/totp-broker.js', contents: bundled.outputFiles[0].text },
      { type: 'CompiledWasm', path: root + 'output/nanocodex.wasm', contents: await readFile(process.env.NANOCODEX_TEST_WASM ?? root + '../nanocodex/pkg-web/nanocodex_bg.wasm') },
    ], compatibilityDate: '2026-07-29', compatibilityFlags: ['nodejs_compat'],
    bindings: { ENVIRONMENT: 'test', CREDENTIAL_ENCRYPTION_KEY: Buffer.from('0123456789abcdef0123456789abcdef').toString('base64url') },
    durableObjects: { USER_CREDENTIALS: { className: 'InspectedBroker', useSQLite: true }, AGENT_SUBJECTS: { className: 'AgentSubjectDirectory', useSQLite: true } },
    outboundService: async request => {
      calls++; assert.ok(expectedCode, 'denied requests must not reach recipient');
      assert.equal(new URL(request.url).origin, origin);
      assert.equal(request.headers.get('x-fixture-time'), null);
      const code = request.headers.get('x-code') ?? JSON.parse(await request.text()).code;
      assert.equal(code, expectedCode);
      privateValues.push(code);
      trace.push({ boundary: 'recipient', code_verified: true, status: remoteStatus });
      return new Response(code, { status: remoteStatus, headers: { 'x-code': code, location: 'https://attacker.example/' + code } });
    } }],
  }));
  const owner = 'totp-owner', subject = 'T'.repeat(43), otherSubject = 'U'.repeat(43);
  async function call(url, method, body, status = 200, headers = {}) {
    const response = await mf.dispatchFetch(url, { method, headers: { 'content-type': 'application/json', ...headers }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
    const text = await response.text();
    assert.ok(!text || text.startsWith('{'), text);
    const result = text ? JSON.parse(text) : null;
    trace.push({ boundary: 'http', route: new URL(url).pathname.replace(/[A-Za-z0-9_-]{22,}/g, ':id'), method, expected_status: status, observed_status: response.status, result });
    assert.equal(response.status, status, text);
    assert.equal(response.headers.get('location'), null); assert.equal(response.headers.get('x-code'), null);
    return result;
  }
  const save = (payload, status = 201) => call(`https://broker.internal/users/${owner}/credentials/vault/totp`, 'POST', payload, status);
  const send = (id, time, status = 200, extra = {}, selected = subject) => call('https://vault-egress.internal/v1/request', 'POST', {
    vault_id: id, url: origin + '/verify', method: 'POST', headers: { 'x-code': '{{NANOCODEX_VAULT_TOTP}}' }, ...extra,
  }, status, { 'x-nanocodex-subject': selected, 'x-fixture-time': String(time) });
  try {
    await call(`https://broker.internal/subjects/${subject}`, 'PUT', { user_id: owner });
    await call(`https://broker.internal/subjects/${otherSubject}`, 'PUT', { user_id: 'totp-other' });
    const material = { name: 'Synthetic RFC account', origin, seed, issuer: 'Example', account: 'synthetic@example.com' };
    const ids = [];
    const vectors = [
      [59, '94287082', '46119246', '90693936'],
      [1111111109, '07081804', '68084774', '25091201'],
      [1111111111, '14050471', '67062674', '99943326'],
      [1234567890, '89005924', '91819424', '93441116'],
      [2000000000, '69279037', '90698825', '38618901'],
      [20000000000, '65353130', '77737706', '47863826'],
    ];
    for (const [index, algorithm, length] of [[1, 'SHA1', 20], [2, 'SHA256', 32], [3, 'SHA512', 64]]) {
      const secret = base32('1234567890'.repeat(7).slice(0, length)); privateValues.push(secret);
      const entry = await save({ ...material, seed: secret, algorithm, digits: 8 }); ids.push(entry.id);
      assert.equal(entry.algorithm, algorithm); assert.equal(entry.period, 30);
      assert.equal('seed' in entry, false); assert.equal('otpauth_uri' in entry, false);
      for (const vector of vectors) {
        expectedCode = vector[index];
        assert.deepEqual(await send(entry.id, vector[0]), { status: 201, ok: true });
      }
    }
    const entry = await save({ name: material.name, origin, otpauth_uri: `otpauth://totp/Example:synthetic%40example.com?secret=${seed}&issuer=Example` });
    assert.equal(entry.digits, 6); assert.equal(entry.algorithm, 'SHA1'); ids.push(entry.id);
    expectedCode = '287082';
    assert.deepEqual(await send(entry.id, 59, 200, { headers: {}, body_encoding: 'json', body: '{"code":"{{NANOCODEX_VAULT_TOTP}}"}' }), { status: 201, ok: true });
    remoteStatus = 302; const beforeRedirect = calls;
    assert.deepEqual(await send(entry.id, 59), { status: 302, ok: false });
    assert.equal(calls, beforeRedirect + 1, 'redirect must not be followed');
    remoteStatus = 500;
    assert.deepEqual(await send(entry.id, 59), { status: 500, ok: false });
    assert.equal(calls, beforeRedirect + 2, 'upstream failure must not be retried'); remoteStatus = 201;
    const storage = await call('https://fixture.test/__storage', 'GET');
    assert.deepEqual(storage, { encrypted: true, metadata_seed_absent: true, plaintext_seed_absent: true, records: 4 });
    const metadata = await call(`https://broker.internal/users/${owner}/credentials/vault`, 'GET');
    assert.equal(metadata.vault.length, 4);
    // The raw code exists only on this private service-binding response. Record
    // fixed verification flags, never the response body, in inspectable evidence.
    const privateUrl = 'https://browser-vault.internal/v1/totp';
    const privateBody = { vault_id: entry.id, expected_origin: origin };
    await call('https://fixture.test/__model', 'POST', privateBody, 403);
    const privateHeaders = { 'content-type': 'application/json', 'x-nanocodex-subject': subject, 'x-fixture-time': '59' };
    await call('https://browser-vault.internal/v1/fields', 'POST', { ...privateBody, fields: ['username'] }, 403, privateHeaders);
    await call('https://browser-vault.internal/v1/fields', 'POST', { ...privateBody, fields: ['seed'] }, 400, privateHeaders);
    await call('https://browser-vault.internal/v1/fields', 'POST', { ...privateBody, fields: ['code'] }, 400, privateHeaders);
    const privateResponse = await mf.dispatchFetch(privateUrl, { method: 'POST', headers: privateHeaders, body: JSON.stringify(privateBody) });
    assert.equal(privateResponse.status, 200); assert.equal(privateResponse.headers.get('cache-control'), 'no-store');
    assert.deepEqual(await privateResponse.json(), { code: '287082' });
    trace.push({ boundary: 'private_browser_rpc', code_verified: true, cache: 'no-store', observed_status: 200 });
    for (const selected of ['', 'malformed', 'Z'.repeat(43), otherSubject]) {
      await call(privateUrl, 'POST', privateBody, 403, { ...privateHeaders, 'x-nanocodex-subject': selected });
    }
    for (const expected_origin of ['https://attacker.example', 'https://sub.recipient.example.com', origin + ':444']) {
      await call(privateUrl, 'POST', { ...privateBody, expected_origin }, 403, privateHeaders);
    }
    for (const expected_origin of [origin + '/', 'http://recipient.example.com']) {
      await call(privateUrl, 'POST', { ...privateBody, expected_origin }, 400, privateHeaders);
    }
    await call(privateUrl, 'GET', undefined, 405, privateHeaders);
    await call(privateUrl, 'POST', { ...privateBody, time: 59 }, 400, privateHeaders);
    const savedLogin = await call(`https://broker.internal/users/${owner}/credentials/vault/login`, 'POST', {
      name: 'Synthetic wrong-kind login', username: 'synthetic', password: 'synthetic-private-password',
    }, 201);
    await call(privateUrl, 'POST', { ...privateBody, vault_id: savedLogin.id }, 403, privateHeaders);
    await call(`https://broker.internal/users/${owner}/credentials/vault/login/${savedLogin.id}`, 'DELETE', undefined, 204);
    const beforeDenials = calls; expectedCode = undefined;
    for (const url of ['https://attacker.example/verify', 'https://sub.recipient.example.com/verify', origin + ':444/verify', 'http://recipient.example.com/verify', 'https://127.0.0.1/verify']) await send(entry.id, 59, 403, { url });
    await send(entry.id, 59, 409, {}, otherSubject);
    await send(entry.id, 59, 403, { headers: { 'x-code': '{{NANOCODEX_VAULT_PASSWORD}}' } });
    await send(entry.id, 59, 400, { time: 59 });
    for (const bad of [
      { ...material, seed: 'INVALID!SEED' }, { ...material, seed: 'MY' }, { ...material, algorithm: ['SHA1'] },
      { ...material, digits: 7 }, { ...material, period: 0 }, { ...material, origin: origin + '/' },
      { ...material, origin: 'http://recipient.example.com' }, { ...material, unexpected: 'value' },
      { name: material.name, origin, otpauth_uri: `otpauth://totp/Example:a?secret=${seed}&issuer=Other` },
      { name: material.name, origin, otpauth_uri: `otpauth://totp/Example:a?secret=${seed}&secret=${seed}` },
      { name: material.name, origin, otpauth_uri: `otpauth://hotp/Example:a?secret=${seed}&counter=1` },
    ]) await save(bad, 400);
    assert.equal(calls, beforeDenials);
    for (const id of ids) await call(`https://broker.internal/users/${owner}/credentials/vault/totp/${id}`, 'DELETE', undefined, 204);
    await send(entry.id, 59, 409);
    await call(privateUrl, 'POST', privateBody, 403, privateHeaders);
    assert.equal(calls, beforeDenials);
    assert.deepEqual(await call(`https://broker.internal/users/${owner}/credentials/vault`, 'GET'), { vault: [] });
    assert.equal((await call('https://fixture.test/__storage', 'GET')).records, 0);
    assert.ok(logs.some(line => line.includes('egress.request')));
    for (const secret of privateValues) {
      assert.equal(JSON.stringify(trace).includes(secret), false, 'seed or code leaked in receipt');
      assert.equal(logs.join('\n').includes(secret), false, 'seed or code leaked in logs');
    }
  } finally {
    await mkdir(root + 'output', { recursive: true });
    await writeFile(root + 'output/vault-totp-journey.json', JSON.stringify(trace, null, 2));
    await writeFile(root + 'output/vault-totp-worker.log', logs.join('\n'));
    await mf.dispose();
  }
});
