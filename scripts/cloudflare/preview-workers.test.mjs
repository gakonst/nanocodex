import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { createServer } from 'node:http';
import { failureClassifier, previewConfig, providerClient, retryAfterMs, runWrangler } from './preview-workers.mjs';

const revision = 'a'.repeat(40);
// Shape of the built js/account/dist/nanocodex/wrangler.json (synthetic identifiers).
const built = () => ({
  name: 'nanocodex', main: 'index.js', no_bundle: true, exports: {}, compatibility_date: '2026-07-29',
  assets: { binding: 'ASSETS', directory: '../client', run_worker_first: ['/', '/api/*'] },
  vars: { ENVIRONMENT: 'production' },
  services: [{ binding: 'NANOCODEX_BACKEND', service: 'nanocodex-durable-agent' }],
  durable_objects: { bindings: [
    { name: 'CHATGPT_EGRESS', class_name: 'ChatGptEgress' },
    { name: 'NANOCODEX_LIVE_SESSIONS', class_name: 'DurableAgentSession', script_name: 'nanocodex-durable-agent' },
  ] },
  migrations: [{ tag: 'v1', new_sqlite_classes: ['ChatGptEgress'] }, { tag: 'v2', deleted_classes: ['Old'] }],
  containers: [{ class_name: 'ChatGptEgress', image: '/runner/js/account/container/Dockerfile', image_build_context: '/runner/js/account/container' }],
  d1_databases: [{ binding: 'EVALS_DB', database_name: 'nanocodex-evals' }],
});

test('production-backed account Preview owns no Durable Object namespaces, migrations or containers', () => {
  const source = built();
  const config = previewConfig(source, { revision, backend: 'production' });
  for (const key of ['migrations', 'containers', 'durable_objects', 'exports', 'env', '$schema']) assert.equal(Object.hasOwn(config, key), false, key);
  assert.deepEqual(config.previews, { vars: { ENVIRONMENT: 'production', NANOCODEX_PREVIEW_REVISION: revision },
    services: [{ binding: 'NANOCODEX_PREVIEW_PRODUCTION', service: 'nanocodex' }] });
  assert.equal(config.assets.run_worker_first, true);
  assert.equal(source.migrations.length, 2, 'source config is not mutated');
  assert.throws(() => previewConfig({ ...built(), exports: { ChatGptEgress: { type: 'durable_object' } } }, { revision, backend: 'production' }),
    /cannot declare Worker exports/);
  const isolated = previewConfig(built(), { revision, backend: 'isolated', images: ['registry.example/egress:1'] });
  assert.equal(isolated.previews.containers[0].image, 'registry.example/egress:1');
  assert.equal(isolated.migrations.length, 2, 'isolated Previews keep owning their namespaces');
});

function fakeWrangler(source) {
  const dir = mkdtempSync(join(tmpdir(), 'fake-wrangler-'));
  const bin = join(dir, 'wrangler.js');
  writeFileSync(bin, source);
  return { bin, dispose: () => rmSync(dir, { recursive: true, force: true }) };
}

test('Wrangler failure reports only numeric Cloudflare codes and fixed categories', { timeout: 30_000 }, async () => {
  const secret = 'synthetic-secret-value-7f3a';
  const fake = fakeWrangler(`
const write = text => new Promise(done => process.stderr.write(text, done));
let input = ''; process.stdin.on('data', chunk => input += chunk);
process.stdin.on('end', async () => {
  if (JSON.parse(input).TOKEN !== '${secret}') process.exit(9);
  await write('\\u001b[31m✘ [ERROR]\\u001b[0m A request to the Cloudflare API (/accounts/0123/workers/workers/nanocodex/previews) failed.\\n');
  await write('  Durable Object migration rejected for class ChatGptEgress [code: 10074]\\n');
  await write(JSON.stringify({ env: { TOKEN: { type: 'secret_text', text: '${secret}' } } }) + '\\n');
  for (let i = 0; i < 64; i++) await write('x'.repeat(65536) + '\\n');
  await write('container application failed [co'); await write('de: 10021]\\n');
  process.exit(1);
});`);
  try {
    const error = await runWrangler(['preview'], { cwd: process.cwd(), env: process.env, secrets: { TOKEN: secret }, bin: fake.bin })
      .then(() => assert.fail('expected failure'), error => error);
    assert.equal(error.message, 'Wrangler Preview failed (exit 1); Cloudflare API codes: 10021, 10074; categories: cloudflare-api-request, migrations, durable-objects, containers; raw output withheld to protect binding values');
    for (const leaked of [secret, '0123', 'ChatGptEgress', 'TOKEN', 'secret_text', '✘']) assert.equal(error.message.includes(leaked), false, leaked);
  } finally { fake.dispose(); }
});

test('Wrangler success and unclassified failures stay fixed', { timeout: 30_000 }, async () => {
  const ok = fakeWrangler('process.stderr.write("[code: 1]\\n"); process.exit(0);');
  const quiet = fakeWrangler('process.stderr.write("synthetic private detail\\n", () => process.exit(2));');
  try {
    assert.equal(await runWrangler([], { cwd: process.cwd(), env: process.env, bin: ok.bin }), undefined);
    await assert.rejects(runWrangler([], { cwd: process.cwd(), env: process.env, bin: quiet.bin }),
      { message: 'Wrangler Preview failed (exit 2); Cloudflare API codes: none; categories: unclassified; raw output withheld to protect binding values' });
  } finally { ok.dispose(); quiet.dispose(); }
});

test('the classifier keeps a bounded window and caps codes', () => {
  const classifier = failureClassifier({ window: 16, maxCodes: 2 });
  for (let i = 0; i < 1000; i++) classifier.push('filler '.repeat(100));
  classifier.push('[code: 3] [code: 1] [code: 2]');
  assert.deepEqual(classifier.summary(), { codes: [1, 3], categories: [] });
});

test('provider metadata reads survive a transient timeout; mutations are sent once', async () => {
  const account = '0'.repeat(32);
  const replies = [];
  const calls = [];
  const request = async (url, init) => {
    calls.push(init.method);
    const next = replies.shift();
    if (next instanceof Error) throw next;
    return { status: next, ok: next < 400, json: async () => ({ success: true, result: { ok: next } }) };
  };
  const get = providerClient({ account, token: 't', request, retryDelay: async () => {} });
  const timeout = Object.assign(new Error('timed out'), { name: 'TimeoutError' });

  replies.push(timeout, 503, 200);
  assert.deepEqual(await get('workers/workers/w'), { ok: 200 });
  assert.deepEqual(calls.splice(0), ['GET', 'GET', 'GET']);

  replies.push(timeout, timeout, timeout);
  await assert.rejects(get('workers/workers/w'), /lookup failed \(HTTP unavailable\)/);
  assert.equal(calls.splice(0).length, 3);

  replies.push(403);
  await assert.rejects(get('workers/workers/w'), /lookup failed \(HTTP 403\)/);
  assert.equal(calls.splice(0).length, 1);

  replies.push(404);
  assert.equal(await get('workers/workers/w/previews/p', { optional: true }), null);
  calls.splice(0);

  replies.push(503);
  await assert.rejects(get('workers/workers/w', { method: 'DELETE' }), /lookup failed \(HTTP 503\)/);
  assert.deepEqual(calls.splice(0), ['DELETE']);
});

test('a throttled metadata read waits for the bounded Retry-After over real HTTP', async () => {
  const account = '0'.repeat(32);
  const seen = [];
  const server = createServer((req, res) => {
    seen.push({ method: req.method, url: req.url, authorization: req.headers.authorization, at: Date.now() });
    if (seen.length === 1) {
      res.writeHead(429, { 'Retry-After': '1', 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ success: false, errors: [{ code: 10000, message: 'rate limited' }] }));
      return;
    }
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ success: true, result: { name: 'nanocodex' } }));
  });
  await new Promise(done => server.listen(0, '127.0.0.1', done));
  const { port } = server.address();
  // Only the provider origin is replaced: path, headers and timeout signal go over a real socket.
  const request = (url, init) => fetch(url.replace('https://api.cloudflare.com', `http://127.0.0.1:${port}`), init);
  try {
    const get = providerClient({ account, token: 'synthetic-token', request });
    assert.deepEqual(await get('workers/workers/nanocodex'), { name: 'nanocodex' });
  } finally {
    await new Promise(done => server.close(done));
  }
  assert.equal(seen.length, 2);
  assert.deepEqual(seen.map(({ method, url, authorization }) => [method, url, authorization]), Array(2).fill(
    ['GET', `/client/v4/accounts/${account}/workers/workers/nanocodex`, 'Bearer synthetic-token'],
  ));
  assert.ok(seen[1].at - seen[0].at >= 950, 'second read honors Retry-After: 1');
  assert.equal(retryAfterMs('3600'), 20_000);
  assert.equal(retryAfterMs(new Date(5_000).toUTCString(), 0), 5_000);
  assert.equal(retryAfterMs('soon'), undefined);
});
