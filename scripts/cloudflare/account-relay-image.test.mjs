import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { configure, deploymentConfig, ensure, fingerprint, imageInputs, receiptStore, restore, validateReceipt } from './account-relay-image.mjs';

const account = 'a'.repeat(32), digest = 'b'.repeat(64), input = 'c'.repeat(64);
const image = 'account-relay', helper = 'scripts/cloudflare/account-relay-image.mjs';
const ref = `registry.cloudflare.com/${account}/nanocodex-ci-account-relay@sha256:${digest}`;
const makeReceipt = key => ({ version: 1, image, input: key, ref });
const receiptPath = '.ci-images/account-relay.json';
const configPath = 'js/account/dist/nanocodex/wrangler.json';

function fixture(t) {
  const cwd = mkdtempSync(join(tmpdir(), 'account-relay-image-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const git = (...args) => execFileSync('git', args, { cwd, stdio: 'pipe' });
  const put = (path, content) => {
    mkdirSync(dirname(join(cwd, path)), { recursive: true });
    writeFileSync(join(cwd, path), content);
  };
  const commit = () => { git('add', '.'); git('-c', 'user.name=CI', '-c', 'user.email=ci@example.invalid', 'commit', '-qm', 'fixture'); };
  git('init', '-q');
  put('.gitignore', '.ci-images/\njs/account/dist/\n');
  put(helper, readFileSync(new URL('./account-relay-image.mjs', import.meta.url)));
  put('js/account/container/Dockerfile', readFileSync(new URL('../../js/account/container/Dockerfile', import.meta.url)));
  put('js/account/container/relay.mjs', 'console.log("synthetic relay");\n');
  commit();
  return { cwd, git, put, commit, options: { account, cwd, epoch: '1' }, key: () => fingerprint(account, '1', cwd) };
}

function ledger() {
  const calls = [], entries = [], statuses = new Map();
  const request = async call => {
    calls.push(call);
    if (call.method === 'POST' && call.path.endsWith('/deployments')) {
      const entry = { ...call.body, sha: call.body.ref, id: entries.length + 1 };
      entries.unshift(entry); return entry;
    }
    if (call.path.includes('/statuses')) {
      const id = Number(call.path.split('/').at(-2));
      if (call.method === 'POST') { const status = { ...call.body, id: calls.length }; statuses.set(id, [status]); return status; }
      return statuses.get(id) ?? [];
    }
    return entries;
  };
  return { calls, entries, statuses, store: receiptStore({ repository: 'fixture/repo', ref: 'd'.repeat(40), request }) };
}

function publisher({ fail, repoDigests = [ref], manifest = { Descriptor: { digest: `sha256:${digest}` } } } = {}) {
  const calls = [];
  return { calls, run(command, args, options) {
    calls.push({ command, args, options });
    if (fail === command || fail === args[0]) throw new Error('injected command failure');
    if (args[0] === 'image') return JSON.stringify(repoDigests);
    if (args[0] === 'manifest') return JSON.stringify(manifest);
    return '';
  } };
}

function sourceConfig(cwd, absolute = false) {
  return {
    name: 'fixture-account', main: 'index.js', assets: { directory: '../client', binding: 'ASSETS' },
    migrations: [{ tag: 'v1', new_sqlite_classes: ['ChatGptEgress'] }],
    durable_objects: { bindings: [{ name: 'EGRESS', class_name: 'ChatGptEgress' }] },
    containers: ['', 'Wnam', 'Enam', 'Weur', 'Eeur', 'Apac', 'Sam', 'Oc'].map((suffix, i) => ({
      class_name: `ChatGptEgress${suffix}`,
      image: absolute ? resolve(cwd, 'js/account/container/Dockerfile') : '../../container/Dockerfile',
      image_build_context: absolute ? resolve(cwd, 'js/account/container') : '../../container',
      image_vars: {}, max_instances: i ? 50 : 1000, instance_type: 'lite',
      ...(i ? { constraints: { regions: suffix === 'Oc' ? ['OC', 'APAC'] : [suffix.toUpperCase()] } } : {}),
    })),
  };
}

test('receipt binds exact input, account, repository, image and immutable digest', () => {
  const receipt = makeReceipt(input);
  assert.equal(validateReceipt(receipt, image, account, input), ref);
  for (const patch of [{ version: 2 }, { image: 'phone' }, { input: digest },
    { ref: ref.replace(account, 'e'.repeat(32)) }, { ref: ref.replace('account-relay', 'sandbox') },
    { ref: ref.replace(`@sha256:${digest}`, ':latest') }, { ref: `${ref}/extra` }, { ref: null }]) {
    assert.throws(() => validateReceipt({ ...receipt, ...patch }, image, account, input));
  }
  assert.throws(() => validateReceipt(receipt, 'phone', account, input));
});

test('key uses committed COPY inputs and helper, scopes account/epoch, ignores unrelated changes', t => {
  const f = fixture(t), first = f.key();
  assert.deepEqual(imageInputs(f.cwd).map(entry => entry.path), [
    'js/account/container/Dockerfile', 'js/account/container/relay.mjs', helper,
  ].sort());
  for (const path of ['js/account/worker/index.ts', 'js/account/container/relay.test.mjs', 'js/managed/src/index.ts', 'README.md']) f.put(path, 'unrelated');
  f.commit();
  assert.equal(f.key(), first);
  assert.notEqual(fingerprint('e'.repeat(32), '1', f.cwd), first);
  assert.notEqual(fingerprint(account, '2', f.cwd), first);
  let previous = first;
  for (const [path, content] of [
    ['js/account/container/relay.mjs', 'console.log("changed relay");\n'],
    ['js/account/container/.dockerignore', '*.test.mjs\n'],
    ['js/account/container/Dockerfile.dockerignore', '*.test.mjs\n'],
    [helper, readFileSync(resolve(f.cwd, helper), 'utf8') + '\n// changed publisher\n'],
  ]) {
    f.put(path, content); f.commit(); assert.notEqual(f.key(), previous); previous = f.key();
  }
  f.put('js/account/container/egress-relay.mjs', 'additional copied runtime');
  f.put('js/account/container/Dockerfile', 'FROM node:22-alpine\nCOPY ["relay.mjs", "egress-relay.mjs", "/app/"]\n');
  f.commit();
  assert.ok(imageInputs(f.cwd).some(entry => entry.path.endsWith('/egress-relay.mjs')));
  previous = f.key();
  f.put('js/account/container/egress-relay.mjs', 'changed additional runtime'); f.commit();
  assert.notEqual(f.key(), previous);
});

test('key CLI prints only the exact digest and supports GitHub output', t => {
  const f = fixture(t), output = join(f.cwd, 'step-output');
  const result = execFileSync(process.execPath, [fileURLToPath(new URL('./account-relay-image.mjs', import.meta.url)), 'key'], {
    cwd: f.cwd, encoding: 'utf8', env: { ...process.env, CLOUDFLARE_ACCOUNT_ID: account, MANAGED_IMAGE_CACHE_EPOCH: '1', GITHUB_OUTPUT: output },
  });
  assert.equal(result, f.key() + '\n');
  assert.equal(readFileSync(output, 'utf8'), `input=${f.key()}\n`);
});

test('unknown Docker build semantics or uncommitted COPY files fail closed', t => {
  const f = fixture(t);
  for (const instruction of ['COPY . /app/', 'COPY *.mjs /app/', 'COPY --from=other relay.mjs /app/',
    'ADD https://example.invalid/source /app/', 'RUN --mount=type=bind,target=/app true',
    'COPY uncommitted.mjs /app/', 'COPY ../outside.mjs /app/']) {
    f.put('js/account/container/Dockerfile', `FROM node:22-alpine\n${instruction}\n`); f.commit();
    assert.throws(() => f.key(), /unsupported|committed regular file/);
  }
});

test('all eight regional images share one digest and retain relative assets, limits and bindings', t => {
  const f = fixture(t);
  for (const absolute of [false, true]) {
    const source = sourceConfig(f.cwd, absolute);
    const rewritten = JSON.parse(deploymentConfig(JSON.stringify(source), ref, { cwd: f.cwd }));
    const expected = structuredClone(source);
    for (const container of expected.containers) {
      container.image = ref; delete container.image_build_context; delete container.image_vars;
    }
    assert.deepEqual(rewritten, expected);
    assert.equal(new Set(rewritten.containers.map(container => container.image)).size, 1);
    assert.equal(rewritten.containers.length, 8);
    f.put(configPath, JSON.stringify(source));
    f.put(receiptPath, JSON.stringify(makeReceipt(f.key())));
    assert.equal(configure(f.options), 'js/account/dist/nanocodex/wrangler.ci.json');
    assert.deepEqual(JSON.parse(readFileSync(join(f.cwd, configPath), 'utf8')), source, 'original remains intact');
    assert.deepEqual(JSON.parse(readFileSync(join(f.cwd, 'js/account/dist/nanocodex/wrangler.ci.json'), 'utf8')), expected);
  }
});

test('config rejects missing/unknown Dockerfiles, alternate contexts, build args and environment overrides', t => {
  const f = fixture(t);
  for (const change of [
    value => { value.containers = []; },
    value => { value.containers[7].image = '../../future/Dockerfile'; },
    value => { value.containers[7].image = ref; },
    value => { value.containers[7].image_build_context = '../..'; },
    value => { value.containers[7].image_vars = { UNSUPPORTED: 'yes' }; },
    value => { value.containers[7].configuration = { image: ref }; },
    value => { value.env = { production: { containers: [] } }; },
  ]) {
    const source = sourceConfig(f.cwd); change(source);
    assert.throws(() => deploymentConfig(JSON.stringify(source), ref, { cwd: f.cwd }));
  }
});

test('cold ensure builds/pushes once, verifies digest, saves durable success; warm reuse never rebuilds', async t => {
  const f = fixture(t), history = ledger(), commands = publisher();
  const options = { ...f.options, store: history.store, run: commands.run, testsEnabled: false };
  const receipt = await ensure(options);
  assert.deepEqual(receipt, makeReceipt(f.key()));
  assert.deepEqual(JSON.parse(readFileSync(join(f.cwd, receiptPath), 'utf8')), receipt);
  assert.equal(commands.calls.filter(call => call.args[0] === 'build').length, 1);
  assert.equal(commands.calls.filter(call => call.command === 'pnpm').length, 1);
  assert.deepEqual(commands.calls.find(call => call.command === 'pnpm').args, [
    '--filter', 'nanocodex-web', 'exec', 'wrangler', 'containers', 'push', `nanocodex-ci-account-relay:input-${f.key()}`,
  ]);
  assert.ok(commands.calls[0].args.includes('--pull'));
  assert.ok(!commands.calls.some(call => call.args.includes('buildx') || call.args[0] === 'run' || call.args[0] === 'login'));
  assert.equal(history.entries.length, 1);
  assert.equal(history.entries[0].environment, 'nanocodex-image-account-relay');
  assert.equal(history.statuses.get(1)[0].state, 'success');
  const count = commands.calls.length;
  assert.deepEqual(await ensure(options), receipt);
  assert.equal(commands.calls.length, count);
  assert.equal(history.entries.length, 1);
  // Actions cache eviction: reconstruct local receipt from durable history only.
  rmSync(join(f.cwd, receiptPath));
  assert.deepEqual(await ensure(options), receipt);
  assert.equal(commands.calls.length, count);
  assert.equal(history.entries.length, 1);
});

test('valid Actions cache receipt is retained durably without Docker', async t => {
  const f = fixture(t), history = ledger(), commands = publisher({ fail: 'docker' });
  f.put(receiptPath, JSON.stringify(makeReceipt(f.key())));
  assert.deepEqual(await ensure({ ...f.options, store: history.store, run: commands.run }), makeReceipt(f.key()));
  assert.equal(commands.calls.length, 0);
  assert.equal(history.entries.length, 1);
});

test('malformed, foreign or stale cached receipts fail before Docker or durable writes', async t => {
  const f = fixture(t), history = ledger(), commands = publisher();
  for (const receipt of ['{invalid', JSON.stringify(makeReceipt(input)),
    JSON.stringify({ ...makeReceipt(f.key()), ref: ref.replace(account, 'e'.repeat(32)) })]) {
    f.put(receiptPath, receipt);
    await assert.rejects(ensure({ ...f.options, store: history.store, run: commands.run }));
    assert.equal(commands.calls.length, 0); assert.equal(history.calls.length, 0);
  }
});

test('manifest fallback verifies actual registry digest when Docker lacks RepoDigests', async t => {
  const f = fixture(t), history = ledger(), commands = publisher({ repoDigests: null });
  assert.deepEqual(await ensure({ ...f.options, store: history.store, run: commands.run, testsEnabled: true }), makeReceipt(f.key()));
  assert.ok(commands.calls.some(call => call.args[0] === 'run'));
  assert.ok(commands.calls.some(call => call.args[0] === 'manifest' && call.args.at(-1).endsWith(`:input-${f.key()}`)));
});

test('build, smoke, push and verification failures create no successful receipt', async t => {
  const f = fixture(t);
  for (const failure of [{ fail: 'build' }, { fail: 'run' }, { fail: 'pnpm' },
    { repoDigests: [ref.replace(`sha256:${digest}`, 'latest')] }, { repoDigests: [ref, ref] },
    { repoDigests: null, manifest: { Descriptor: { digest: 'unverified' } } }]) {
    const history = ledger(), commands = publisher(failure);
    await assert.rejects(ensure({ ...f.options, store: history.store, run: commands.run, testsEnabled: true }));
    assert.equal(existsSync(join(f.cwd, receiptPath)), false);
    assert.equal(history.calls.filter(call => call.method === 'POST').length, 0);
    if (['build', 'run'].includes(failure.fail)) assert.ok(!commands.calls.some(call => call.command === 'pnpm'));
  }
});

test('integrity checks stay enabled when smoke tests are paused', async t => {
  const f = fixture(t), history = ledger(), commands = publisher({ repoDigests: [ref + '-invalid'] });
  await assert.rejects(ensure({ ...f.options, store: history.store, run: commands.run, testsEnabled: false }));
  assert.ok(!commands.calls.some(call => call.args[0] === 'run'));
  assert.equal(existsSync(join(f.cwd, receiptPath)), false);
  assert.equal(history.entries.length, 0);
});

test('dirty sources and ignored Docker config changes reject reuse before any external command', async t => {
  const f = fixture(t), history = ledger(), commands = publisher();
  f.put(receiptPath, JSON.stringify(makeReceipt(f.key())));
  f.put('js/account/container/relay.mjs', 'dirty');
  await assert.rejects(ensure({ ...f.options, store: history.store, run: commands.run }), /Commit relevant/);
  f.git('checkout', '--', 'js/account/container/relay.mjs');
  f.git('update-index', '--assume-unchanged', 'js/account/container/relay.mjs');
  f.put('js/account/container/relay.mjs', 'hidden dirty');
  await assert.rejects(ensure({ ...f.options, store: history.store, run: commands.run }), /dirty account relay/);
  f.git('update-index', '--no-assume-unchanged', 'js/account/container/relay.mjs');
  f.git('checkout', '--', 'js/account/container/relay.mjs');
  f.put('.git/info/exclude', 'js/account/container/.dockerignore\n');
  f.put('js/account/container/.dockerignore', 'relay.mjs\n');
  await assert.rejects(restore({ ...f.options, store: history.store }), /Commit relevant/);
  assert.equal(commands.calls.length, 0); assert.equal(history.calls.length, 0);
});

test('failed durable status is never written to Actions cache or restored as success', async t => {
  const f = fixture(t), history = ledger(), commands = publisher();
  const store = { ...history.store, save: async () => { throw new Error('durable write failed'); } };
  await assert.rejects(ensure({ ...f.options, store, run: commands.run, testsEnabled: false }), /durable write/);
  assert.equal(existsSync(join(f.cwd, receiptPath)), false);
  await history.store.save(makeReceipt(f.key()), account, f.key());
  history.statuses.set(1, [{ state: 'in_progress', environment: 'nanocodex-image-account-relay' }]);
  assert.equal(await restore({ ...f.options, store: history.store }), null);
});
