import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import test from 'node:test';
import { main } from './configure-mach-onramp.mjs';
import { getConfig } from '../../js/nanocodex/cloud/actions/machineUsd.mjs';
import { http } from '../../js/nanocodex/cloud/Transport.mjs';

const env = {
  CLOUDFLARE_ACCOUNT_ID: 'a'.repeat(32),
  CLOUDFLARE_API_TOKEN: 'synthetic-cloudflare-secret',
  MACH_ONRAMP_RELAY_TOKEN: 'synthetic-relay-secret-at-least-32-characters',
  MACH_ONRAMP_RELAY_HOST: 'issuer-relay.synthetic.workers.dev',
  MACH_ONRAMP_RELAY_URL: 'https://issuer-relay.synthetic.workers.dev',
  MACH_ONRAMP_ACTION: 'configure',
};
const config = { chain_id: 4217, token_address: '0x20c000000000000000000000f37de3740adec032',
  min_usd_amount_cents: 500, max_usd_amount_cents: 10000, stripe_publishable_key: 'pk_live_synthetic_fixture' };

// External Cloudflare/issuer endpoints use synthetic accounts over a real HTTP
// listener. The shipped operator runs unchanged; only destination routing is local.
async function journey(t, overrides = {}, behavior = {}) {
  const requests = [];
  const logs = [];
  const bindings = new Map((behavior.existingBindings ?? []).map(binding => [binding.name, binding.name === 'MACH_ONRAMP_RELAY_TOKEN' ? env.MACH_ONRAMP_RELAY_TOKEN : env.MACH_ONRAMP_RELAY_URL]));
  const server = createServer(async (req, res) => {
    const chunks = [];
    for await (const chunk of req) chunks.push(chunk);
    const body = chunks.length ? JSON.parse(Buffer.concat(chunks)) : undefined;
    const entry = { method: req.method, path: req.url, headers: req.headers, body };
    requests.push(entry);
    const json = (status, value) => { res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(value)); };
    if (req.url.startsWith('/api.cloudflare.com/')) {
      assert.equal(req.headers.authorization, `Bearer ${env.CLOUDFLARE_API_TOKEN}`);
      assert.equal(req.headers['x-nanocodex-mach-relay-token'], undefined);
      if (req.url.endsWith('/subdomain')) return json(200, { success: true, result: { subdomain: behavior.wrongAccount ? 'other' : 'gakonst' } });
      assert.match(req.url, /\/scripts\/nanocodex-connect-api\/(settings|secrets)$/);
      if (req.url.endsWith('/settings')) return json(behavior.missingWorker ? 404 : 200, { success: true, result: { bindings: [{ name: 'CONNECT_STATE', type: 'durable_object_namespace', class_name: 'ConnectNonceStorage', ...behavior.binding }, ...(behavior.existingBindings ?? [])] } });
      assert.equal(req.method, 'PUT');
      assert.equal(body.type, 'secret_text');
      bindings.set(body.name, body.text);
      if (behavior.disconnectWrite) return req.socket.destroy();
      if (behavior.rejectOrigin && body.name === 'MACH_ONRAMP_RELAY_URL') return json(500, { error: env.MACH_ONRAMP_RELAY_TOKEN });
      return json(behavior.writeStatus ?? 200, behavior.writeResponse ?? { success: true, result: { ...body } }); // Even an echo must never be logged.
    }
    assert.equal(req.method, 'GET');
    assert.equal(req.headers.authorization, undefined);
    if (req.url.startsWith('/issuer-relay.synthetic.workers.dev/')) {
      assert.equal(req.url, '/issuer-relay.synthetic.workers.dev/v1/config');
      if (!req.headers['x-nanocodex-mach-relay-token']) return json(behavior.publicRelay ? 200 : 401, {});
      assert.equal(req.headers['x-nanocodex-mach-relay-token'], env.MACH_ONRAMP_RELAY_TOKEN);
      if (behavior.redirect) { res.writeHead(302, { location: '/credential-trap' }); return res.end(); }
      return json(200, { ...config, ...behavior.relayConfig });
    }
    assert.equal(req.url, '/nanocodex-connect-api.gakonst.workers.dev/v1/machine-usd/config');
    assert.equal(req.headers['x-nanocodex-mach-relay-token'], undefined);
    if (!behavior.verifying) {
      assert.equal(bindings.get('MACH_ONRAMP_RELAY_TOKEN'), env.MACH_ONRAMP_RELAY_TOKEN);
      assert.equal(bindings.get('MACH_ONRAMP_RELAY_URL'), env.MACH_ONRAMP_RELAY_URL);
    }
    return json(200, { ...config, ...(behavior.disabled ? { onramp_enabled: false } : {}), ...behavior.publicConfig });
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  const transport = (url, init) => {
    const target = new URL(url);
    return fetch(`http://127.0.0.1:${server.address().port}/${target.host}${target.pathname}`, init);
  };
  const code = await main({ ...env, ...overrides }, transport, line => logs.push(line));
  if (code === 0) {
    const client = http('https://nanocodex-connect-api.gakonst.workers.dev', { fetch: transport }).setup({ appId: 'synthetic-mach-validation' });
    const normalized = await getConfig(client);
    assert.equal(normalized.onrampEnabled, true);
    assert.equal(normalized.chainId, 4217);
    assert.equal(normalized.minUsdAmountCents, 500);
    assert.equal(normalized.maxUsdAmountCents, 10000);
    assert.equal(normalized.tokenAddress, config.token_address);
    assert.equal(normalized.stripePublishableKey, config.stripe_publishable_key);
    logs.push('normalized: public_sdk_enabled');
  }
  const output = logs.join('\n');
  for (const secret of [env.CLOUDFLARE_API_TOKEN, env.MACH_ONRAMP_RELAY_TOKEN]) assert.equal(output.includes(secret), false);
  assert.ok(logs.every(line => /^(?:[a-z_]+: http_\d{3}|failed: [a-z_]+|verified: relay_auth_required canonical_mach_enabled public_config_200|normalized: public_sdk_enabled)$/.test(line)));
  t.diagnostic(JSON.stringify({ code, transcript: logs, requests: requests.map(({ method, path }) => `${method} ${path}`) }));
  return { code, output, requests, writes: requests.filter(r => r.method === 'PUT') };
}

test('configure verifies issuer authentication, writes only fixed Worker bindings, and checks public config', async t => {
  const result = await journey(t);
  assert.equal(result.code, 0);
  assert.deepEqual(result.writes.map(r => r.body.name), ['MACH_ONRAMP_RELAY_TOKEN', 'MACH_ONRAMP_RELAY_URL']);
  assert.equal(result.requests.length, 8);
});
test('verify is read-only and checks the same live boundaries', async t => {
  const result = await journey(t, { MACH_ONRAMP_ACTION: 'verify' }, { verifying: true });
  assert.equal(result.code, 0);
  assert.equal(result.writes.length, 0);
  assert.equal(result.requests.length, 6);
});
test('unverified origins fail before any request or credential transmission', async t => {
  for (const origin of ['https://evil.workers.dev', env.MACH_ONRAMP_RELAY_URL + '/', env.MACH_ONRAMP_RELAY_URL + '?next=evil', 'http://issuer-relay.synthetic.workers.dev']) {
    const result = await journey(t, { MACH_ONRAMP_RELAY_URL: origin });
    assert.equal(result.code, 1);
    assert.equal(result.requests.length, 0);
  }
});
test('wrong account and missing Worker cannot create or change a Worker', async t => {
  for (const behavior of [{ wrongAccount: true }, { missingWorker: true }, { binding: { class_name: 'OtherStorage' } }, { binding: { script_name: 'other-worker' } }, { binding: { environment: 'preview' } }]) {
    const result = await journey(t, {}, behavior);
    assert.equal(result.code, 1);
    assert.equal(result.writes.length, 0);
  }
});
test('public relay, redirect and noncanonical config all fail before writes', async t => {
  for (const behavior of [{ publicRelay: true }, { redirect: true }, ...[
    { token_address: '0x' + '1'.repeat(40) }, { chain_id: 4218 },
    { min_usd_amount_cents: 499 }, { max_usd_amount_cents: 10001 },
    { stripe_publishable_key: 'pk_test_fixture' }, { stripe_publishable_key: 'pk_live_' },
    { onramp_enabled: false }, { onramp_enabled: null },
  ].map(relayConfig => ({ relayConfig }))]) {
    const result = await journey(t, {}, behavior);
    assert.equal(result.code, 1);
    assert.equal(result.writes.length, 0);
    assert.equal(result.requests.some(r => r.path.includes('credential-trap')), false);
  }
});
test('applied write followed by disconnect is unknown, stops, and never retries', async t => {
  const result = await journey(t, {}, { disconnectWrite: true });
  assert.equal(result.code, 1);
  assert.equal(result.writes.length, 1);
  assert.match(result.output, /write_token_outcome_unknown_do_not_retry/);
});
test('partial configuration and disabled public config remain failures without automatic retries', async t => {
  const partial = await journey(t, {}, { rejectOrigin: true });
  assert.equal(partial.code, 1);
  assert.equal(partial.writes.length, 2);
  assert.match(partial.output, /write_origin_unconfirmed_do_not_retry/);
  const disabled = await journey(t, {}, { disabled: true });
  assert.equal(disabled.code, 1);
  assert.equal(disabled.writes.length, 2);
  assert.match(disabled.output, /public_config_invalid_or_disabled/);
});

const existingToken = [{ name: 'MACH_ONRAMP_RELAY_TOKEN', type: 'secret_text' }];
test('HTTP 201 secret creation succeeds only with a valid matching result', async t => {
  const success = await journey(t, {}, { writeStatus: 201 });
  assert.equal(success.code, 0);
  assert.equal(success.writes.length, 2);
  assert.match(success.output, /write_token: http_201/);
  assert.match(success.output, /write_origin: http_201/);
  for (const writeResponse of [
    { success: false, result: { name: 'MACH_ONRAMP_RELAY_TOKEN', type: 'secret_text' } },
    { success: true, result: { name: 'OTHER', type: 'secret_text' } },
    { success: true, result: { name: 'MACH_ONRAMP_RELAY_TOKEN', type: 'plain_text' } },
    { success: true },
  ]) {
    const result = await journey(t, {}, { writeStatus: 201, writeResponse });
    assert.equal(result.code, 1);
    assert.equal(result.writes.length, 1);
    assert.match(result.output, /write_token_unconfirmed_do_not_retry/);
  }
});
test('origin-only recovery reconciles token metadata and writes only origin once', async t => {
  const result = await journey(t, { MACH_ONRAMP_ACTION: 'configure-origin' }, {
    existingBindings: existingToken, writeStatus: 201,
  });
  assert.equal(result.code, 0);
  assert.deepEqual(result.writes.map(r => r.body.name), ['MACH_ONRAMP_RELAY_URL']);
  assert.equal(result.requests.length, 7);
});
test('origin-only recovery refuses any existing origin and missing or nonsecret token', async t => {
  for (const type of ['secret_text', 'plain_text']) {
    const result = await journey(t, { MACH_ONRAMP_ACTION: 'configure-origin' }, {
      existingBindings: [...existingToken, { name: 'MACH_ONRAMP_RELAY_URL', type }],
    });
    assert.equal(result.code, 1);
    assert.equal(result.writes.length, 0);
    assert.equal(result.requests.length, 2);
    assert.match(result.output, /origin_already_bound_run_verify/);
  }
  for (const existingBindings of [[], [{ name: 'MACH_ONRAMP_RELAY_TOKEN', type: 'plain_text' }]]) {
    const result = await journey(t, { MACH_ONRAMP_ACTION: 'configure-origin' }, { existingBindings });
    assert.equal(result.code, 1);
    assert.equal(result.writes.length, 0);
    assert.equal(result.requests.length, 2);
    assert.match(result.output, /recovery_requires_existing_secret_token/);
  }
});
test('origin-only recovery still requires authenticated canonical relay and never retries unknown writes', async t => {
  for (const behavior of [{ publicRelay: true }, { redirect: true }, { relayConfig: { chain_id: 1 } }]) {
    const result = await journey(t, { MACH_ONRAMP_ACTION: 'configure-origin' }, { existingBindings: existingToken, ...behavior });
    assert.equal(result.code, 1);
    assert.equal(result.writes.length, 0);
  }
  const result = await journey(t, { MACH_ONRAMP_ACTION: 'configure-origin' }, {
    existingBindings: existingToken, disconnectWrite: true,
  });
  assert.equal(result.code, 1);
  assert.deepEqual(result.writes.map(r => r.body.name), ['MACH_ONRAMP_RELAY_URL']);
  assert.equal(result.requests.length, 5);
  assert.match(result.output, /write_origin_outcome_unknown_do_not_retry/);
});
