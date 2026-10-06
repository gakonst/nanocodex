import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile, mkdir, writeFile, mkdtemp, rm } from 'node:fs/promises';
import { build } from 'esbuild';
import { Secp256k1 } from 'ox';
import { KeyAuthorization, SignatureEnvelope, TxEnvelopeTempo as TxTempo } from 'ox/tempo';
import { Account, Abis } from 'viem/tempo';
import { decodeFunctionData, encodeFunctionResult, keccak256, concatHex } from 'viem';
import { Challenge, Credential } from 'mppx';
const require = createRequire(import.meta.url);
const { Miniflare, convertV4MiniflareOptions, Log, LogLevel } = createRequire(require.resolve('wrangler/package.json'))('miniflare');
const { Wata, Store, deviceCode } = await import(createRequire(import.meta.resolve('accounts')).resolve('wata/host'));
const root = new URL('../', import.meta.url).pathname;
const rootKey = `0x${'34'.repeat(32)}`;
const external = Account.fromSecp256k1(rootKey).address.toLowerCase();
const zero = `0x${'0'.repeat(40)}`;
// Production Worker, Accounts consumer and Wata host; only consent and RPC are synthetic.
test('permanent wallet HTTP journey', { timeout: 120_000 }, async () => {
  const bundle = await build({ stdin: { resolveDir: root, sourcefile: 'wallet-link-storage-fixture.ts', contents: `
    export * from './src/egress.ts';
    export { default } from './src/egress.ts';
    import { UserCredentialBroker as Broker } from './src/egress.ts';
    // Inject only the durable storage failure; all broker behavior is production code.
    export class UserCredentialBroker extends Broker {
      constructor(state, env) {
        let fail = false;
        const put = state.storage.put.bind(state.storage);
        state.storage.put = async (...args) => {
          if (fail && args[0] === 'credential-state') { fail = false; throw Error('synthetic completion write failure'); }
          return put(...args);
        };
        super(state, env); this.arm = () => { fail = true; }; this.inspect = () => state.storage.get('credential-state');
      }
      async fetch(request) {
        if (new URL(request.url).pathname === '/test/fail-next-write') { this.arm(); return new Response('armed'); }
        if (new URL(request.url).pathname === '/test/encrypted-state') return Response.json(await this.inspect());
        return super.fetch(request);
      }
    }
  ` }, bundle: true, write: false, format: 'esm', platform: 'node', external: ['cloudflare:*', 'node:*'], alias: { 'node-rsa': root + '../nanocodex/tools/browser/unsupportedNodeRsa.mjs' }, plugins: [{ name: 'external-transports', setup(b) {
    b.onResolve({ filter: /^nanocodex\/wasm$/ }, () => ({ path: './nanocodex.wasm', external: true }));
    b.onResolve({ filter: /^\.\/whatsapp-runtime$/ }, () => ({ path: root + 'test/whatsapp/runtime.fixture.ts' }));
  } }] });
  await mkdir(root + 'output', { recursive: true });
  const persist = await mkdtemp(root + 'output/wallet-link-');
  const trace = [], logs = [];
  let registrations = 0, host, grant = 'full', revoked = false, metadata = 'unpublished', keyAddress;
  class Capture extends Log { logWithLevel(_level, message) { logs.push(String(message)); } }
  async function newHost() {
    const transport = deviceCode({ baseUrl: 'https://wallet.tempo.xyz', path: '/api/auth/device', store: Store.memory(), pollingInterval: 10, html: { render: () => new Response('synthetic approval'), authenticate: async ({ request, actions }) => {
      const form = await request.formData(); await actions[form.get('action') === 'deny' ? 'deny' : 'approve'](String(form.get('user_code'))); return new Response('ok');
    } } });
    const session = await Wata.create({ transports: [transport] }).start();
    session.onRequest(async event => { try {
      assert.equal(event.method, 'wallet_connect');
      const requested = event.params[0].capabilities.authorizeAccessKey;
      assert.equal(requested.expiry, 0); assert.equal(requested.limits, undefined); assert.equal(requested.scopes, undefined); assert.equal(requested.privateKey, undefined);
      keyAddress = requested.address; assert.match(keyAddress, /^0x[0-9a-f]{40}$/i);
      const authorization = KeyAuthorization.from({ chainId: grant === 'chain' ? 1n : 4217n, type: 'secp256k1', address: keyAddress, ...(grant === 'finite' ? { expiry: 2_000_000_000 } : {}), ...(grant === 'limited' ? { limits: [] } : {}), ...(grant === 'calls' ? { scopes: [] } : {}) });
      const signature = SignatureEnvelope.from({ type: 'secp256k1', signature: Secp256k1.sign({ payload: KeyAuthorization.getSignPayload(authorization), privateKey: grant === 'signature' ? `0x${'56'.repeat(32)}` : rootKey }) });
      await event.respond({ accounts: [{ address: external, capabilities: { keyAuthorization: KeyAuthorization.toRpc({ ...authorization, signature }) } }] });
    } catch (error) { console.error('HOST_FAILURE', error); throw error; } }); host = transport;
  }
  async function outbound(request) {
    const url = new URL(request.url);
    if (url.hostname === 'wallet.tempo.xyz') { if (url.pathname.endsWith('/register')) registrations++; return host.fetch(new Request(request.url, { method: request.method, headers: request.headers, body: await request.text() })); }
    assert.equal(url.hostname, 'rpc.tempo.xyz');
    const body = await request.json(), { method, params } = body; let result;
    if (method === 'eth_chainId') result = '0x1079';
    else if (method === 'eth_estimateGas') result = '0x186a0';
    else if (method === 'eth_gasPrice' || method === 'eth_maxPriorityFeePerGas') result = '0x1';
    else if (method === 'eth_getTransactionCount') result = '0x0';
    else if (method === 'eth_getBlockByNumber') result = { number: '0x1', timestamp: '0x1', baseFeePerGas: '0x1', gasLimit: '0x1000000', gasUsed: '0x0' };
    else if (method === 'eth_call') {
      const call = params[0];
      if (call.calls) result = '0x';
      else if (call.data.startsWith('0x70a08231')) result = `0x${'0'.repeat(58)}ffffff`;
      else if (call.data === '0x313ce567') result = `0x${'0'.repeat(63)}6`;
      else { assert.equal(decodeFunctionData({ abi: Abis.accountKeychain, data: call.data }).functionName, 'getKey'); result = encodeFunctionResult({ abi: Abis.accountKeychain, functionName: 'getKey', result: { signatureType: 0, keyId: metadata === 'foreign' ? external : (revoked || metadata !== 'unpublished') ? keyAddress : zero, expiry: metadata === 'finite' ? 2_000_000_000n : metadata === 'published' ? 0xffffffffffffffffn : 0n, enforceLimits: false, isRevoked: revoked } }); }
    } else assert.fail(`Unexpected RPC ${method}; broadcast forbidden`);
    return Response.json({ jsonrpc: '2.0', id: body.id, result });
  }
  const wasm = await readFile(root + '../nanocodex/pkg-web/nanocodex_bg.wasm');
  const options = () => convertV4MiniflareOptions({ log: new Capture(LogLevel.ERROR), resourcePersistencePath: persist, workers: [{ name: 'wallet-link-journey', modules: [{ type: 'ESModule', path: root + 'output/wallet-link-broker.js', contents: bundle.outputFiles[0].text }, { type: 'CompiledWasm', path: root + 'output/nanocodex.wasm', contents: wasm }], compatibilityDate: '2026-07-29', compatibilityFlags: ['nodejs_compat'], bindings: { ENVIRONMENT: 'test', ALLOW_LOCAL_CREDENTIAL_CLAIM: 'true', CREDENTIAL_ENCRYPTION_KEY: Buffer.from('0123456789abcdef0123456789abcdef').toString('base64url') }, durableObjects: Object.fromEntries(Object.entries({ USER_CREDENTIALS: 'UserCredentialBroker', AGENT_SUBJECTS: 'AgentSubjectDirectory', USER_CONNECTORS: 'UserConnectorBroker', MCP_CONNECTIONS: 'McpConnectionDirectory', WHATSAPP_ACCOUNTS: 'WhatsAppAccount', SPOTIFY_RATE_LIMITS: 'SpotifyRateLimit', GMAIL_PUSH_MAILBOXES: 'GmailPushMailbox' }).map(([key, className]) => [key, { className, useSQLite: true }])), outboundService: outbound }] });
  let mf = new Miniflare(options());
  const route = 'https://broker.internal/users/wallet-link-synthetic/wallet';
  async function request(path = '', method = 'GET', body, status = 200) {
    const response = await mf.dispatchFetch(route + path, { method, headers: { 'content-type': 'application/json' }, ...(body ? { body: JSON.stringify(body) } : {}) }); const raw = await response.text(); let value; try { value = JSON.parse(raw); } catch { assert.fail(raw); }
    trace.push({ path, method, expected: status, observed: response.status, value }); assert.equal(response.status, status, JSON.stringify(value)); assert.doesNotMatch(JSON.stringify(value), /privateKey|device_code|code_verifier/); return value;
  }
  const operation = () => crypto.randomUUID();
  async function start() { await newHost(); const pending = await request('/link', 'POST', { operation_id: operation() }); assert.equal(pending.status, 'pending'); assert.ok(pending.approval_url); return pending; }
  async function approve(pending, action = 'approve') { await host.fetch(new Request('https://wallet.tempo.xyz/api/auth/device/verify', { method: 'POST', body: new URLSearchParams({ user_code: pending.user_code, action }) })); }
  async function terminal(pending, expected) { for (let n = 0; n < 150; n++) { const value = await request('/link/poll', 'POST', { operation_id: pending.operation_id }); if (value.status !== 'pending') { assert.equal(value.status, expected); return value; } await new Promise(r => setTimeout(r, 20)); } assert.fail('link never completed'); }
  async function restart() { await mf.dispose(); mf = new Miniflare(options()); }
  try {
    const original = await request('', 'PUT'); assert.deepEqual(Object.keys(original).sort(), ['address', 'created_at']);
    const p = await start(), count = registrations;
    assert.equal((await request('/link', 'POST', { operation_id: p.operation_id })).operation_id, p.operation_id); assert.equal(registrations, count); await request('/link', 'POST', { operation_id: operation() }, 409);
    await approve(p); await terminal(p, 'linked');
    const linked = await request(); assert.equal(linked.address, external); assert.equal(linked.mode, 'linked'); assert.equal(linked.access_key.permissions, 'full'); assert.equal(linked.access_key.expiry, null);
    assert.deepEqual(await request('', 'PUT'), original); assert.deepEqual(await request('/identity'), original);
    await restart(); assert.deepEqual(await request(), linked);
    const payment = { idempotency_key: 'synthetic-linked-payment', approved_total: '0.05', plan: { nodes: [{ id: 'one', serviceId: 'synthetic', method: 'GET', path: '/lookup', input: {} }] }, challenge: Challenge.from({ id: 'synthetic-linked-charge', realm: 'mercator.sh', method: 'tempo', intent: 'charge', expires: new Date(Date.now() + 60_000).toISOString(), request: { amount: '50000', currency: '0x20c000000000000000000000b9537d11c60e8b50', recipient: '0x0000000000000000000000000000000000000002', methodDetails: { chainId: 4217, feePayer: true, supportedModes: ['pull'], machineTokenEnabled: true } } }) };
    const signed = await request('/mercator/credential', 'POST', payment), tx = TxTempo.deserialize(Credential.deserialize(signed.credential).payload.signature);
    assert.equal(tx.signature.type, 'keychain'); assert.equal(tx.signature.userAddress.toLowerCase(), external); assert.equal(SignatureEnvelope.verify(tx.signature.inner, { address: keyAddress, payload: tx.signature.version === "v2" ? keccak256(concatHex(["0x04", TxTempo.getSignPayload(tx), external])) : TxTempo.getSignPayload(tx) }), true);
    assert.deepEqual(await request('/mercator/credential', 'POST', payment), signed);
    assert.ok(tx.keyAuthorization, 'unpublished delegate carries retained root authorization');
    for (metadata of ['published', 'finite', 'foreign']) {
      await request('/mercator/credential', 'POST', { ...payment, idempotency_key: 'metadata-' + metadata }, metadata === 'published' ? 200 : 503);
    }
    metadata = 'unpublished';
    const namespace = await mf.getDurableObjectNamespace('USER_CREDENTIALS');
    const stored = await (await namespace.get(namespace.idFromName('wallet-link-synthetic')).fetch('https://fixture/test/encrypted-state')).text();
    assert.doesNotMatch(stored, /privateKey|authorization|linkedWallet/); assert.ok(!stored.includes(external));
    trace.push({ encrypted_storage: true, metadata_cases: ['unpublished', 'published', 'finite', 'foreign'] });
    revoked = true; await request('/mercator/credential', 'POST', { ...payment, idempotency_key: 'revoked-linked-payment' }, 503); revoked = false;
    const unlink = { operation_id: operation(), expected_address: external };
    await request('/unlink', 'POST', { ...unlink, expected_address: original.address }, 409); assert.equal((await request('/unlink', 'POST', unlink)).onchain_revoked, false); assert.equal((await request('/unlink', 'POST', unlink)).status, 'unlinked'); assert.equal((await request()).address, original.address);
    await restart(); assert.equal((await request('/unlink', 'POST', unlink)).onchain_revoked, false);
    const rootSigned = await request('/mercator/credential', 'POST', { ...payment, idempotency_key: 'root-after-unlink' });
    const rootTx = TxTempo.deserialize(Credential.deserialize(rootSigned.credential).payload.signature);
    assert.equal(rootTx.signature.type, 'secp256k1');
    assert.equal(SignatureEnvelope.verify(rootTx.signature, { address: original.address, payload: TxTempo.getSignPayload(rootTx) }), true);
    trace.push({ unlink_custody_removed: true, original_root_signer_restored: true, onchain_revocation_claimed: false });
    const beforeStart = operation(), registrationCount = registrations;
    await request('/link/poll', 'POST', { operation_id: beforeStart }, 404);
    assert.equal((await request('/link/cancel', 'POST', { operation_id: beforeStart })).status, 'cancelled');
    await restart();
    assert.equal((await request('/link', 'POST', { operation_id: beforeStart })).status, 'cancelled');
    assert.equal((await request('/link/cancel', 'POST', { operation_id: beforeStart })).status, 'cancelled');
    assert.equal(registrations, registrationCount);
    const failedWrite = await start();
    const failureNamespace = await mf.getDurableObjectNamespace('USER_CREDENTIALS');
    await failureNamespace.get(failureNamespace.idFromName('wallet-link-synthetic')).fetch('https://fixture/test/fail-next-write');
    await approve(failedWrite); await terminal(failedWrite, 'interrupted');
    assert.equal((await request()).address, original.address);
    await restart(); assert.equal((await request()).address, original.address);
    await terminal(failedWrite, 'interrupted');
    trace.push({ completion_write_failure_recovered: true, cancel_before_start_replayed: true });
    const cancelled = await start(); assert.equal((await request('/link/cancel', 'POST', { operation_id: cancelled.operation_id })).status, 'cancelled'); await approve(cancelled); await terminal(cancelled, 'cancelled');
    const interrupted = await start(); await restart(); await terminal(interrupted, 'interrupted'); assert.equal((await request('/link', 'POST', { operation_id: interrupted.operation_id })).status, 'interrupted');
    for (grant of ['chain', 'finite', 'limited', 'calls', 'signature']) { const wrong = await start(); await approve(wrong); await terminal(wrong, 'rejected'); assert.equal((await request()).address, original.address); }
    grant = 'full'; const denied = await start(); await approve(denied, 'deny'); await terminal(denied, 'rejected'); trace.push({ result: 'passed', sdk_revision: '31708a31ea324cd2cdd5e666e4ad777bd65d6c24', registrations, delegated_signature_verified: true, root_identity_preserved: true });
  } finally { await mf.dispose(); await writeFile(root + 'output/wallet-link-journey.json', JSON.stringify({ trace, logs }, null, 2)); await rm(persist, { recursive: true, force: true }); }
});
