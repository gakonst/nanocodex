import assert from 'node:assert/strict';
import test from 'node:test';
import { EventEmitter } from 'node:events';
import { existsSync, writeFileSync, readFileSync, statSync, mkdtempSync, rmSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { DeploymentLedgerError } from './deployment-ledger.mjs';
import { releaseWorkers, releasePhases, prepareReleasePhase, guardedCommand, accountHealth, AccountHealthError, localWrangler } from './release-workers.mjs';
function fixture(selected, overrides = {}) {
  const events = [], calls = [];
  const plan = { revision: 'b'.repeat(40), selected, fingerprints: Object.fromEntries(selected.map(name => [name, 'a'.repeat(64)])) };
  const options = {
    env: { DEPLOY_MESSAGE: 'spaces "quotes" $HOME `literal` $(literal)' },
    ledger: { async start(name, fingerprint) { assert.equal(fingerprint, plan.fingerprints[name]); events.push(['start', name]); return name; }, async finish(name, state) { events.push([state, name]); } },
    isCurrent: async () => true,
    prepare: async () => {},
    run: async (command, options) => { calls.push({ command, options }); return true; },
    health: async () => events.push(['health']), ...overrides,
  };
  return { plan, options, events, calls, release: () => releaseWorkers(plan, options) };
}

test('all selected Workers preserve dependency barriers, literal arguments and account-last health', async () => {
  const f = fixture(releasePhases.flat());
  const results = await f.release();
  assert.equal(results.length, 12); assert.ok(results.every(row => row.state === 'success'));
  for (let i = 1; i < releasePhases.length; i++) {
    const previous = releasePhases[i - 1].map(name => f.events.findIndex(row => row[0] === 'success' && row[1] === name));
    const next = releasePhases[i].map(name => f.events.findIndex(row => row[0] === 'start' && row[1] === name));
    assert.ok(Math.max(...previous) < Math.min(...next));
  }
  assert.ok(f.events.findIndex(row => row[0] === 'success' && row[1] === 'managed')
    < f.events.findIndex(row => row[0] === 'start' && row[1] === 'egress'),
  'publish the private PhoneProvider entry point before the egress binding');
  for (const { command } of f.calls) {
    assert.equal(command[command.indexOf('--message') + 1], f.options.env.DEPLOY_MESSAGE);
    assert.equal(command[command.indexOf('--tag') + 1], `nc-ci-${'a'.repeat(64)}`);
  }
  assert.equal(f.events.filter(row => row[0] === 'health').length, releasePhases.length);
  assert.equal(f.calls.filter(({ command }) => command.includes('--env=')).length, 3);
  assert.equal(f.calls.find(({ options }) => options.directory === 'js/media').command.slice(0, 6).join(' '), 'npx wrangler deploy --config wrangler.jsonc --message');
  assert.equal(f.calls.at(-1).options.directory, 'js/account');
  const account = f.calls.at(-1).command;
  assert.equal(account[account.indexOf('--config') + 1], 'dist/nanocodex/wrangler.ci.json');
  assert.equal(account[account.indexOf('--var') + 1], `DEPLOYMENT_SHA:${f.plan.revision}`);
  assert.deepEqual(f.events.slice(-2), [['health'], ['success', 'account']]);
});

test('phase failure waits for siblings, records failure, and blocks dependent deployments', async () => {
  const f = fixture(['email', 'dialog', 'account']);
  f.options.run = async (_, { directory }) => { if (directory === 'js/email') throw Error('deploy failed'); await new Promise(resolve => setImmediate(resolve)); return true; };
  await assert.rejects(f.release(), /Release phase failed/);
  assert.ok(f.events.some(row => row[0] === 'failure' && row[1] === 'email'));
  assert.ok(f.events.some(row => row[0] === 'success' && row[1] === 'dialog'));
  assert.ok(!f.events.some(row => row[1] === 'account'));
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
  for (const selected of [['account'], ['managed'], ['x'], ['egress'], ['email', 'astra']]) {
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

test('managed uploads before unrelated Astra and account builds without duplicate targets', async () => {
  const f = fixture(['egress', 'managed', 'astra', 'account']);
  const targets = [];
  f.options.prepare = async (plan, options) => prepareReleasePhase(plan, {
    ...options,
    run(command, args) {
      f.events.push(['build', args.join(' ')]);
      if (command === 'node_modules/.bin/turbo') targets.push(...args.filter((_, i) => args[i - 1] === '--filter'));
    },
    managed: async () => f.events.push(['config', 'managed']),
    account: async () => f.events.push(['config', 'account']),
  });
  await f.release();
  const managed = f.events.findIndex(row => row[0] === 'success' && row[1] === 'managed');
  const astraInstall = f.events.findIndex(row => row[0] === 'build' && row[1].startsWith('ci --prefix examples/astra-mpp-trial'));
  const astra = f.events.findIndex(row => row[0] === 'build' && row[1].includes('build:client'));
  const account = f.events.findIndex(row => row[0] === 'build' && row[1] === 'build js/account');
  assert.ok(managed >= 0 && managed < astraInstall && astraInstall < astra && astra < account);
  assert.ok(account < f.events.findIndex(row => row[0] === 'config' && row[1] === 'account'));
  assert.equal(new Set(targets).size, targets.length);
  assert.deepEqual(f.events.slice(-2), [['health'], ['success', 'account']]);
});

test('preparation failure preserves earlier releases and blocks dependent uploads', async () => {
  const f = fixture(['egress', 'managed', 'account']), prepared = [];
  f.options.prepare = async plan => {
    prepared.push(...plan.selected);
    if (plan.selected.includes('egress')) throw Error('synthetic build failure');
  };
  await assert.rejects(f.release(), /phase preparation failed/);
  assert.deepEqual(prepared, ['managed', 'egress']);
  assert.deepEqual(f.events, [['start', 'managed'], ['health'], ['success', 'managed']]);
});

test('superseded phases skip compilation and freshness is rechecked after building', async () => {
  const f = fixture(['managed', 'account'], {
    isCurrent: async () => false,
    prepare: async () => { throw Error('must not build'); },
  });
  assert.ok((await f.release()).every(row => row.state === 'superseded'));
  assert.deepEqual(f.events, []);
  let checks = 0;
  const prepared = [];
  const later = fixture(['managed', 'account'], {
    isCurrent: async () => ++checks === 1,
    prepare: async plan => prepared.push(...plan.selected),
  });
  assert.ok((await later.release()).every(row => row.state === 'superseded'));
  assert.deepEqual(prepared, ['managed']);
  assert.deepEqual(later.events, []);
});

test('phase builds strip deployment-only secrets and configure images after their build', async () => {
  const env = { ASTRA_MANAGED_API_KEY: 'synthetic-api', ASTRA_MPP_SECRET: 'synthetic-mpp', TEMPO_API_KEY: 'synthetic-tempo',
    KEEP: 'value', RELEASE_ONLY: 'managed,account', CLOUDFLARE_ACCOUNT_ID: 'a'.repeat(32), GITHUB_REPOSITORY: 'fixture/repo' };
  const calls = [], configs = [];
  await prepareReleasePhase({ selected: ['managed', 'account'] }, {
    cwd: '/synthetic-checkout', env,
    run(command, args, options) {
      calls.push([command, args]);
      assert.equal(options.cwd, '/synthetic-checkout');
      assert.equal(options.env.KEEP, 'value');
      for (const key of ['ASTRA_MANAGED_API_KEY', 'ASTRA_MPP_SECRET', 'TEMPO_API_KEY']) assert.ok(!Object.hasOwn(options.env, key));
    },
    managed: async options => { assert.equal(options.requireCurrent, true); configs.push('managed'); },
    account: async () => { assert.ok(calls.some(([, args]) => args.includes('js/account'))); configs.push('account'); },
  });
  assert.deepEqual(configs, ['managed', 'account']);
  assert.equal(env.ASTRA_MANAGED_API_KEY, 'synthetic-api');
});


test('post-build input verification rejects changed inputs before publishing that phase', async () => {
  const f = fixture(['managed', 'account']);
  const events = [];
  f.options.prepare = async plan => events.push(['prepare', ...plan.selected]);
  f.options.verify = async () => { events.push(['verify']); throw Error('inputs changed'); };
  await assert.rejects(f.release(), /phase preparation failed/);
  assert.deepEqual(events, [['prepare', 'managed'], ['verify']]);
  assert.deepEqual(f.events, []);
});


test('successful WASM builds remain retainable after a later phase fails or is superseded', async t => {
  const directory = mkdtempSync(join(tmpdir(), 'release-wasm-'));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  for (const outcome of ['failure', 'superseded', 'never-built']) {
    const output = join(directory, outcome);
    let checks = 0;
    const f = fixture(['managed', 'account'], { env: { GITHUB_OUTPUT: output } });
    f.options.isCurrent = async () => outcome === 'never-built' ? false : outcome === 'superseded' ? ++checks === 1 : true;
    f.options.prepare = async (plan, { completedTargets }) => {
      if (plan.selected.includes('account')) throw Error('later build failed');
      completedTargets.add('nanocodex');
    };
    if (outcome === 'failure') await assert.rejects(f.release(), /preparation failed/);
    else await f.release();
    assert.equal(readFileSync(output, 'utf8'), `wasm-built=${outcome !== 'never-built'}\n`);
  }
});

test('media must pass its own health and ledger phase before managed starts', async () => {
  const f = fixture(['media', 'managed']);
  f.options.health = async () => {
    f.events.push(['health']);
    if (!f.events.some(row => row[0] === 'success' && row[1] === 'media')) throw Error('media not healthy');
  };
  // The first phase fails; managed is never started.
  await assert.rejects(f.release(), /Release phase failed/);
  assert.deepEqual(f.events, [['start', 'media'], ['health'], ['failure', 'media']]);
  const success = fixture(['media', 'managed']);
  await success.release();
  assert.ok(success.events.findIndex(row => row[0] === 'success' && row[1] === 'media') <
    success.events.findIndex(row => row[0] === 'start' && row[1] === 'managed'));
});

test('managed releases enter the CRM migration/upload boundary with their pinned image config', async () => {
  const f = fixture(['managed']);
  await f.release();
  assert.deepEqual(f.calls[0].command.slice(0, 5), [process.execPath, '../../scripts/cloudflare/managed-crm.mjs', 'deploy', '--config', 'wrangler.ci.jsonc']);
  assert.ok(f.calls[0].command.includes('--containers-rollout'));
});


test('admission failure identifies Worker and GitHub status without leaking provider output', async () => {
  const f = fixture(['managed', 'account']);
  f.options.ledger.start = async () => { throw new DeploymentLedgerError(422); };
  await assert.rejects(f.release(), /managed ledger admission: .*GitHub HTTP 422/);
  assert.equal(f.calls.length, 0);
  assert.deepEqual(f.events, []);
});

test('freshness and completion failures retain safe stage diagnostics and block dependencies', async () => {
  const freshness = fixture(['managed', 'account']);
  let checks = 0;
  freshness.options.isCurrent = async () => {
    if (++checks > 1) throw Error('synthetic-private-provider-output');
    return true;
  };
  await assert.rejects(freshness.release(), error => {
    assert.match(error.message, /managed freshness check/);
    assert.ok(!error.message.includes('synthetic-private'));
    assert.ok(!error.cause);
    return true;
  });
  assert.equal(freshness.calls.length, 0);
  const completion = fixture(['managed', 'account']);
  completion.options.ledger.finish = async (_, state) => {
    if (state === 'success') throw new DeploymentLedgerError(403);
  };
  await assert.rejects(completion.release(), /managed completion receipt: .*GitHub HTTP 403/);
  assert.equal(completion.calls.length, 1);
});

test('health classifications retain strict checks without echoing remote data', async () => {
  const valid={service:'nanocodex',runtime:'cloudflare-workers',status:'ok',deployment_sha:'b'.repeat(40)};
  const cases=[
    [new Response('SECRET_BODY',{status:503}),'http_status'],
    [new Response('SECRET_INVALID_JSON'),'invalid_json'],
    [Response.json(null),'invalid_shape'],
    [Response.json({...valid,service:'SECRET_SERVICE'}),'service_mismatch'],
    [Response.json({...valid,runtime:'SECRET_RUNTIME'}),'runtime_mismatch'],
    [Response.json({...valid,status:'SECRET_STATUS'}),'status_mismatch'],
    [Response.json({...valid,deployment_sha:null}),'revision_missing'],
    [Response.json({...valid,deployment_sha:'SECRET_REVISION'}),'revision_mismatch'],
  ];
  for(const [response,category] of cases){
    await assert.rejects(accountHealth(valid.deployment_sha,{request:async()=>response}),error=>{
      assert.equal(error.category,category);assert.ok(!error.message.includes('SECRET'));return true;
    });
  }
  for(const name of ['TimeoutError','Error']){
    await assert.rejects(accountHealth(undefined,{request:async()=>{throw Object.assign(new Error('SECRET_NETWORK'),{name});}}),error=>{
      assert.equal(error.category,name==='TimeoutError'?'timeout':'network');assert.ok(!error.message.includes('SECRET'));return true;
    });
  }
  await accountHealth(valid.deployment_sha,{request:async()=>Response.json(valid)});
});

test('health annotation uses private fixed detail even after public fields change', async () => {
  const error=new AccountHealthError('http_status',503);
  error.message='SECRET_MESSAGE';error.category='SECRET_CATEGORY';error.httpStatus='SECRET_STATUS';
  const messages=[];const original=console.error;console.error=(line)=>messages.push(line);
  try{
    const f=fixture(['account'],{health:async()=>{throw error;}});
    await assert.rejects(f.release(),/Release phase failed/);
    assert.ok(f.events.some(row=>row[0]==='failure'&&row[1]==='account'));
    assert.ok(messages.some(line=>line.includes('HTTP 503')));
    assert.ok(messages.every(line=>!line.includes('SECRET')));
  }finally{console.error=original;}
});

test('health check classifies actual HTTP and stalled body failures', async () => {
  const {createServer}=await import('node:http');
  const server=createServer((req,res)=>{
    if(req.url==='/stall'){res.writeHead(200,{'content-type':'application/json'});res.write('{');return;}
    res.writeHead(502);res.end('SECRET_UPSTREAM_BODY');
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const origin=`http://127.0.0.1:${server.address().port}`;
  try{
    await assert.rejects(accountHealth(undefined,{url:origin,timeoutMs:1000}),e=>e.category==='http_status'&&e.httpStatus===502&&!e.message.includes('SECRET'));
    await assert.rejects(accountHealth(undefined,{url:origin+'/stall',timeoutMs:50}),e=>e.category==='timeout');
  }finally{server.closeAllConnections();await new Promise(resolve=>server.close(resolve));}
});

test('parallel plans upload every selected Worker together with one health check and record topology', async () => {
  const f = fixture(['media', 'managed', 'egress', 'account']);
  f.plan.parallel = true; f.plan.topology = 'c'.repeat(64);
  const topologies = [];
  const start = f.options.ledger.start;
  f.options.ledger.start = async (name, fingerprint, options) => { topologies.push(options?.topology); return start(name, fingerprint, options); };
  await f.release();
  const starts = f.events.map((row, i) => [row, i]).filter(([row]) => row[0] === 'start').map(([, i]) => i);
  const firstSuccess = f.events.findIndex(row => row[0] === 'success');
  assert.equal(starts.length, 4);
  assert.ok(starts.every(i => i < firstSuccess), 'all uploads start before any receipt');
  assert.equal(f.events.filter(row => row[0] === 'health').length, 1);
  assert.deepEqual(topologies, Array(4).fill('c'.repeat(64)));
});

test('parallel plans skip container comparisons; ordered plans keep immediate rollouts', async () => {
  const command = (f, name) => f.calls.find(call => call.options.directory === (name === 'account' ? 'js/account' : 'js/managed')).command;
  const parallel = fixture(['managed', 'account']); parallel.plan.parallel = true; parallel.plan.topology = 'c'.repeat(64);
  await parallel.release();
  for (const name of ['managed', 'account']) {
    const args = command(parallel, name);
    assert.equal(args[args.indexOf('--containers-rollout') + 1], 'none', name);
  }
  const ordered = fixture(['managed', 'account']);
  await ordered.release();
  const managed = command(ordered, 'managed');
  assert.equal(managed[managed.indexOf('--containers-rollout') + 1], 'immediate');
  assert.ok(!command(ordered, 'account').includes('--containers-rollout'));
});

test('Wrangler commands use the package binary when installed', () => {
  assert.deepEqual(localWrangler(['npx', 'wrangler', 'deploy'], '/pkg', () => true), ['node_modules/.bin/wrangler', 'deploy']);
  assert.deepEqual(localWrangler(['npx', 'wrangler', 'deploy'], '/pkg', () => false), ['npx', 'wrangler', 'deploy']);
  assert.deepEqual(localWrangler(['node', 'script.mjs'], '/pkg', () => true), ['node', 'script.mjs']);
});
