import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import { Miniflare } from 'miniflare';
import WebSocket from 'ws';
import { createTools } from 'nanocodex/tools';
import { createAttachment } from 'nanocodex-tools/attachment';
import { createNodeProcessTools } from 'nanocodex-tools/node';
import { fetch } from "./support/miniflare-fetch.mjs";

// One persistent native Hand keeps its command-recovery generation across
// reconnects, so its call ledger grows with lifetime use. Transport loss and
// owner restart must stay bounded by in-flight work, not that history.
// Real public HTTP, a real reverse WebSocket publisher, workerd SQLite and
// /bin/sh. The isolate heap is capped like a production Worker isolate.
// NANOCODEX_LEDGER_BASELINE=<dir> swaps in archived broker sources only.
const root = fileURLToPath(new URL('../../../', import.meta.url));
const label = process.env.NANOCODEX_LEDGER_LABEL ?? 'candidate';
assert.match(label, /^[a-zA-Z0-9_.-]+$/);
const baseline = process.env.NANOCODEX_LEDGER_BASELINE;
const heapMb = Number(process.env.NANOCODEX_LEDGER_HEAP_MB ?? 64);
const calls = Number(process.env.NANOCODEX_LEDGER_CALLS ?? 900);
const outputBytes = Number(process.env.NANOCODEX_LEDGER_OUTPUT_BYTES ?? 36_000);
const owner = '00000000-0000-4000-8000-000000000071';
const credential = 'Bearer synthetic-ledger-memory';
const machineId = 'ledger-hand-01';
const archived = ['js/managed/src/hosted-tools-broker.ts', 'js/nanocodex-tools/src/hosted/broker-core.ts'];
const source = `
import { AccountHostedTools } from './src/account-hosted-tools.ts';
export { AccountHostedTools };
const info=console.info.bind(console);
console.info=(record,...rest)=>info(record&&typeof record==='object'?JSON.stringify(record):record,...rest);
export default { fetch(request,env) {
 if(request.headers.get('authorization')!=='${credential}')return new Response(null,{status:401});
 return env.ACCOUNT.getByName('${owner}').fetch(request);
}};
`;
const freePort = () => new Promise((done, fail) => {
  const server = createServer().once('error', fail).listen(0, '127.0.0.1', () => {
    const { port } = server.address(); server.close(() => done(port));
  });
});
const sleep = ms => new Promise(done => setTimeout(done, ms));

test('a persistent Hand with a large call ledger survives transport loss and owner restart under a capped heap', { timeout: 300_000 }, async () => {
  const output = join(root, 'output/hand-ledger-memory', label, `${Date.now()}-${process.pid}`);
  await mkdir(output, { recursive: true });
  const http = [], lines = [], phases = [], checks = {};
  let mf, failure, publisher, socket, tools, native, workspace, sourceHashes, bundle;
  const port = await freePort();
  const base = `http://127.0.0.1:${port}`;
  const previousFlags = process.env.MINIFLARE_WORKERD_V8_FLAGS;
  const capture = line => lines.push(`${new Date().toISOString()} ${line}`);
  const request = async (path, body) => {
    const start = performance.now();
    try {
      const response = await fetch(new URL(path, base), { method: 'POST',
        headers: { authorization: credential, 'content-type': 'application/json' },
        body: JSON.stringify(body), signal: AbortSignal.timeout(20_000) });
      const value = await response.json();
      const row = { path, status: response.status, elapsed_ms: performance.now() - start };
      if (path !== '/invoke' || response.status !== 200 || !value.success) row.value = value;
      http.push(row);
      return { status: response.status, value };
    } catch (error) {
      http.push({ path, error: String(error?.cause?.code ?? error?.message ?? error), elapsed_ms: performance.now() - start });
      return { status: 0, error };
    }
  };
  const invoke = (route, call, cmd) => request('/invoke', { owner_id: owner, machine_id: machineId, name: 'exec_command',
    route_token: route, session_id: 'ledger-session', thread_id: 'ledger-thread', call_id: call, model: 'fixture',
    input: { cmd, shell: '/bin/sh', login: false, yield_time_ms: 2000 } });
  const effect = call => `printf '%s\\n' '${call}' >> effects.log; printf '%s' '${call}'`;
  const effects = async () => (await readFile(join(workspace, 'effects.log'), 'utf8').catch(() => '')).trim().split('\n').filter(Boolean);
  const hand = async () => {
    const snapshot = await request('/snapshot', { owner_id: owner });
    return snapshot.status === 200 ? snapshot.value.machines.find(row => row.machine.id === machineId) : undefined;
  };
  // A reset broker cannot report the Hand; transport recovery must finish well
  // inside the attachment's bounded reconnect backoff.
  const waitOnline = async (phase, deadlineMs = 20_000) => {
    const started = performance.now();
    while (performance.now() - started < deadlineMs) {
      const row = await hand();
      if (row?.online) { phases.push({ phase, online_after_ms: Math.round(performance.now() - started) }); return row; }
      await sleep(100);
    }
    const crashed = lines.filter(line => /out of memory|OOM|heap limit|Fatal|crash|exceeded/i.test(line)).slice(-5);
    phases.push({ phase, online_after_ms: null, crash_lines: crashed });
    assert.fail(`${phase}: Hand did not return online within ${deadlineMs}ms; broker reset evidence: ${JSON.stringify(crashed)}`);
  };
  const start = async () => {
    mf = new Miniflare({ port, host: '127.0.0.1', modules: true, script: bundle, compatibilityDate: '2026-07-30',
      compatibilityFlags: ['nodejs_compat', 'enable_request_signal'],
      durableObjects: { ACCOUNT: { className: 'AccountHostedTools', useSQLite: true } },
      durableObjectsPersist: join(output, 'sqlite'), handleRuntimeStdio(stdout, stderr) {
        createInterface({ input: stdout }).on('line', capture); createInterface({ input: stderr }).on('line', capture);
      } });
    await mf.ready;
  };
  try {
    const overrides = new Map();
    for (const path of archived) {
      overrides.set(join(root, path), await readFile(baseline ? join(resolve(baseline), path.split('/').at(-1)) : join(root, path), 'utf8'));
    }
    sourceHashes = Object.fromEntries([...overrides].map(([path, contents]) => [path, createHash('sha256').update(contents).digest('hex')]));
    const built = await build({ stdin: { contents: source, resolveDir: join(root, 'js/managed') }, bundle: true, write: false,
      format: 'esm', platform: 'node', conditions: ['workerd'], target: 'es2022', external: ['cloudflare:*', 'node:*'],
      alias: { 'nanocodex-tools/hosted': join(root, 'js/nanocodex-tools/src/hosted/index.ts'),
        'nanocodex-tools': join(root, 'js/nanocodex-tools/src/index.ts'),
        'node-rsa': join(root, 'js/nanocodex/tools/browser/unsupportedNodeRsa.mjs') },
      plugins: [{ name: 'archived-baseline', setup(builder) { builder.onLoad({ filter: /\/(broker-core|hosted-tools-broker)\.ts$/ }, args =>
        overrides.has(args.path) ? { contents: overrides.get(args.path), loader: 'ts', resolveDir: args.path.slice(0, args.path.lastIndexOf('/')) } : undefined); } }],
      logLevel: 'silent' });
    bundle = built.outputFiles[0].text;
    await writeFile(join(output, 'worker.mjs'), bundle);
    for (const [path, contents] of overrides) await writeFile(join(output, path.split('/').at(-1)), contents);
    process.env.MINIFLARE_WORKERD_V8_FLAGS = `--max-old-space-size=${heapMb}`;
    await start();

    workspace = join(output, machineId); await mkdir(workspace, { recursive: true });
    native = await createNodeProcessTools({ workspace });
    const machines = [{ id: machineId, name: 'Synthetic persistent Hand', workspace, capabilities: ['shell'] }];
    tools = await createTools({ attachmentId: machineId, machines, tools: Object.fromEntries(native.tools.map(tool => [tool.name, tool])) });
    const endpoint = `${base.replace(/^http/, 'ws')}/tool-host`;
    publisher = createAttachment(tools, { endpoint, transport: { connect() {
      socket = new WebSocket(endpoint, { headers: { authorization: credential, 'x-nanocodex-owner-id': owner } });
      return socket;
    } } }, { reconnectDelayMs: 200, attachmentId: machineId, machines });
    assert.equal((await publisher.connect()).connected, true);
    const initial = await waitOnline('initial');
    const route = initial.tools.find(tool => tool.name === 'exec_command').route_token;

    // Lifetime use: completed calls with realistic bounded shell output.
    const fillStarted = performance.now();
    let next = 0, completed = 0, retainedBytes = 0;
    await Promise.all(Array.from({ length: 8 }, async () => {
      while (next < calls) {
        const call = `history-${next++}`;
        const result = await invoke(route, call, `head -c ${outputBytes} /dev/zero | tr '\\0' 'h'; printf '%s' '${call}'`);
        assert.equal(result.status, 200, JSON.stringify(result.value ?? String(result.error)));
        assert.equal(result.value.success, true, JSON.stringify(result.value));
        assert.ok(result.value.structured_result.output.endsWith(call));
        retainedBytes += JSON.stringify(result.value).length; completed++;
      }
    }));
    phases.push({ phase: 'history', calls: completed, approx_result_bytes: retainedBytes, elapsed_ms: Math.round(performance.now() - fillStarted) });
    const marker = await invoke(route, 'marker-once', effect('marker-once'));
    assert.equal(marker.value?.structured_result?.output, 'marker-once');

    // Transport loss without a close frame: the CLI reconnects to the same
    // runtime and catalog, so the broker must resume the same generation.
    socket.terminate();
    const resumed = await waitOnline('transport_loss');
    assert.equal(resumed.tools.find(tool => tool.name === 'exec_command').route_token, route, 'same runtime resumes its generation');
    const afterLoss = await invoke(route, 'after-transport-loss', effect('after-transport-loss'));
    assert.equal(afterLoss.status, 200, JSON.stringify(afterLoss.value ?? String(afterLoss.error)));
    assert.equal(afterLoss.value.structured_result.output, 'after-transport-loss');
    checks.transport_loss = { online: true, same_generation: true, executes: true };

    // Owner restart: a fresh isolate opens the same durable ledger.
    await mf.dispose(); mf = undefined;
    await start();
    const restarted = await waitOnline('owner_restart', 30_000);
    const restartedRoute = restarted.tools.find(tool => tool.name === 'exec_command').route_token;
    const replay = await invoke(route, 'marker-once', effect('marker-once'));
    assert.equal(replay.status, 200, JSON.stringify(replay.value ?? String(replay.error)));
    assert.equal(replay.value.structured_result.output, 'marker-once', 'durable replay returns the stored outcome');
    const afterRestart = await invoke(restartedRoute, 'after-owner-restart', effect('after-owner-restart'));
    assert.equal(afterRestart.status, 200, JSON.stringify(afterRestart.value ?? String(afterRestart.error)));
    assert.equal(afterRestart.value.structured_result.output, 'after-owner-restart');
    assert.deepEqual(await effects(), ['marker-once', 'after-transport-loss', 'after-owner-restart'], 'each effect executes exactly once');
    // Unauthorized owners still see nothing after recovery.
    assert.equal((await request('/snapshot', { owner_id: '00000000-0000-4000-8000-000000000072' })).status, 404);
    checks.owner_restart = { online: true, replay_once: true, executes: true, other_owner: 404, route_retained: restartedRoute === route };
    console.log(JSON.stringify({ evidence: output, label, heap_mb: heapMb, phases, checks }));
  } catch (error) { failure = error; throw error; }
  finally {
    await publisher?.close(); await tools?.close(); await native?.close();
    await mf?.dispose();
    if (previousFlags === undefined) delete process.env.MINIFLARE_WORKERD_V8_FLAGS;
    else process.env.MINIFLARE_WORKERD_V8_FLAGS = previousFlags;
    await writeFile(join(output, 'http.json'), JSON.stringify(http, null, 2));
    await writeFile(join(output, 'runtime.log'), lines.join('\n') + '\n');
    await writeFile(join(output, 'result.json'), JSON.stringify({ label, baseline, heap_mb: heapMb, calls, output_bytes: outputBytes,
      sourceHashes, phases, checks, error: failure?.stack }, null, 2));
    await writeFile(join(output, 'README.md'), `Run: NANOCODEX_LEDGER_LABEL=${label} ${baseline ? `NANOCODEX_LEDGER_BASELINE=${baseline} ` : ''}NANOCODEX_LEDGER_HEAP_MB=${heapMb} NANOCODEX_LEDGER_CALLS=${calls} node --test test/hand-ledger-memory-journey.test.mjs (from js/managed)\n\nExpected: one real reverse-WebSocket native Hand completes ${calls} real /bin/sh calls (~${outputBytes} B output each) through public /invoke; after an abrupt transport loss and after an owner (workerd) restart, the Hand is online again, resumes its generation, replays a completed call without re-execution and executes new calls exactly once, with the isolate heap capped at ${heapMb} MB.\n\nObserved phases: ${JSON.stringify(phases)}\nChecks: ${JSON.stringify(checks)}\nStatus: ${failure ? 'FAIL ' + failure.message : 'PASS'}\n\nhttp.json (public requests), runtime.log (workerd stdio incl. any OOM), result.json, worker.mjs, archived broker sources and persisted sqlite are inspectable.\n`);
  }
});
