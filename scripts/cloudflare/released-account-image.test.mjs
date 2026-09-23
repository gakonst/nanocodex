import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { applicationName, configureReleasedAccount, deployedAccountImages, lastReleasedAccountReceipt, releasedAccountIdentity } from './released-account-image.mjs';

const account = 'a'.repeat(32), input = 'b'.repeat(64);
const ref = `registry.cloudflare.com/${account}/nanocodex-ci-account-relay@sha256:${'c'.repeat(64)}`;
const environment = 'nanocodex-image-account-relay';
const receipt = { version: 1, image: 'account-relay', input, ref };
const configPath = 'js/account/dist/nanocodex/wrangler.json';
const outputPath = 'js/account/dist/nanocodex/wrangler.ci.json';
const source = () => ({
  name: 'fixture-account', main: 'index.js', assets: { directory: '../client' },
  durable_objects: { bindings: [{ name: 'EGRESS', class_name: 'ChatGptEgress' }] },
  containers: ['', 'Wnam', 'Enam', 'Weur', 'Eeur', 'Apac', 'Sam', 'Oc'].map((suffix, index) => ({
    class_name: `ChatGptEgress${suffix}`, image: '../../container/Dockerfile',
    image_build_context: '../../container', image_vars: {}, max_instances: index ? 50 : 1000,
    instance_type: 'lite', ...(index ? { constraints: { regions: [suffix.toUpperCase()] } } : {}),
  })),
});
const entry = (id, value = receipt) => ({ id, environment, payload: { schema: 1, receipt: value } });
function history(entries = [], states = new Map()) {
  const calls = [];
  return { calls, request: async call => {
    calls.push(call); assert.equal(call.method, 'GET');
    if (!call.path.includes('/statuses?')) return entries;
    return [{ state: states.get(Number(call.path.split('/').at(-2))) ?? 'success', environment }];
  } };
}
function fixture(t, config = source()) {
  const cwd = mkdtempSync(join(tmpdir(), 'released-account-image-'));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  mkdirSync(dirname(join(cwd, configPath)), { recursive: true });
  writeFileSync(join(cwd, configPath), JSON.stringify(config));
  return { cwd, config, output: () => JSON.parse(readFileSync(join(cwd, outputPath), 'utf8')) };
}
function applications(config) {
  return config.containers.map((container, index) => ({
    name: applicationName(config, container), durable_objects: { namespace_id: `namespace-${index}` },
    configuration: { image: `registry.cloudflare.com/${account}/legacy-controller-${index}:version-${index}`,
      environment_variables: [{ name: 'DO_NOT_COPY', value: 'private-provider-value' }] },
  }));
}
const provider = result => async () => ({ ok: true, json: async () => ({ success: true, result }) });

test('latest successful published receipt may have any valid input and only GETs are issued', async () => {
  const old = { ...receipt, input: 'd'.repeat(64) };
  const mock = history([entry(3), entry(2, old), entry(1)], new Map([[3, 'failure']]));
  assert.deepEqual(await lastReleasedAccountReceipt({ account, repository: 'fixture/repo', request: mock.request }), old);
  assert.deepEqual(mock.calls.map(call => call.path), [
    'repos/fixture/repo/deployments?environment=nanocodex-image-account-relay&per_page=100',
    'repos/fixture/repo/deployments/3/statuses?per_page=1',
    'repos/fixture/repo/deployments/2/statuses?per_page=1',
  ]);
});

test('malformed, foreign-account and mutable receipts cannot become published releases', async () => {
  const bad = [
    { ...receipt, input: 'invalid' }, { ...receipt, ref: ref.replace(account, 'f'.repeat(32)) },
    { ...receipt, ref: ref.replace(/@sha256:.+$/, ':latest') }, { ...receipt, image: 'sandbox' },
  ];
  const mock = history(bad.map((value, index) => entry(index + 1, value)));
  assert.equal(await lastReleasedAccountReceipt({ account, repository: 'fixture/repo', request: mock.request }), null);
  assert.equal(mock.calls.length, 1);
});

test('receipt path rewrites all eight controllers without reading the provider or requiring image inputs', async t => {
  const f = fixture(t), mock = history([entry(1)]);
  assert.equal(await configureReleasedAccount({ account, cwd: f.cwd, repository: 'fixture/repo', receiptRequest: mock.request,
    request: async () => { throw new Error('provider must not be read for a published receipt'); } }), outputPath);
  const expected = structuredClone(f.config);
  for (const container of expected.containers) {
    container.image = ref; delete container.image_build_context; delete container.image_vars;
  }
  assert.deepEqual(f.output(), expected);
  assert.deepEqual(JSON.parse(readFileSync(join(f.cwd, configPath), 'utf8')), f.config);
});

test('bootstrap preserves every deployed controller image, explicit names, settings and relative assets', async t => {
  const config = source(); config.containers[1].name = 'Explicit-Controller';
  const f = fixture(t, config), apps = applications(config), calls = [];
  await configureReleasedAccount({ account, cwd: f.cwd, repository: 'fixture/repo', token: 'fixture-token',
    receiptRequest: history().request, request: async (url, options) => {
      calls.push(url);
      assert.equal(url, `https://api.cloudflare.com/client/v4/accounts/${account}/containers/applications`);
      assert.equal(options.method, 'GET'); assert.equal(options.redirect, 'error');
      assert.equal(options.headers.Authorization, 'Bearer fixture-token');
      return provider([...apps].reverse())();
    } });
  assert.equal(calls.length, 1);
  const output = f.output();
  assert.deepEqual(output.containers.map(container => container.image), apps.map(app => app.configuration.image));
  assert.deepEqual(output.assets, config.assets);
  assert.equal(output.containers[1].name, 'Explicit-Controller');
  assert.deepEqual(output.containers.map(container => container.constraints), config.containers.map(container => container.constraints));
  assert.ok(!JSON.stringify(output).includes('private-provider-value'));
  assert.ok(output.containers.every(container => !('image_build_context' in container) && !('image_vars' in container)));
});

test('unavailable publication history bootstraps from live apps without printing the lookup error', async t => {
  const f = fixture(t);
  await configureReleasedAccount({ account, cwd: f.cwd, repository: 'fixture/repo', token: 'fixture-token',
    receiptRequest: async () => { throw new Error('private lookup detail'); }, request: provider(applications(f.config)) });
  assert.equal(f.output().containers.length, 8);
});

test('missing or ambiguous apps, namespace-less apps and invalid images fail closed without writing config', async t => {
  for (const change of [
    apps => apps.slice(1), apps => [...apps, apps[0]],
    apps => { delete apps[0].durable_objects; return apps; },
    apps => { apps[0].configuration.image = './Dockerfile'; return apps; },
    apps => { apps[0].configuration.image = `registry.cloudflare.com/${'f'.repeat(32)}/other:version`; return apps; },
  ]) {
    const f = fixture(t);
    await assert.rejects(configureReleasedAccount({ account, cwd: f.cwd, repository: 'fixture/repo', token: 'fixture-token',
      receiptRequest: history().request, request: provider(change(applications(f.config))) }), /no image build was attempted/);
    assert.equal(existsSync(join(f.cwd, outputPath)), false);
  }
});

test('provider failures are sanitized and never disclose token, response or cause', async () => {
  for (const request of [
    async () => { throw new Error('private-token and body'); },
    async () => ({ ok: false, json: async () => { throw new Error('body must not be read'); } }),
    async () => ({ ok: true, json: async () => ({ success: false, errors: [{ message: 'private body' }] }) }),
  ]) {
    await assert.rejects(deployedAccountImages(source(), { account, token: 'private-token', request }), error => {
      assert.equal(error.message, 'Released account relay image could not be resolved; no image build was attempted');
      assert.equal(error.cause, undefined); return true;
    });
  }
});

test('Wrangler default naming and short registry refs match installed 4.127.1 behavior', async () => {
  const config = source(); config.name = 'Fixture Account';
  assert.equal(applicationName(config, config.containers[0]), 'fixture-account-chatgptegress');
  const apps = applications(config);
  apps[0].configuration.image = 'legacy-controller:version-1';
  apps[1].configuration.image = 'registry.cloudflare.com/other-controller:version-2';
  const refs = await deployedAccountImages(config, { account, token: 'fixture-token', request: provider(apps) });
  assert.equal(refs[0], `registry.cloudflare.com/${account}/legacy-controller:version-1`);
  assert.equal(refs[1], `registry.cloudflare.com/${account}/other-controller:version-2`);
});


test('published identity and configure share one pin despite a newer publication during the build', async t => {
  const f = fixture(t), initial = history([entry(1)]);
  const options = { account, cwd: f.cwd, repository: 'fixture/repo', receiptRequest: initial.request };
  assert.equal(await releasedAccountIdentity(options), ref);
  const later = history([entry(2, { ...receipt, ref: ref.replace('c'.repeat(64), 'd'.repeat(64)) })]);
  assert.equal(await releasedAccountIdentity({ ...options, receiptRequest: later.request }), ref);
  await configureReleasedAccount({ ...options, receiptRequest: later.request,
    request: async () => { throw new Error('published pin must avoid provider lookup'); } });
  assert.equal(later.calls.length, 0);
  assert.ok(f.output().containers.every(container => container.image === ref));
  assert.deepEqual(JSON.parse(readFileSync(join(f.cwd, '.ci-images/released-account.json'), 'utf8')),
    { version: 1, account, repository: 'fixture/repo', receipt });
});

test('bootstrap identity pins null and ignores a receipt published before configuration', async t => {
  const f = fixture(t), options = { account, cwd: f.cwd, repository: 'fixture/repo' };
  assert.equal(await releasedAccountIdentity({ ...options, receiptRequest: history().request }), 'live');
  const later = history([entry(1)]);
  assert.equal(await releasedAccountIdentity({ ...options, receiptRequest: later.request }), 'live');
  await configureReleasedAccount({ ...options, token: 'fixture-token', receiptRequest: later.request,
    request: provider(applications(f.config)) });
  assert.equal(later.calls.length, 0);
  assert.deepEqual(f.output().containers.map(container => container.image), applications(f.config).map(app => app.configuration.image));
});
