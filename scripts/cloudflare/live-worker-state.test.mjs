import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { currentWorkerDeployment, releaseTag, workerScripts } from './live-worker-state.mjs';

const account = '1'.repeat(32), fingerprint = 'a'.repeat(64);
const deploymentId = '11111111-1111-4111-8111-111111111111';
const versionId = '22222222-2222-4222-8222-222222222222';
function fixture() {
  const calls = [];
  const listing = { deployments: [{ id: deploymentId, versions: [{ version_id: versionId, percentage: 100 }] }] };
  const version = { id: versionId, annotations: { 'workers/tag': releaseTag(fingerprint) } };
  const options = { account, token: 'synthetic-private-token', request: async (url, init) => {
    calls.push({ url, init });
    return { ok: true, json: async () => ({ success: true, result: url.endsWith('/deployments') ? listing : version }) };
  } };
  return { calls, listing, version, options, read: () => currentWorkerDeployment('managed', options) };
}

test('live state reads only the selected account/script and a sole 100% version', async () => {
  const f = fixture();
  assert.deepEqual(await f.read(), { account, script: workerScripts.managed, deploymentId, versionId, tag: releaseTag(fingerprint) });
  assert.deepEqual(f.calls.map(call => call.url), [
    `https://api.cloudflare.com/client/v4/accounts/${account}/workers/scripts/nanocodex-durable-agent/deployments`,
    `https://api.cloudflare.com/client/v4/accounts/${account}/workers/scripts/nanocodex-durable-agent/versions/${versionId}`,
  ]);
  for (const { init } of f.calls) {
    assert.equal(init.method, 'GET'); assert.equal(init.redirect, 'error');
    assert.equal(init.headers.Authorization, 'Bearer synthetic-private-token');
    assert.equal(init.headers['Cache-Control'], 'no-cache'); assert.ok(init.signal instanceof AbortSignal);
  }
});

test('empty, partial, gradual, malformed, or untagged live state cannot be reused', async () => {
  for (const mutate of [
    f => { f.listing.deployments = []; },
    f => { f.listing.deployments[0].id = 'invalid'; },
    f => { f.listing.deployments[0].versions[0].percentage = 99; },
    f => { f.listing.deployments[0].versions.push({ version_id: versionId, percentage: 0 }); },
    f => { f.listing.deployments[0].versions[0].version_id = '../another-script'; },
    f => { f.version.id = deploymentId; },
    f => { f.version.annotations = {}; },
  ]) {
    const f = fixture(); mutate(f);
    await assert.rejects(f.read(), /^Error: Live Worker deployment could not be verified$/);
  }
});

test('provider failures redact diagnostics and invalid context never sends credentials', async () => {
  for (const request of [
    async () => { throw Error('synthetic-private-token'); },
    async () => ({ ok: false, json: async () => { throw Error('must not read private body'); } }),
    async () => ({ ok: true, json: async () => ({ success: false, errors: ['synthetic-private-token'] }) }),
  ]) {
    const f = fixture(); f.options.request = request;
    await assert.rejects(f.read(), error => error.message === 'Live Worker deployment could not be verified' && error.cause === undefined);
  }
  for (const patch of [{ account: `${account}\n` }, { account: '../other' }, { token: '' }]) {
    const f = fixture(); Object.assign(f.options, patch);
    await assert.rejects(f.read()); assert.equal(f.calls.length, 0);
  }
  const f = fixture(); await assert.rejects(currentWorkerDeployment('toString', f.options)); assert.equal(f.calls.length, 0);
  assert.throws(() => releaseTag(`${fingerprint}\n`));
});

test('live script inventory matches the deployed production configurations', () => {
  const configs = {
    egress: 'js/egress/wrangler.broker.jsonc', x: 'js/x-api/wrangler.jsonc', managed: 'js/managed/wrangler.jsonc',
    email: 'js/email/wrangler.jsonc', dialog: 'js/connect-dialog/wrangler.jsonc', 'connect-api': 'js/connect-api/wrangler.jsonc',
    astra: 'examples/astra-mpp-trial/wrangler.jsonc', 'chief-of-staff': 'js/chief-of-staff/wrangler.jsonc',
    playground: 'js/connect-playground/wrangler.jsonc', account: 'js/account/wrangler.jsonc',
  };
  assert.deepEqual(Object.keys(workerScripts).sort(), Object.keys(configs).sort());
  for (const [worker, path] of Object.entries(configs)) {
    const source = readFileSync(new URL(`../../${path}`, import.meta.url), 'utf8');
    assert.equal(source.match(/^  "name": "([^"]+)"/m)?.[1], workerScripts[worker], path);
  }
});
