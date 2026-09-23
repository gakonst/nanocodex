import assert from 'node:assert/strict';
import test from 'node:test';
import { EventEmitter } from 'node:events';
import { existsSync, writeFileSync, readFileSync, statSync } from 'node:fs';
import { dirname } from 'node:path';
import { releaseWorkers, releasePhases, guardedCommand, accountHealth } from './release-workers.mjs';
function fixture(selected, overrides = {}) {
  const events = [], calls = [];
  const plan = { revision: 'b'.repeat(40), selected, fingerprints: Object.fromEntries(selected.map(name => [name, 'a'.repeat(64)])) };
  const options = {
    env: { DEPLOY_MESSAGE: 'spaces "quotes" $HOME `literal` $(literal)' },
    ledger: { async start(name, fingerprint) { assert.equal(fingerprint, plan.fingerprints[name]); events.push(['start', name]); return name; }, async finish(name, state) { events.push([state, name]); } },
    isCurrent: async () => true,
    run: async (command, options) => { calls.push({ command, options }); return true; },
    health: async () => events.push(['health']), ...overrides,
  };
  return { plan, options, events, calls, release: () => releaseWorkers(plan, options) };
}

test('all selected Workers preserve dependency barriers, literal arguments and account-last health', async () => {
  const f = fixture(releasePhases.flat());
  const results = await f.release();
  assert.equal(results.length, 10); assert.ok(results.every(row => row.state === 'success'));
  for (let i = 1; i < releasePhases.length; i++) {
    const previous = releasePhases[i - 1].map(name => f.events.findIndex(row => row[0] === 'success' && row[1] === name));
    const next = releasePhases[i].map(name => f.events.findIndex(row => row[0] === 'start' && row[1] === name));
    assert.ok(Math.max(...previous) < Math.min(...next));
  }
  for (const { command } of f.calls) {
    assert.equal(command[command.indexOf('--message') + 1], f.options.env.DEPLOY_MESSAGE);
    assert.equal(command[command.indexOf('--tag') + 1], `nc-ci-${'a'.repeat(64)}`);
  }
  assert.equal(f.events.filter(row => row[0] === 'health').length, releasePhases.length);
  assert.equal(f.calls.filter(({ command }) => command.includes('--env=')).length, 3);
  assert.equal(f.calls.at(-1).options.directory, 'js/account');
  const account = f.calls.at(-1).command;
  assert.equal(account[account.indexOf('--config') + 1], 'dist/nanocodex/wrangler.ci.json');
  assert.equal(account[account.indexOf('--var') + 1], `DEPLOYMENT_SHA:${f.plan.revision}`);
  assert.deepEqual(f.events.slice(-2), [['health'], ['success', 'account']]);
});

test('phase failure waits for siblings, records failure, and blocks dependent deployments', async () => {
  const f = fixture(['egress', 'x', 'managed', 'account']);
  f.options.run = async (_, { directory }) => { if (directory === 'js/egress') throw Error('deploy failed'); await new Promise(resolve => setImmediate(resolve)); return true; };
  await assert.rejects(f.release(), /Release phase failed/);
  assert.ok(f.events.some(row => row[0] === 'failure' && row[1] === 'egress'));
  assert.ok(f.events.some(row => row[0] === 'success' && row[1] === 'x'));
  assert.ok(!f.events.some(row => ['managed', 'account'].includes(row[1])));
});

test('supersession before deployment avoids ledger writes and guarded skips cannot become successes', async () => {
  const f = fixture(['x'], { isCurrent: async () => false });
  assert.equal((await f.release())[0].state, 'superseded'); assert.equal(f.calls.length, 0);
  assert.ok(!f.events.some(row => row[0] === 'start'));
  const skipped = fixture(['account'], { run: async () => false });
  assert.equal((await skipped.release())[0].state, 'superseded');
  assert.deepEqual(skipped.events, [['start', 'account'], ['inactive', 'account']]);
});

test('Astra secrets use a private temporary file in the single tagged guarded deploy', async () => {
  const env = { DEPLOY_MESSAGE: 'release', ASTRA_MANAGED_API_KEY: 'synthetic "key"', ASTRA_MPP_SECRET: 'synthetic-secret', TEMPO_API_KEY: '' };
  for (const active of [true, false]) {
    const f = fixture(['astra'], { env }); let path;
    f.options.run = async (command, options) => {
      f.calls.push({ command, options });
      assert.ok(command.includes('deploy')); assert.ok(!command.includes('bulk'));
      assert.equal(command[command.indexOf('--tag') + 1], `nc-ci-${'a'.repeat(64)}`);
      path = command[command.indexOf('--secrets-file') + 1];
      assert.equal(statSync(path).mode & 0o777, 0o600);
      assert.equal(statSync(dirname(path)).mode & 0o777, 0o700);
      assert.deepEqual(JSON.parse(readFileSync(path, 'utf8')), { NANOCODEX_ASTRA_MANAGED_API_KEY: env.ASTRA_MANAGED_API_KEY, NANOCODEX_ASTRA_MPP_SECRET: env.ASTRA_MPP_SECRET });
      for (const key of ['ASTRA_MANAGED_API_KEY', 'ASTRA_MPP_SECRET', 'TEMPO_API_KEY']) assert.ok(!Object.hasOwn(options.env, key));
      for (const value of [env.ASTRA_MANAGED_API_KEY, env.ASTRA_MPP_SECRET]) assert.ok(!command.some(arg => arg.includes(value)));
      return active;
    };
    assert.equal((await f.release())[0].state, active ? 'success' : 'superseded');
    assert.equal(f.calls.length, 1); assert.equal(existsSync(dirname(path)), false);
    assert.equal(f.events.filter(row => row[0] === 'health').length, active ? 1 : 0);
  }
  assert.equal(env.ASTRA_MPP_SECRET, 'synthetic-secret');
  const omitted = fixture(['astra']); await omitted.release();
  assert.equal(omitted.calls.length, 1); assert.ok(!omitted.calls[0].command.includes('--secrets-file'));
});

test('health failures cannot certify account, managed, or any API-only phase', async () => {
  for (const selected of [['account'], ['managed'], ['x'], ['egress', 'x'], ['email', 'astra']]) {
    const f = fixture(selected, { health: async () => { f.events.push(['health']); throw Error('unhealthy'); } });
    await assert.rejects(f.release(), /Release phase failed/);
    assert.equal(f.events.filter(row => row[0] === 'health').length, 1);
    assert.equal(f.events.filter(row => row[0] === 'success').length, 0);
    assert.deepEqual(f.events.filter(row => row[0] === 'failure').map(row => row[1]).sort(), [...selected].sort());
  }
  const empty = fixture([]); assert.deepEqual(await empty.release(), []); assert.deepEqual(empty.events, []);
  const x = fixture(['x']); await x.release(); assert.deepEqual(x.events.slice(-2), [['health'], ['success', 'x']]);
});

test('live receipt failure stops dependent Workers after health and records failure', async () => {
  const f = fixture(['managed', 'account']);
  f.options.ledger.finish = async (name, state) => {
    if (state === 'success') throw Error('live version mismatch');
    f.events.push([state, name]);
  };
  await assert.rejects(f.release(), /Release phase failed/);
  assert.deepEqual(f.events, [['start', 'managed'], ['health'], ['failure', 'managed']]);
});

test('guarded command wraps argv, forwards stdin, interprets status and cleans temporary output', async () => {
  for (const [status, active] of [[0, true], [0, false], [17, true]]) {
    let output, received;
    const promise = guardedCommand(['npx', 'wrangler', 'secret', 'bulk', '--env='], {
      cwd: '/checkout', directory: 'js/worker', env: { KEEP: 'value' }, input: '{"key":"value"}',
      launch(executable, args, options) {
        assert.equal(executable, process.execPath);
        assert.deepEqual(args, ['/checkout/scripts/cloudflare/current-production-release.mjs', '--', 'npx', 'wrangler', 'secret', 'bulk', '--env=']);
        assert.equal(options.cwd, '/checkout/js/worker'); assert.equal(options.env.KEEP, 'value');
        assert.deepEqual(options.stdio, ['pipe', 'inherit', 'inherit']);
        output = options.env.GITHUB_OUTPUT;
        const child = new EventEmitter(); child.stdin = new EventEmitter(); child.stdin.end = value => { received = value; };
        queueMicrotask(() => { writeFileSync(output, `active=${active}\n`); child.emit('close', status); });
        return child;
      },
    });
    if (status) await assert.rejects(promise, /Guarded Worker command failed/); else assert.equal(await promise, active);
    assert.equal(received, '{"key":"value"}'); assert.equal(existsSync(dirname(output)), false);
  }
});

test('secret publication failure records Astra failure and blocks account', async () => {
  const f = fixture(['astra', 'account'], { env: { TEMPO_API_KEY: 'synthetic-tempo' } });
  let path;
  f.options.run = async command => {
    path = command[command.indexOf('--secrets-file') + 1];
    assert.ok(existsSync(path)); throw Error('secret publication failed');
  };
  await assert.rejects(f.release(), /Release phase failed/);
  assert.deepEqual(f.events, [['start', 'astra'], ['failure', 'astra']]);
  assert.equal(existsSync(dirname(path)), false);
});

test('guard launch errors reject and remove temporary output directory', async () => {
  let output;
  await assert.rejects(guardedCommand(['deploy'], {
    launch(_, __, options) {
      output = options.env.GITHUB_OUTPUT;
      assert.equal(options.stdio[0], 'inherit');
      const child = new EventEmitter();
      queueMicrotask(() => child.emit('error', Error('launch failed')));
      return child;
    },
  }), /launch failed/);
  assert.equal(existsSync(dirname(output)), false);
});


test('account health validates HTTP status and application identity', async t => {
  const healthy = { service: 'nanocodex', runtime: 'cloudflare-workers', status: 'ok' };
  let response = { status: 200, json: async () => healthy };
  t.mock.method(globalThis, 'fetch', async (url, options) => {
    assert.equal(url, 'https://nanocodex.gakonst.workers.dev/api/health');
    assert.ok(options.signal instanceof AbortSignal);
    return response;
  });
  await accountHealth();
  response = { status: 200, json: async () => ({ ...healthy, deployment_sha: 'b'.repeat(40) }) };
  await accountHealth('b'.repeat(40));
  await assert.rejects(accountHealth('c'.repeat(40)), /released revision/);
  response = { status: 503, json: async () => healthy };
  await assert.rejects(accountHealth());
  for (const key of ['service', 'runtime', 'status']) {
    response = { status: 200, json: async () => ({ ...healthy, [key]: 'wrong' }) };
    await assert.rejects(accountHealth());
  }
});


test('account phase health receives the revision before certifying its release', async () => {
  const f = fixture(['managed', 'account']);
  const revisions = [];
  f.options.health = async revision => { revisions.push(revision); };
  await f.release();
  assert.deepEqual(revisions, [undefined, f.plan.revision]);
});
