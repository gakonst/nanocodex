import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { resolveReleasedImages } from './released-images.mjs';

const account = 'a'.repeat(32);
const receipt = (image, marker = 'b', target = account) => ({ version: 1, image, input: marker.repeat(64),
  ref: `registry.cloudflare.com/${target}/nanocodex-ci-${image}@sha256:${marker.repeat(64)}` });
function fixture(t) {
  const cwd = mkdtempSync(join(tmpdir(), 'released-images-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const put = (path, value) => { mkdirSync(dirname(join(cwd, path)), { recursive: true }); writeFileSync(join(cwd, path), value); };
  const source = '{"main":"src/index.ts","containers":[{"class_name":"PhoneContainer","image":"../phone-cloud/Dockerfile","image_build_context":"../..","max_instances":1},{"class_name":"Sandbox","image":"./Dockerfile","instance_type":"standard-3"}]}';
  put('js/managed/wrangler.jsonc', source);
  const entries = { phone: [], sandbox: [] }, statuses = new Map(), calls = [];
  let id = 0;
  const add = (image, value, state = 'success') => {
    const entry = { id: ++id, environment: `nanocodex-image-${image}`, payload: { schema: 1, receipt: value } };
    entries[image].unshift(entry); statuses.set(id, [{ state, environment: entry.environment }]); return entry;
  };
  const request = async call => {
    calls.push(call); assert.equal(call.method, 'GET');
    if (call.path.includes('/statuses?')) return statuses.get(Number(call.path.split('/').at(-2)));
    assert.ok(call.path.endsWith('per_page=100'));
    return entries[call.path.includes('image-phone&') ? 'phone' : 'sandbox'];
  };
  return { cwd, put, entries, statuses, calls, add, source,
    options: { account, cwd, repository: 'fixture/repo', request, fingerprint: () => 'f'.repeat(64) }, pinned: join(cwd, '.ci-images/released.json') };
}

test('falls back to latest successful published receipts and preserves config', async t => {
  const f = fixture(t);
  f.add('phone', receipt('phone')); f.add('phone', receipt('phone', 'c')); f.add('sandbox', receipt('sandbox', 'd'));
  const images = await resolveReleasedImages(f.options);
  assert.deepEqual(images, { phone: receipt('phone', 'c'), sandbox: receipt('sandbox', 'd') });
  assert.deepEqual(JSON.parse(readFileSync(f.pinned, 'utf8')), { schema: 1, account, images });
  assert.deepEqual(JSON.parse(readFileSync(join(f.cwd, 'js/managed/wrangler.ci.jsonc'), 'utf8')), {
    main: 'src/index.ts', containers: [
      { class_name: 'PhoneContainer', image: images.phone.ref, max_instances: 1 },
      { class_name: 'Sandbox', image: images.sandbox.ref, instance_type: 'standard-3' },
    ],
  });
  assert.equal(readFileSync(join(f.cwd, 'js/managed/wrangler.jsonc'), 'utf8'), f.source);
});

test('ignores malformed, failed, pending, foreign-account and wrong-environment history', async t => {
  const f = fixture(t);
  for (const image of ['phone', 'sandbox']) {
    f.add(image, receipt(image));
    f.add(image, receipt(image, 'c'), 'failure'); f.add(image, receipt(image, 'd'), 'in_progress');
    f.add(image, receipt(image, 'e', 'f'.repeat(32)));
    f.add(image, { ...receipt(image), ref: 'tag:latest' });
    f.add(image, receipt(image)).payload = '{invalid';
    const entry = f.add(image, receipt(image, 'f'));
    f.statuses.set(entry.id, [{ state: 'success', environment: 'unrelated' }]);
    f.add(image, receipt(image, 'f')).environment = 'unrelated';
  }
  assert.deepEqual(await resolveReleasedImages(f.options), { phone: receipt('phone'), sandbox: receipt('sandbox') });
});

test('frozen job selection ignores later publications and requires no network', async t => {
  const f = fixture(t);
  f.add('phone', receipt('phone')); f.add('sandbox', receipt('sandbox'));
  const first = await resolveReleasedImages(f.options), original = readFileSync(f.pinned, 'utf8');
  f.add('phone', receipt('phone', 'c')); f.add('sandbox', receipt('sandbox', 'd'));
  assert.deepEqual(await resolveReleasedImages({ ...f.options, repository: undefined,
    request: async () => { throw new Error('must not access network'); } }), first);
  assert.equal(readFileSync(f.pinned, 'utf8'), original);
});

test('no successful published image fails without partial selection or configuration', async t => {
  const f = fixture(t);
  f.add('phone', receipt('phone')); f.add('sandbox', receipt('sandbox'), 'failure');
  await assert.rejects(resolveReleasedImages(f.options), /No successful published sandbox/);
  assert.equal(existsSync(f.pinned), false);
  assert.equal(existsSync(join(f.cwd, 'js/managed/wrangler.ci.jsonc')), false);
  assert.ok(f.calls.every(call => call.method === 'GET'));
});

test('history is bounded to the latest 100 records', async t => {
  const f = fixture(t);
  f.add('phone', receipt('phone')); f.add('sandbox', receipt('sandbox'));
  for (let i = 0; i < 100; i++) f.add('sandbox', { version: 99 });
  await assert.rejects(resolveReleasedImages(f.options), /No successful published sandbox/);
  assert.equal(existsSync(f.pinned), false);
});

test('corrupt or cross-account job selection fails without replacement', async t => {
  const f = fixture(t);
  for (const value of ['{invalid', JSON.stringify({ schema: 1, account: 'f'.repeat(32), images: {} }),
    JSON.stringify({ schema: 1, account, images: { phone: receipt('phone'), sandbox: { ...receipt('sandbox'), input: 'bad' } } })]) {
    f.put('.ci-images/released.json', value);
    await assert.rejects(resolveReleasedImages(f.options));
    assert.equal(readFileSync(f.pinned, 'utf8'), value);
  }
  assert.equal(f.calls.length, 0);
});

test('network uncertainty fails without recording a selection', async t => {
  const f = fixture(t);
  await assert.rejects(resolveReleasedImages({ ...f.options, request: async () => { throw new Error('unavailable'); } }), /unavailable/);
  assert.equal(existsSync(f.pinned), false);
});

test('prefers current-input receipt over a newer stale publication', async t => {
  const f = fixture(t);
  for (const image of ['phone', 'sandbox']) {
    f.add(image, receipt(image, 'f')); // current input
    f.add(image, receipt(image, 'b')); // stale producer finished afterward
  }
  assert.deepEqual(await resolveReleasedImages(f.options), { phone: receipt('phone', 'f'), sandbox: receipt('sandbox', 'f') });
});

test('forced image rollout requires exact current phone and sandbox keys', async t => {
  const f = fixture(t);
  f.add('phone', receipt('phone', 'f')); f.add('sandbox', receipt('sandbox', 'b'));
  await assert.rejects(resolveReleasedImages({ ...f.options, requireCurrent: true }), /current inputs; image rollout deferred/);
  assert.equal(existsSync(f.pinned), false);
  assert.equal(existsSync(join(f.cwd, 'js/managed/wrangler.ci.jsonc')), false);
  f.add('sandbox', receipt('sandbox', 'f'));
  assert.deepEqual(await resolveReleasedImages({ ...f.options, requireCurrent: true }), {
    phone: receipt('phone', 'f'), sandbox: receipt('sandbox', 'f'),
  });
});

test('strict rollout rejects frozen stale selection instead of changing a job plan', async t => {
  const f = fixture(t);
  f.add('phone', receipt('phone')); f.add('sandbox', receipt('sandbox'));
  await resolveReleasedImages(f.options);
  const original = readFileSync(f.pinned, 'utf8'), count = f.calls.length;
  f.add('phone', receipt('phone', 'f')); f.add('sandbox', receipt('sandbox', 'f'));
  await assert.rejects(resolveReleasedImages({ ...f.options, requireCurrent: true }), /receipt does not match current image inputs/);
  assert.equal(readFileSync(f.pinned, 'utf8'), original);
  assert.equal(f.calls.length, count);
});

test('current input resolution receives account, epoch and checkout scope', async t => {
  const f = fixture(t), calls = [];
  f.add('phone', receipt('phone', 'c')); f.add('sandbox', receipt('sandbox', 'd'));
  const images = await resolveReleasedImages({ ...f.options, epoch: '2', requireCurrent: true,
    fingerprint: (image, targetAccount, epoch, cwd) => {
      calls.push([image, targetAccount, epoch, cwd]);
      return (image === 'phone' ? 'c' : 'd').repeat(64);
    },
  });
  assert.equal(images.phone.input, 'c'.repeat(64));
  assert.deepEqual(calls, [['phone', account, '2', f.cwd], ['sandbox', account, '2', f.cwd]]);
});

test('current-first lookup avoids status reads for unrelated history', async t => {
  const f = fixture(t);
  f.add('phone', receipt('phone', 'f')); f.add('sandbox', receipt('sandbox', 'f'));
  for (let i = 0; i < 20; i++) {
    f.add('phone', receipt('phone')); f.add('sandbox', receipt('sandbox'));
  }
  await resolveReleasedImages(f.options);
  assert.equal(f.calls.filter(call => call.path.includes('/statuses?')).length, 2);
});
