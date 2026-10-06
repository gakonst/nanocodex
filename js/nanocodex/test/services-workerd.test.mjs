import assert from 'node:assert/strict';
import test from 'node:test';
import { build } from 'esbuild';
import { Miniflare } from 'miniflare';
import { fileURLToPath } from 'node:url';

// Native workerd fetch rejects redirect:'error' before dispatch. Keep this journey
// on its real runtime: a Node fetch fixture alone cannot detect that regression.
test('standalone SDK works over native workerd HTTP and refuses credential-bearing redirects', async t => {
  const compiled = await build({
    stdin: { contents: `import { createServicesClient } from 'nanocodex/services';
      export default { async fetch(request) {
        const path = new URL(request.url).pathname;
        const client = createServicesClient({ apiKey: 'synthetic-workerd-key', baseUrl: 'https://account.synthetic.example' });
        try {
          if (path === '/vault') return Response.json(await client.vault.list());
          return Response.json(await client.vault.request({ vault_id: 'vault_synthetic_item_123456789', url: 'https://example.com' + path }));
        } catch (error) {
          return Response.json({ code: error.code, outcomeUnknown: error.outcomeUnknown }, { status: 502 });
        }
      } }`, resolveDir: fileURLToPath(new URL('..', import.meta.url)) },
    bundle: true, write: false, format: 'esm', platform: 'browser', target: 'es2022',
  });
  const calls = [], trace = [];
  const mf = new Miniflare({
    port: 0, modules: true, script: compiled.outputFiles[0].text,
    compatibilityDate: '2026-07-30', compatibilityFlags: ['enable_request_signal'],
    outboundService: async request => {
      const url = new URL(request.url);
      calls.push({ origin: url.origin, path: url.pathname, method: request.method, authorized: request.headers.get('authorization') === 'Bearer synthetic-workerd-key' });
      if (request.method === 'GET') return Response.json({ vault: [{ id: 'vault_synthetic_item_123456789', kind: 'api_key', name: 'Example key', created_at: 1, secret: 'private' }] });
      const input = await request.json();
      if (input.url.endsWith('/redirect')) return new Response('redirect', { status: 307, headers: { location: 'https://attacker.example/collect' } });
      return Response.json({ status: 201, ok: true, body: 'private' });
    },
  });
  try {
    const base = await mf.ready;
    for (const [path, expected] of [['/vault', 200], ['/request', 200], ['/redirect', 502]]) {
      const response = await fetch(new URL(path, base));
      const value = await response.json();
      trace.push({ path, expected, observed: response.status, value });
      assert.equal(response.status, expected);
    }
    assert.deepEqual(trace[0].value, { vault: [{ id: 'vault_synthetic_item_123456789', kind: 'api_key', name: 'Example key', created_at: 1 }] });
    assert.deepEqual(trace[1].value, { status: 201, ok: true });
    assert.deepEqual(trace[2].value, { code: 'outcome_unknown', outcomeUnknown: true });
    assert.equal(calls.length, 3);
    assert(calls.every(call => call.origin === 'https://account.synthetic.example' && call.authorized));
  } finally {
    await mf.dispose();
    t.diagnostic(JSON.stringify({ runtime: 'workerd', trace, calls }));
  }
});
