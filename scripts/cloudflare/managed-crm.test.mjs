import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtempSync, readFileSync, writeFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { deployManaged } from './managed-crm.mjs';

// Failure modes defined before implementation: missing/unauthorized database,
// ambiguous create, malformed provider identity, failed migration, superseded
// release, and preview accidentally using production credentials or bindings.
// Provider calls are isolated because this suite must never mutate cloud state.
function fixture(t, { missing = false, failMigration = false } = {}) {
  const directory = mkdtempSync(join(tmpdir(), 'managed-crm-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const config = join(directory, 'wrangler.jsonc');
  const source = {
    name: 'nanocodex-durable-agent', main: 'src/index.ts',
    d1_databases: [{ binding: 'NANOCODEX_CRM', database_name: 'nanocodex-crm-production', migrations_dir: 'migrations' }],
    env: { development: { d1_databases: [{ binding: 'NANOCODEX_CRM', database_name: 'wrong-inherited-db' }] } },
  };
  writeFileSync(config, JSON.stringify(source));
  const events = [], generated = [];
  const database = { name: 'nanocodex-crm-production', uuid: '11111111-1111-4111-8111-111111111111' };
  const options = {
    config, env: { CLOUDFLARE_ACCOUNT_ID: 'a'.repeat(32), CLOUDFLARE_API_TOKEN: 'synthetic-secret', CLOUDFLARE_ENV: 'development' },
    isCurrent: async () => true,
    request: async (url, init) => {
      events.push({ type: 'api', method: init.method, url, body: init.body });
      if (init.method === 'GET' && missing) return Response.json({ success: false }, { status: 404 });
      return Response.json({ success: true, result: database });
    },
    run: async (command, args, settings) => {
      const path = args[args.indexOf('--config') + 1];
      const effective = JSON.parse(readFileSync(path, 'utf8'));
      generated.push(path);
      events.push({ type: args.includes('migrations') ? 'migrate' : 'upload', command, args, settings, effective });
      if (failMigration && args.includes('migrations')) throw Error('synthetic migration failure');
    },
  };
  return { options, events, source, config, generated, database };
}

test('first release creates the named database, migrates, then uploads the same pinned binding', async t => {
  const f = fixture(t, { missing: true });
  await deployManaged('deploy', { ...f.options, deployArgs: ['--message', 'literal release', '--tag', 'release-tag'] });
  assert.deepEqual(f.events.map(e => e.type === 'api' ? e.method : e.type), ['GET', 'POST', 'migrate', 'upload']);
  assert.deepEqual(JSON.parse(f.events[1].body), { name: f.database.name });
  for (const event of f.events.filter(e => e.effective)) {
    assert.equal(event.effective.d1_databases[0].database_id, f.database.uuid);
    assert.equal(event.effective.env, undefined);
    assert.ok(event.args.includes('--env='));
  }
  assert.ok(f.events[2].args.includes('--remote'));
  assert.equal(f.events[3].args.at(-1), 'release-tag');
  assert.deepEqual(JSON.parse(readFileSync(f.config, 'utf8')), f.source);
  assert.ok(f.generated.every(path => !existsSync(path)));
});

test('existing database is reused; a failed migration prevents upload and cleans generated config', async t => {
  const f = fixture(t, { failMigration: true });
  await assert.rejects(deployManaged('deploy', f.options), /migration failure/);
  assert.deepEqual(f.events.map(e => e.type === 'api' ? e.method : e.type), ['GET', 'migrate']);
  assert.ok(f.generated.every(path => !existsSync(path)));
});

test('provider authorization, identity and ambiguous creation failures never reach migrations/upload', async t => {
  for (const scenario of ['denied', 'wrong-name', 'invalid-id', 'create-unknown']) {
    const f = fixture(t);
    const requests = [];
    f.options.request = async (_, init) => {
      requests.push(init.method);
      if (scenario === 'denied') return Response.json({ success: false }, { status: 403 });
      if (scenario === 'create-unknown') {
        if (init.method === 'GET') return Response.json({ success: false }, { status: 404 });
        throw Error('uncertain network result with sensitive details');
      }
      return Response.json({ success: true, result: { ...f.database, ...(scenario === 'wrong-name' ? { name: 'other-database' } : { uuid: 'invalid' }) } });
    };
    await assert.rejects(deployManaged('deploy', f.options), /CRM database/);
    assert.deepEqual(requests, scenario === 'create-unknown' ? ['GET', 'POST'] : ['GET']);
    assert.equal(f.generated.length, 0);
  }
});

test('supersession after migration stops upload; a preview event cannot enter production deployment', async t => {
  const f = fixture(t);
  f.options.isCurrent = async () => !f.events.some(e => e.type === 'migrate');
  await assert.rejects(deployManaged('deploy', f.options), /superseded/);
  assert.ok(!f.events.some(e => e.type === 'upload'));
  const preview = fixture(t);
  preview.options.env.GITHUB_EVENT_NAME = 'pull_request';
  delete preview.options.isCurrent;
  await assert.rejects(deployManaged('deploy', preview.options), /production/);
  assert.equal(preview.events.length, 0);
});

test('preview overrides production IDs, uses only local migrations and dry-run, even with production credentials', async t => {
  const f = fixture(t);
  f.source.d1_databases[0].database_id = f.database.uuid;
  f.source.d1_databases[0].preview_database_id = f.database.uuid;
  f.source.d1_databases[0].remote = true;
  writeFileSync(f.config, JSON.stringify(f.source));
  f.options.request = () => assert.fail('preview must not call Cloudflare');
  f.options.isCurrent = () => assert.fail('preview must not enter production guard');
  await deployManaged('preview', f.options);
  assert.deepEqual(f.events.map(e => e.type), ['migrate', 'upload']);
  assert.ok(f.events[0].args.includes('--local'));
  assert.ok(!f.events[0].args.includes('--remote'));
  assert.ok(f.events[1].args.includes('--dry-run'));
  assert.ok(f.events[1].args.includes('none'));
  for (const { effective, settings } of f.events) {
    assert.deepEqual(effective.d1_databases, [{ binding: 'NANOCODEX_CRM', database_name: 'nanocodex-crm-preview', database_id: 'nanocodex-crm-preview', migrations_dir: 'migrations', remote: false }]);
    assert.equal(effective.env, undefined);
    assert.equal(settings.env.CLOUDFLARE_API_TOKEN, undefined);
    assert.equal(settings.env.CLOUDFLARE_ACCOUNT_ID, undefined);
  }
});

test('deployment cannot override database environment or config through trailing Wrangler flags', async t => {
  for (const deployArgs of [['--env', 'development'], ['--config', 'other.json'], ['--name', 'other-worker']]) {
    const f = fixture(t);
    await assert.rejects(deployManaged('deploy', { ...f.options, deployArgs }), /Unsupported/);
    assert.equal(f.events.length, 0);
  }
});

test('database failures report safe stage and HTTP status without leaking provider or exception contents', async t => {
  for (const scenario of ['bad-request', 'denied', 'create-denied', 'create-unknown', 'invalid-json', 'invalid-identity']) {
    const f = fixture(t);
    const methods = [];
    f.options.request = async (_, init) => {
      methods.push(init.method);
      if (scenario.startsWith('create-') && init.method === 'GET') return Response.json({ success: false }, { status: 404 });
      if (scenario === 'create-unknown') throw Error('synthetic-secret raw network details');
      if (scenario === 'invalid-json') return new Response('synthetic-secret invalid JSON');
      if (scenario === 'invalid-identity') return Response.json({ success: true, result: { name: 'synthetic-secret', uuid: f.database.uuid } });
      return Response.json({ success: false, errors: [{ code: 10000, message: 'synthetic-secret provider details' }] }, { status: scenario === 'bad-request' ? 400 : 403 });
    };
    const expected = {
      'bad-request': 'stage=lookup-response, HTTP=400',
      denied: 'stage=lookup-response, HTTP=403',
      'create-denied': 'stage=create-response, HTTP=403',
      'create-unknown': 'stage=create-request, HTTP=unavailable',
      'invalid-json': 'stage=lookup-response, HTTP=200',
      'invalid-identity': 'stage=lookup-validation, HTTP=200',
    }[scenario];
    await assert.rejects(deployManaged('deploy', f.options), error => {
      assert.ok(error.message.includes(expected), error.message);
      assert.ok(!String(error.stack).includes('synthetic-secret'));
      assert.equal(error.cause, undefined);
      return true;
    });
    assert.deepEqual(methods, scenario.startsWith('create-') ? ['GET', 'POST'] : ['GET']);
    assert.equal(f.generated.length, 0);
  }
});
