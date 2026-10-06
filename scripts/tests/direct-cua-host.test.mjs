import { test } from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter, once } from 'node:events';
import { PassThrough } from 'node:stream';
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { mkdtemp, mkdir, rm, realpath, chmod, writeFile, readFile, readdir } from 'node:fs/promises';
import path from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';
import { readLines, checkManagedPolicy, policyReply, appConsent, configuration, runHost } from '../../crates/experimental/nanocodex-computer/src/direct-cua-host.mjs';

const hostUrl = new URL('../../crates/experimental/nanocodex-computer/src/direct-cua-host.mjs', import.meta.url);
const tick = () => new Promise(resolve => setImmediate(resolve));
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const rpc = (id, method, params) => ({ jsonrpc: '2.0', id, method, params });
const consent = overrides => rpc('approve', 'elicitation/create', { mode: 'form', requestedSchema: { type: 'object', properties: {}, required: [] }, _meta: { connector_id: 'computer-use', codex_approval_kind: 'mcp_tool_call', tool_name: 'click', tool_params: { app: 'com.apple.TextEdit' } }, ...overrides });
const missing = () => { throw Object.assign(new Error('missing'), { code: 'ENOENT' }); };
class Child extends EventEmitter {
  constructor() { super(); this.pid = undefined; this.exitCode = null; this.signalCode = null; this.stdin = new PassThrough(); this.stdout = new PassThrough(); this.stderr = new PassThrough(); }
  exit() { this.exitCode = 0; this.emit('exit', 0, null); }
}
async function root(t) {
  // Native Hands need not mount /brain. Resolve the OS temp directory's
  // symlinks before the host checks its private socket's ancestors.
  const base = await realpath(tmpdir());
  await mkdir(base, { recursive: true });
  const directory = await realpath(await mkdtemp(path.join(base, 'dcua-')));
  t.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}
async function fixture(t, options = {}) {
  const directory = await root(t), state = path.join(directory, 's');
  await mkdir(state, { mode: 0o700 });
  const input = new PassThrough(), output = new PassThrough(), children = [], invocations = [];
  const config = { app: '/synthetic/App.app', provider: '/synthetic/provider', policyHost: '/synthetic/policy', state, allowApps: true, env: { HOME: directory, PATH: '/usr/bin:/bin', CODEX_HOME: '/synthetic/account/home', CODEX_TOKEN: 'synthetic-not-a-secret', NODE_OPTIONS: '--inspect' } };
  const completed = runHost(config, { input, output, managedCheck: async () => {}, spawnChild(command, args, settings) {
    const child = new Child(); children.push(child); invocations.push({ command, args, settings }); options.onSpawn?.(child, settings, children.length); return child;
  }});
  while (!children.length) await tick();
  t.after(async () => { children.forEach(child => child.exit()); input.end(); await completed; });
  return { directory, state, input, output, children, invocations, completed };
}

test('framing preserves split UTF-8 messages and rejects malformed/truncated input', () => {
  const stream = new PassThrough(), values = []; let failures = 0;
  readLines(stream, v => values.push(v), () => failures++);
  const value = rpc('ü', 'ping'), bytes = Buffer.from(JSON.stringify(value) + '\n');
  const cut = bytes.indexOf(Buffer.from('ü')) + 1;
  stream.write(bytes.subarray(0, cut)); stream.write(bytes.subarray(cut));
  assert.deepEqual(values, [JSON.parse(JSON.stringify(value))]); stream.write('not-json\n'); assert.equal(failures, 1);
});
test('policy shim identifies its own host and exposes no auth/model/thread APIs', () => {
  assert.equal(policyReply(rpc(1, 'initialize'), true).result.userAgent, 'nanocodex-cua-policy-host/1');
  for (const method of ['account/read', 'thread/start', 'model/list']) assert.equal(policyReply(rpc(1, method), true).error.code, -32601);
  assert.equal(policyReply(rpc(1, 'config/read'), false).result.config.computer_use.default_app_access, 'deny');
  assert.deepEqual(policyReply(rpc(1, 'getAuthStatus', {includeToken:true}), true).result, {authMethod:null,authToken:null,requiresOpenaiAuth:false});
  assert.equal(policyReply(rpc(1, 'configRequirements/read'), true).result.requirements.computerUse.allowLockedComputerUse, false);
});
test('blanket app consent excludes audio, data forms, unknown connectors and no active JS', () => {
  assert.equal(appConsent(consent(), true, true).result.action, 'accept');
  const audio = consent(); audio.params._meta.tool_name = 'start_audio_recording';
  const data = consent({ requestedSchema: { type: 'object', properties: { password: { type: 'string' } } } });
  const unknown = consent(); unknown.params._meta.connector_id = 'other';
  const newOperation = consent(); newOperation.params._meta.tool_name = 'unknown_sdk_operation';
  for (const request of [audio, data, unknown, newOperation]) assert.equal(appConsent(request, true, true).result.action, 'decline');
  assert.equal(appConsent(consent(), false, true).result.action, 'decline');
  assert.equal(appConsent(consent(), true, false).result.action, 'decline');
});
test('observed native app-access methods retain blanket consent', () => {
  for (const tool of ['click', 'drag', 'get_app_state', 'paste', 'perform_secondary_action',
    'press_key', 'scroll', 'select_text', 'set_value', 'type_text']) {
    const request = consent(); request.params._meta.tool_name = tool;
    assert.equal(appConsent(request, true, true).result.action, 'accept', tool);
  }
});
test('configuration requires macOS and normalized absolute paths', () => {
  assert.throws(() => configuration({}, 'linux'), /macOS/);
  assert.throws(() => configuration({ NANOCODEX_CUA_NATIVE_APP: '/x/../y' }, 'darwin'), /normalized/);
});
test('managed policy presence and unreadable probes fail closed', async () => {
  await assert.rejects(checkManagedPolicy({ platform: 'linux', inspect: async () => ({}) }), /policy requires integration/);
  await assert.rejects(checkManagedPolicy({ platform: 'darwin', inspect: missing, run: async () => { throw Object.assign(new Error('permission'), { code: 1, stderr: 'permission denied' }); } }), /permission/);
  await checkManagedPolicy({ platform: 'darwin', inspect: missing, run: async () => { throw Object.assign(new Error('missing'), { code: 1, stderr: 'The domain/default pair does not exist' }); } });
});
test('managed policy boundary detects a known local config path', async () => {
  const visited = [];
  await checkManagedPolicy({ home: '/synthetic/home', platform: 'linux', inspect: async file => { visited.push(file); return missing(); } });
  assert.ok(visited.includes('/synthetic/home/.codex/config.toml'), 'known local computer-use policy is never probed');
});
test('upstream initialization/results/errors/notifications preserved and provider env isolated', async t => {
  const f = await fixture(t), sent = [], returned = [];
  readLines(f.children[0].stdin, v => sent.push(v), assert.fail);
  readLines(f.output, v => returned.push(v), assert.fail);
  f.input.write(JSON.stringify(rpc(1, 'initialize', { capabilities: { experimental: { sample: true } }, protocolVersion: '2025-03-26' })) + '\n');
  await tick();
  assert.equal(sent[0].params.protocolVersion, '2025-03-26');
  assert.deepEqual(sent[0].params.capabilities.experimental, { sample: true });
  assert.deepEqual(sent[0].params.capabilities.elicitation, { form: {} });
  const result = { jsonrpc: '2.0', id: 1, result: { capabilities: { tools: {} }, arbitrary: ['preserved'] } };
  f.children[0].stdout.write(JSON.stringify(result) + '\n');
  const notification = { jsonrpc: '2.0', method: 'notifications/tools/list_changed' };
  f.children[0].stdout.write(JSON.stringify(notification) + '\n');
  assert.deepEqual(returned, [result, notification]);
  assert.equal(f.invocations[0].settings.env.CODEX_TOKEN, undefined);
  assert.equal(f.invocations[0].settings.env.NODE_OPTIONS, undefined);
  assert.equal(f.invocations[0].settings.env.CODEX_CLI_PATH, '/synthetic/policy');
  assert.equal(path.dirname(f.invocations[0].settings.env.SKY_CUA_SERVICE_NATIVE_PIPE_PATH), f.invocations[0].settings.env.CODEX_HOME);
  assert.ok(f.invocations[0].settings.env.CODEX_HOME.startsWith(f.state + path.sep));
});
test('EOF reports uncertainty and removes private session state without replay', async t => {
  const f = await fixture(t), replies = [];
  readLines(f.output, v => replies.push(v), assert.fail);
  f.input.write(JSON.stringify(rpc('pending', 'tools/list')) + '\n'); await tick();
  f.children[0].exit(); f.input.end(); await f.completed;
  assert.equal(replies[0].id, 'pending'); assert.match(replies[0].error.message, /uncertain.*No input was replayed/);
  assert.deepEqual(await readdir(f.state), []);
});
test('state rejects group/world-writable non-sticky ancestors', async t => {
  const directory = await root(t), parent = path.join(directory, 'public'), state = path.join(parent, 's');
  await mkdir(parent); await chmod(parent, 0o777); await mkdir(state, { mode: 0o700 });
  const child = new Child(); let spawned = false;
  const completed = runHost({ state, provider: '/synthetic', env: {} }, { managedCheck: async () => {}, input: new PassThrough(), output: new PassThrough(), spawnChild() { spawned = true; setImmediate(() => child.exit()); return child; } });
  try { await completed; } catch (e) { assert.match(e.message, /state|Unsafe/); }
  assert.equal(spawned, false, 'unsafe ancestor admitted and provider launched');
});
test('client cancellation during native startup does not dispatch cancelled input', async t => {
  let server;
  const f = await fixture(t, { onSpawn(child, settings, index) {
    if (index === 2) { server = createServer(); server.listen(settings.env.SKY_CUA_SERVICE_NATIVE_PIPE_PATH); }
  }});
  t.after(() => server && new Promise(resolve => server.close(resolve)));
  const sent = []; readLines(f.children[0].stdin, v => sent.push(v), assert.fail);
  f.input.write(JSON.stringify(rpc('cancel-me', 'tools/call', { name: 'js', arguments: { code: 'syntheticInput()' } })) + '\n');
  f.input.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/cancelled', params: { requestId: 'cancel-me', reason: 'user cancelled' } }) + '\n');
  for (let n = 0; n < 50 && !sent.some(v => v.method === 'notifications/cancelled'); n++) await pause(10);
  assert.equal(sent.some(v => v.id === 'cancel-me' && v.method === 'tools/call'), false, 'cancelled JS dispatched before queued cancellation');
});
test('host SIGKILL reaps detached provider even if provider ignores stdin EOF', async t => {
  const directory = await root(t), provider = path.join(directory, 'provider'), pidFile = path.join(directory, 'pid');
  await writeFile(provider, `#!${process.execPath}\nimport { writeFileSync } from 'node:fs';\nwriteFileSync(process.env.HOME + '/pid', String(process.pid));\nprocess.stdin.resume(); process.stdin.on('end', () => {}); setInterval(() => {}, 1000);\n`, { mode: 0o700 });
  // A .mjs target prevents Node's extensionless executable CommonJS detection.
  const providerMjs = provider + '.mjs'; await writeFile(providerMjs, await readFile(provider), { mode: 0o700 });
  const harness = spawn(process.execPath, ['--input-type=module', '-e', `import { runHost } from ${JSON.stringify(hostUrl.href)}; await runHost(${JSON.stringify({ state: path.join(directory, 's'), provider: providerMjs, env: { HOME: directory } })}, { managedCheck: async () => {} });`], { stdio: ['pipe', 'ignore', 'pipe'] });
  let providerPid;
  t.after(() => { harness.kill('SIGKILL'); if (providerPid) { try { process.kill(-providerPid, 'SIGKILL'); } catch {} } });
  // Parallel native builds can delay synthetic Node startup beyond one second.
  // Bound the fixture wait independently of the owned-process cleanup assertion.
  const startupDeadline = Date.now() + 5000;
  while (Date.now() < startupDeadline) { try { providerPid = Number(await readFile(pidFile, 'utf8')); break; } catch {} await pause(10); }
  assert.ok(providerPid, 'synthetic provider did not start');
  harness.kill('SIGKILL'); await once(harness, 'exit'); await pause(300);
  let alive = true; try { process.kill(providerPid, 0); } catch { alive = false; }
  assert.equal(alive, false, 'detached provider survives host SIGKILL');
  await pause(1600);
  assert.deepEqual(await readdir(path.join(directory, 's')), [], 'catalog-only killed host leaves its private session directory behind');
});

test('provider stdout EOF closes pending requests even if its process remains alive', async t => {
  const f = await fixture(t), replies = [];
  readLines(f.output, v => replies.push(v), assert.fail);
  f.input.write(JSON.stringify(rpc('pending-eof', 'tools/list')) + '\n'); await tick();
  f.children[0].stdout.end(); await tick();
  assert.equal(replies.length, 1, 'provider stdout EOF does not trigger stop; pending calls hang indefinitely');
  assert.match(replies[0].error.message, /uncertain/);
});

test('native owner watchdog reaps TERM-ignoring helper descendants after leader exit', async t => {
  const directory = await root(t), app = path.join(directory, 'Fake.app');
  const helper = path.join(app, 'Contents/Resources/cua_node/lib/node_modules/@oai/sky/Codex Computer Use.app/Contents/MacOS/SkyComputerUseService');
  await mkdir(path.dirname(helper), { recursive: true });
  const grandchildCode = "require('node:fs').writeFileSync(process.env.HOME + '/grandchild', String(process.pid)); process.on('SIGTERM', () => {}); setInterval(() => {}, 1000);";
  await writeFile(helper, `#!${process.execPath}\nconst { spawn } = require('node:child_process'); require('node:fs').writeFileSync(process.env.HOME + '/leader', String(process.pid)); spawn(process.execPath, ['-e', ${JSON.stringify(grandchildCode)}], { stdio: 'ignore' }); setInterval(() => {}, 1000);\n`, { mode: 0o700 });
  const worker = spawn(process.execPath, [fileURLToPath(hostUrl), '--native-worker'], { env: { HOME: directory, NANOCODEX_CUA_NATIVE_APP: app, NANOCODEX_CUA_POLICY_HOST: '/synthetic/policy' }, stdio: ['pipe', 'ignore', 'pipe'] });
  let leader, grandchild;
  t.after(() => { worker.kill('SIGKILL'); if (leader) { try { process.kill(-leader, 'SIGKILL'); } catch {} } if (grandchild) { try { process.kill(grandchild, 'SIGKILL'); } catch {} } });
  for (let n = 0; n < 500; n++) {
    try { leader = Number(await readFile(path.join(directory, 'leader'), 'utf8')); grandchild = Number(await readFile(path.join(directory, 'grandchild'), 'utf8')); break; } catch {} await pause(10);
  }
  assert.ok(leader && grandchild, 'synthetic helper descendants did not start');
  worker.stdin.end(); await once(worker, 'exit'); await pause(1700);
  let alive = true; try { process.kill(grandchild, 0); } catch { alive = false; }
  assert.equal(alive, false, 'helper leader exit terminates watchdog before descendant SIGKILL escalation');
});

test('policy wire accepts missing jsonrpc only in explicitly non-MCP framing mode', () => {
  const request = { id: 7, method: 'config/read', params: {} };
  const strict = new PassThrough(); let strictFailures = 0;
  readLines(strict, () => assert.fail('MCP accepted missing version'), () => strictFailures++);
  strict.write(JSON.stringify(request) + '\n'); assert.equal(strictFailures, 1);
  const policy = new PassThrough(), replies = []; let policyFailures = 0;
  readLines(policy, value => replies.push(policyReply(value, true)), () => policyFailures++, false);
  policy.write(JSON.stringify(request) + '\n');
  assert.equal(replies[0].id, 7); assert.equal(replies[0].result.config.computer_use.default_app_access, 'allow');
  policy.write(JSON.stringify({ ...request, jsonrpc: '1.0' }) + '\n'); assert.equal(policyFailures, 1);
});

test('SIGKILL of native worker owner closes lease and reaps synthetic native helper', async t => {
  const directory = await root(t), app = path.join(directory, 'Lease.app');
  const helper = path.join(app, 'Contents/Resources/cua_node/lib/node_modules/@oai/sky/Codex Computer Use.app/Contents/MacOS/SkyComputerUseService');
  await mkdir(path.dirname(helper), { recursive: true });
  await writeFile(helper, `#!${process.execPath}\nrequire('node:fs').writeFileSync(process.env.HOME + '/helper', String(process.pid)); setInterval(() => {}, 1000);\n`, { mode: 0o700 });
  const workerEnv = { HOME: directory, NANOCODEX_CUA_NATIVE_APP: app, NANOCODEX_CUA_POLICY_HOST: '/synthetic/policy' };
  const ownerCode = `const { spawn } = require('node:child_process'); spawn(process.execPath, [${JSON.stringify(fileURLToPath(hostUrl))}, '--native-worker'], { env: ${JSON.stringify(workerEnv)}, detached: true, stdio: ['pipe', 'ignore', 'ignore'] }); setInterval(() => {}, 1000);`;
  const owner = spawn(process.execPath, ['-e', ownerCode], { stdio: ['ignore', 'ignore', 'ignore'] });
  let helperPid;
  t.after(() => { owner.kill('SIGKILL'); if (helperPid) { try { process.kill(-helperPid, 'SIGKILL'); } catch {} } });
  for (let n = 0; n < 500; n++) { try { helperPid = Number(await readFile(path.join(directory, 'helper'), 'utf8')); break; } catch {} await pause(10); }
  assert.ok(helperPid, 'synthetic native helper did not start');
  owner.kill('SIGKILL'); await once(owner, 'exit'); await pause(1800);
  let alive = true; try { process.kill(helperPid, 0); } catch { alive = false; }
  assert.equal(alive, false, 'native worker failed to observe killed owner EOF');
});

test('stat-only policy rejects external ownership and permits explicitly superseded owner preferences', async () => {
  const configPath = '/synthetic/home/.codex/config.toml';
  const inspectFor = metadata => async file => file === configPath ? metadata : missing();
  await assert.rejects(checkManagedPolicy({ home: '/synthetic/home', codexHome: undefined, platform: 'linux', inspect: inspectFor({ uid: process.getuid() + 1, isSymbolicLink: () => false }) }), /Externally owned/);
  // An owner-controlled preferences link is not a managed-policy authority.
  // No contents/credentials are read; the current explicit Nano grant wins.
  await checkManagedPolicy({ home: '/synthetic/home', codexHome: undefined, platform: 'linux', inspect: inspectFor({ uid: process.getuid(), isSymbolicLink: () => true }), follow: async () => ({ uid: process.getuid() }) });
  await checkManagedPolicy({ home: '/synthetic/home', codexHome: undefined, platform: 'linux', inspect: inspectFor({ uid: process.getuid(), isSymbolicLink: () => false }) });
});

test('owner config link cannot hide externally owned or unreadable policy targets', async () => {
  const settings = { home: '/synthetic/home', codexHome: undefined, platform: 'linux', inspect: async file => file.endsWith('/config.toml') ? {uid:process.getuid(), isSymbolicLink:()=>true} : missing() };
  await assert.rejects(checkManagedPolicy({...settings, follow:async()=>({uid:process.getuid()+1})}), /Externally owned/);
  await assert.rejects(checkManagedPolicy({...settings, follow:async()=>{throw Object.assign(new Error('unreadable target'),{code:'EACCES'});}}), /unreadable/);
});

test('home and absolute external CODEX_HOME enforced-source presence fails closed', async () => {
  for (const file of ['/synthetic/home/.codex/requirements.toml', '/synthetic/home/.codex/managed_config.toml', '/synthetic/managed/requirements.toml', '/synthetic/managed/managed_config.toml']) {
    await assert.rejects(checkManagedPolicy({ home: '/synthetic/home', codexHome: '/synthetic/managed', platform: 'linux', inspect: async checked => checked === file ? {} : missing() }), /Managed computer policy/);
  }
});

test('relative CODEX_HOME cannot silently bypass potentially enforced policy sources', async () => {
  const visited = [];
  const settings = { home: '/synthetic/home', codexHome: 'relative-managed', platform: 'linux', inspect: async file => {
    visited.push(file);
    if (file === path.resolve('relative-managed/requirements.toml') || file === 'relative-managed/requirements.toml') return {};
    return missing();
  }};
  await assert.rejects(checkManagedPolicy(settings), /policy|CODEX_HOME|absolute/i, 'relative external home was silently ignored');
});

test('cancelled queued request may reuse ID without dispatching the previous input', async t => {
  let server;
  const f = await fixture(t, { onSpawn(child, settings, index) { if (index === 2) { server = createServer(); server.listen(settings.env.SKY_CUA_SERVICE_NATIVE_PIPE_PATH); } } });
  t.after(() => server && new Promise(resolve => server.close(resolve)));
  const sent = [], replies = []; readLines(f.children[0].stdin, v => sent.push(v), assert.fail); readLines(f.output, v => replies.push(v), assert.fail);
  f.input.write(JSON.stringify(rpc('reuse', 'tools/call', { name: 'js', arguments: { code: 'oldInput()' } })) + '\n');
  f.input.write(JSON.stringify({ jsonrpc: '2.0', method: 'notifications/cancelled', params: { requestId: 'reuse' } }) + '\n');
  f.input.write(JSON.stringify(rpc('reuse', 'tools/call', { name: 'js', arguments: { code: 'newInput()' } })) + '\n');
  for (let n = 0; n < 100 && !sent.some(v => v.method === 'tools/call'); n++) await pause(10);
  assert.deepEqual(sent.filter(v => v.method === 'tools/call').map(v => v.params.arguments.code), ['newInput()']);
  assert.equal(replies[0].error.code, -32800);
});

test('provider stdin EPIPE fails pending calls through orderly cleanup', async t => {
  const f = await fixture(t), replies = []; readLines(f.output, v => replies.push(v), assert.fail);
  f.input.write(JSON.stringify(rpc('pipe-failed', 'tools/list')) + '\n'); await tick();
  f.children[0].stdin.emit('error', Object.assign(new Error('synthetic pipe failure'), { code: 'EPIPE' }));
  assert.equal(replies[0].id, 'pipe-failed'); assert.match(replies[0].error.message, /uncertain/);
  f.children.forEach(child => child.exit()); await f.completed;
  assert.deepEqual(await readdir(f.state), []);
});

test('more than 64 queued notifications closes without dispatch', async t => {
  const f = await fixture(t), sent = []; readLines(f.children[0].stdin, v => sent.push(v), assert.fail);
  f.input.write(Array.from({ length: 65 }, () => JSON.stringify({ jsonrpc: '2.0', method: 'notifications/initialized' })).join('\n') + '\n');
  await tick(); f.children.forEach(child => child.exit()); await f.completed;
  assert.deepEqual(sent, []); assert.deepEqual(await readdir(f.state), []);
});

test('input close without end fails pending calls instead of leaking host lease', async t => {
  const f = await fixture(t), replies = []; readLines(f.output, v => replies.push(v), assert.fail);
  f.input.write(JSON.stringify(rpc('input-closed', 'tools/list')) + '\n'); await tick();
  f.input.destroy(); await tick();
  assert.equal(replies.length, 1, 'clean close without end does not invalidate pending requests');
  assert.match(replies[0].error.message, /uncertain/);
});
