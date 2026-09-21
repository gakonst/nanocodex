import { test } from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import { createConnection } from 'node:net';
import { mkdtemp, realpath, rm, chmod, symlink, stat, readdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { AppServer } from '../../crates/experimental/nanocodex-computer/src/openai-cua-app-server.mjs';
import { createGuiReadiness } from '../../crates/experimental/nanocodex-computer/src/openai-cua-gui-readiness.mjs';
import { NativeHost, configuration, secureDirectory, serverArguments, serverEndpoint, waitReady, bounded, serveLeases, ControlLease, connectLease, bindBridge, dispatchGuiUrl, PID_GURL_SCRIPT, runDaemon, configuredMcpServers, safeFailureMessage } from '../../crates/experimental/nanocodex-computer/src/openai-cua-native-host.mjs';

const A = '00000000-0000-0000-0000-000000000001';
const B = '00000000-0000-0000-0000-000000000002';
const tick = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => { let resolve, reject; const promise = new Promise((a, b) => { resolve = a; reject = b; }); return { promise, resolve, reject }; };
const config = extra => ({ timeoutMs: 200, idleMs: 10000, startupGraceMs: 0, codex: '/immutable/Contents/Resources/codex', gui: '/immutable/Contents/MacOS/Codex', provider: '/immutable/direct-provider', profile: '/private/synthetic/profile', env: { CODEX_HOME: '/synthetic/normal-home' }, ...extra });
const readyLine = id => `[electron-message-handler] maybe_resume_success threadId=${id} conversationId=${id} vmEvent=thread_resumed assignedStreamRole=owner markedStreaming=true\n`;
let pid = 50000;
class Child extends EventEmitter {
  constructor() { super(); this.pid = ++pid; this.exitCode = null; this.signalCode = null; this.killed = false; this.stdout = new PassThrough(); this.stderr = new PassThrough(); this.signals = []; }
  kill(signal) { this.signals.push(signal); this.killed = true; this.signalCode = signal; this.emit('exit', null, signal); return true; }
}
function fixture(t, extra = {}, settings = {}) {
  const children = [], invocations = [], dispatches = [];
  const deps = { spawn(command, args, options) { const child = new Child(); children.push(child); invocations.push({ command, args, options }); return child; },
    serverEndpoint: async () => 'ws://127.0.0.1:54321', waitReady: async () => {}, createReadiness: createGuiReadiness,
    dispatchUrl: async (...args) => { dispatches.push(args); }, ...extra };
  const host = new NativeHost(config(settings), deps);
  t.after(async () => { host.close(); await host.stopped; });
  return { host, children, invocations, dispatches };
}

for (const [name, fn] of [
  ['configuration rejects other platforms and URL inputs; bundle build isolates state', async () => {
    await assert.rejects(configuration({}, 'linux'), /requires macOS/);
    await assert.rejects(configuration({ NANOCODEX_CUA_NATIVE_APP: 'https://example.com' }, 'darwin'), /absolute filesystem/);
    const env = { NANOCODEX_CUA_NATIVE_APP: '/immutable/Codex.app', NANOCODEX_CUA_NATIVE_PROVIDER: '/immutable/direct', NANOCODEX_CUA_NATIVE_STATE: '/private/synthetic' };
    const plist = (_, key) => key === 'CFBundleVersion' ? '9922' : 'Codex';
    const first = await configuration(env, 'darwin', plist);
    const next = await configuration(env, 'darwin', (_, key) => key === 'CFBundleVersion' ? '9923' : 'Codex');
    assert.notEqual(first.socket, next.socket);
    assert.equal(first.node, '/immutable/Codex.app/Contents/Resources/cua_node/bin/node');
    assert.equal(first.gui, '/immutable/Codex.app/Contents/MacOS/Codex');
  }],
  ['state rejects symlinks and broad permissions', async t => {
    const root = await realpath(await mkdtemp(path.join(tmpdir(), 'native-host-test-')));
    t.after(() => rm(root, { recursive: true, force: true }));
    const directory = path.join(root, 'state'); await secureDirectory(directory);
    assert.equal((await stat(directory)).mode & 0o777, 0o700);
    await chmod(directory, 0o755);
    await assert.rejects(secureDirectory(directory), /0700/);
    await symlink(directory, path.join(root, 'link'));
    await assert.rejects(secureDirectory(path.join(root, 'link')), /real directories/);
  }],
  ['only direct upstream cua_repl is configured; normal CODEX_HOME stays unchanged', async t => {
    const { host, invocations } = fixture(t);
    await host.start();
    assert.deepEqual(invocations[0].args, serverArguments(host.config));
    assert.deepEqual(invocations[0].args, ['app-server', '--listen', 'ws://127.0.0.1:0', '-c', 'mcp_servers={cua_repl={command="/immutable/direct-provider",args=[],enabled=true,enabled_tools=["js","js_reset","turn_ended"],startup_timeout_sec=120}}']);
    assert.strictEqual(invocations[0].options.env, host.config.env);
  }],
  ['random endpoint comes only from exact owned server startup line', async () => {
    const child = new Child(); const abort = new AbortController();
    const pending = serverEndpoint(child, abort.signal); let done = false; pending.then(() => { done = true; });
    child.stderr.write('unrelated ws://127.0.0.1:1234\n  listening on: ws://0.0.0.0:1234\n');
    await tick(); assert.equal(done, false);
    child.stderr.write('  listening on: ws://127.0.0.1:54321\n');
    assert.equal(await pending, 'ws://127.0.0.1:54321');
    assert.equal(child.stderr.listenerCount('data'), 0);
  }],
  ['actual readyz success gates endpoint exposure and GUI spawn', async t => {
    const ready = deferred(); const { host, children } = fixture(t, { waitReady: () => ready.promise });
    let started = false; const startup = host.start().then(() => { started = true; });
    const attach = host.attach(A);
    await tick(); assert.equal(started, false); assert.equal(children.length, 1);
    ready.resolve(); await startup; await tick(); assert.equal(children.length, 2);
    children[1].stdout.write(readyLine(A)); await attach;
  }],
  ['readyz requires a successful response and refuses redirects', async () => {
    const calls = []; let attempt = 0;
    await waitReady('ws://127.0.0.1:43210', { fetchImpl: async (url, options) => { calls.push({ url: String(url), options }); return { ok: ++attempt === 3 }; }, pause: async () => {} });
    assert.equal(calls.length, 3);
    assert.equal(calls[0].url, 'http://127.0.0.1:43210/readyz');
    assert.equal(calls[0].options.redirect, 'error');
    await assert.rejects(waitReady('ws://example.com:1234'), /Invalid/);
  }],
  ['cold GUI uses signed executable, dedicated profile and actual server URL', async t => {
    const { host, children, invocations } = fixture(t);
    const attaching = host.attach(A); await tick();
    assert.equal(invocations[1].command, host.config.gui);
    assert.deepEqual(invocations[1].args, [`--user-data-dir=${host.config.profile}`, `codex://threads/${A}?hostId=local`]);
    assert.equal(invocations[1].options.env.CODEX_ELECTRON_USER_DATA_PATH, host.config.profile);
    assert.equal(invocations[1].options.env.CODEX_APP_SERVER_WS_URL, 'ws://127.0.0.1:54321');
    let ready = false; attaching.then(() => { ready = true; });
    children[1].stderr.write(readyLine(A)); children[1].stdout.write(readyLine(B)); await tick();
    assert.equal(ready, false);
    children[1].stdout.write(readyLine(A)); await attaching;
  }],
  ['warm attach serializes concurrent navigation and waits for each exact thread', async t => {
    const { host, children, dispatches } = fixture(t);
    const first = host.attach(A); const second = host.attach(B);
    await tick(); assert.equal(dispatches.length, 0);
    children[1].stdout.write(readyLine(A)); await first; await tick();
    assert.equal(dispatches.length, 1); assert.equal(dispatches[0][0], children[1].pid);
    assert.equal(dispatches[0][1], `codex://threads/${B}?hostId=local`);
    let ready = false; second.then(() => { ready = true; }); await tick(); assert.equal(ready, false);
    children[1].stdout.write(readyLine(B)); await second;
    assert.equal(children.length, 2);
  }],
  ['missing GUI readiness is bounded and closes all leases and owned children', async t => {
    const { host, children } = fixture(t, {}, { timeoutMs: 15 });
    let disconnects = 0; host.acquire(() => disconnects++); host.acquire(() => disconnects++);
    await assert.rejects(host.attach(A), /GUI readiness timed out/);
    assert.equal(disconnects, 2); assert.equal(host.closed instanceof Error, true);
    assert.deepEqual(children.map(child => child.signals), [['SIGTERM'], ['SIGTERM']]);
  }],
  ['GUI exit invalidates pending attach and leases, and never redispatches', async t => {
    const { host, children, dispatches } = fixture(t);
    let closed = false; host.acquire(() => { closed = true; });
    const pending = host.attach(A); await tick();
    children[1].exitCode = 1; children[1].emit('exit', 1);
    await assert.rejects(pending, /child exited/);
    await assert.rejects(host.attach(B), /child exited/);
    assert.equal(closed, true); assert.equal(dispatches.length, 0);
    assert.deepEqual(children[0].signals, ['SIGTERM']); assert.deepEqual(children[1].signals, []);
  }],
  ['last lease release starts idle cleanup; another lease cancels it', async t => {
    const { host } = fixture(t, {}, { idleMs: 20 }); await host.start();
    const first = host.acquire(() => {}), second = host.acquire(() => {});
    first(); await new Promise(resolve => setTimeout(resolve, 25)); assert.equal(host.closed, undefined);
    second(); const third = host.acquire(() => {});
    await new Promise(resolve => setTimeout(resolve, 25)); assert.equal(host.closed, undefined);
    third(); await new Promise(resolve => setTimeout(resolve, 30)); assert.ok(host.closed);
  }],
  ['PID GURL dispatch targets only the supplied live-child PID and never approves', async () => {
    const calls = [];
    await dispatchGuiUrl(12345, `codex://threads/${A}?hostId=local`, undefined, async (...args) => { calls.push(args); });
    assert.equal(calls[0][0], '/usr/bin/osascript');
    assert.deepEqual(calls[0][1], ['-l', 'JavaScript', '-e', PID_GURL_SCRIPT, '12345', `codex://threads/${A}?hostId=local`]);
    assert.match(PID_GURL_SCRIPT, /descriptorWithProcessIdentifier\(pid\)/);
    assert.match(PID_GURL_SCRIPT, /result\.isNil\(\)/);
    assert.match(PID_GURL_SCRIPT, /0x00000001 \| 0x00000010/);
    await assert.rejects(dispatchGuiUrl(0, `codex://threads/${A}?hostId=local`), /Invalid/);
  }],
  ['connect spawns once only for missing connection and retries connections only', async () => {
    let attempts = 0, spawned = 0;
    const socket = new PassThrough();
    const lease = await connectLease(config({ socket: '/synthetic/socket' }), {
      connect: async () => {
        if (++attempts < 3) throw Object.assign(new Error(), { code: 'ENOENT' });
        setImmediate(() => socket.write('{"endpoint":"ws://127.0.0.1:12345"}\n')); return socket;
      }, spawnDaemon: () => { spawned++; }, pause: async () => {},
    });
    assert.equal(attempts, 3); assert.equal(spawned, 1); lease.close();
    await assert.rejects(connectLease(config(), { connect: async () => { throw Object.assign(new Error('denied'), { code: 'EACCES' }); }, spawnDaemon: () => { spawned++; } }), /denied/);
    assert.equal(spawned, 1);
  }],
  ['Unix leases share one host; client end releases its lease without killing peers', async t => {
    const root = await realpath(await mkdtemp(path.join(tmpdir(), 'native-lease-')));
    const socketPath = path.join(root, 'host.sock');
    const { host, children } = fixture(t); await host.start();
    const server = serveLeases(host);
    const accepted = []; server.on('connection', connection => accepted.push(connection));
    await new Promise(resolve => server.listen(socketPath, resolve));
    t.after(async () => { host.close(); await new Promise(resolve => server.close(resolve)); await rm(root, { recursive: true, force: true }); });
    const first = new ControlLease(createConnection(socketPath), 500);
    const second = new ControlLease(createConnection(socketPath), 500);
    await Promise.all([first.ready, second.ready]); assert.equal(host.leases.size, 2);
    const attaching = first.attach(A); await tick(); await tick(); children[1].stdout.write(readyLine(A)); await attaching;
    const disconnected = new Promise(resolve => accepted[0].once('close', resolve));
    first.close(); await disconnected; assert.equal(host.leases.size, 1); assert.equal(host.closed, undefined);
    second.close();
  }],
  ['control disconnect closes AppServer and rejects pending calls without retries', async () => {
    const socket = new PassThrough(); const lease = new ControlLease(socket, 500);
    socket.write('{"endpoint":"ws://127.0.0.1:12345"}\n'); await lease.ready;
    const app = bindBridge(lease); const sent = []; app.socket = { send: value => sent.push(JSON.parse(value)), close() {} };
    const pending = app.request('mcpServer/tool/call', { tool: 'js' });
    lease.close(new Error('synthetic disconnect'));
    await assert.rejects(pending, /synthetic disconnect/); assert.equal(app.closed.message, 'synthetic disconnect'); assert.equal(sent.length, 1);
  }],
  ['MCP client close releases its lease, while approval requests remain unanswered', async () => {
    const socket = new PassThrough(); const lease = new ControlLease(socket, 500);
    socket.write('{"endpoint":"ws://127.0.0.1:12345"}\n'); await lease.ready;
    const app = bindBridge(lease); assert.ok(app instanceof AppServer);
    const sent = []; app.socket = { send: value => sent.push(value), close() {} };
    app.receive(JSON.stringify({ id: 99, method: 'item/commandExecution/requestApproval', params: {} }));
    assert.deepEqual(sent, []);
    app.close(); assert.ok(lease.closed); assert.equal(socket.destroyed, true);
  }],
  ['server startup timeout and cancellation terminate owned processes', async t => {
    const { host, children } = fixture(t, { waitReady: () => new Promise(() => {}) }, { timeoutMs: 15 });
    await assert.rejects(host.start(), /App server readiness timed out/);
    assert.deepEqual(children[0].signals, ['SIGTERM']);
    const abort = new AbortController(); abort.abort(new Error('cancelled'));
    await assert.rejects(bounded(() => { throw new Error('must not run'); }, 10, abort.signal), /cancelled/);
  }],
]) test(name, { timeout: 3000 }, fn);


test('macOS shlock allows one daemon for concurrent starts and cleanup removes only its socket/lock', { timeout: 3000, skip: process.platform !== 'darwin' }, async t => {
  const root = await realpath(await mkdtemp(path.join(tmpdir(), 'native-daemon-')));
  const previousMask = process.umask();
  t.after(async () => { process.umask(previousMask); await rm(root, { recursive: true, force: true }); });
  const spawned = deferred(), readiness = deferred(); const children = [];
  const settings = config({ state: root, profile: path.join(root, 'profile'), socket: path.join(root, 'host.sock'), lock: path.join(root, 'host.lock'), idleMs: 20, timeoutMs: 2000 });
  const dependencies = {
    configuredMcpServers: async () => [],
    spawn() { const child = new Child(); children.push(child); spawned.resolve(); return child; },
    serverEndpoint: async () => 'ws://127.0.0.1:54321', waitReady: () => readiness.promise,
    createReadiness: createGuiReadiness,
  };
  const first = runDaemon(settings, dependencies);
  first.catch(() => {});
  await spawned.promise;
  await runDaemon(settings, dependencies);
  assert.equal(children.length, 1);
  readiness.resolve(); await first;
  assert.deepEqual(children[0].signals, ['SIGTERM']);
  assert.deepEqual(await readdir(root), ['profile']);
});

test('connected daemon loss fails without spawning or reconnecting', { timeout: 3000 }, async () => {
  let attempts = 0, spawned = 0;
  await assert.rejects(connectLease(config({ timeoutMs: 100 }), {
    connect: async () => { attempts++; const socket = new PassThrough(); setImmediate(() => socket.destroy()); return socket; },
    spawnDaemon: () => { spawned++; },
  }), /disconnected/);
  assert.equal(attempts, 1); assert.equal(spawned, 0);
});

test('server exit immediately disconnects all client leases and cannot restart the same lifecycle', { timeout: 3000 }, async t => {
  const { host, children } = fixture(t); await host.start();
  let closed = 0; host.acquire(() => { closed++; }); host.acquire(() => { closed++; });
  children[0].exitCode = 0; children[0].emit('exit', 0);
  assert.equal(closed, 2);
  await assert.rejects(host.start(), /child exited/);
  assert.equal(children.length, 1);
});

test('daemon and GUI identity isolate effective CODEX_HOME without reading or changing it', async () => {
  const base = { HOME: '/synthetic/user', NANOCODEX_CUA_NATIVE_APP: '/immutable/Codex.app', NANOCODEX_CUA_NATIVE_PROVIDER: '/immutable/direct', NANOCODEX_CUA_NATIVE_STATE: '/private/synthetic' };
  const plist = (_, key) => key === 'CFBundleVersion' ? '9922' : 'Codex';
  const firstEnv = { ...base, CODEX_HOME: '/synthetic/account-a' };
  const first = await configuration(firstEnv, 'darwin', plist);
  const second = await configuration({ ...base, CODEX_HOME: '/synthetic/account-b' }, 'darwin', plist);
  for (const field of ['key', 'socket', 'lock', 'profile']) assert.notEqual(first[field], second[field]);
  assert.strictEqual(first.env, firstEnv);
  const repeated = await configuration({ ...firstEnv }, 'darwin', plist);
  assert.equal(first.socket, repeated.socket);
  const implicit = await configuration(base, 'darwin', plist);
  const explicit = await configuration({ ...base, CODEX_HOME: '/synthetic/user/.codex' }, 'darwin', plist);
  assert.equal(implicit.socket, explicit.socket);
  assert.equal(implicit.profile, explicit.profile);
  const otherHome = await configuration({ ...base, HOME: '/synthetic/other-user' }, 'darwin', plist);
  assert.notEqual(implicit.socket, otherHome.socket);
  const relative = await configuration({ ...base, CODEX_HOME: 'synthetic-relative-home' }, 'darwin', plist);
  const resolved = await configuration({ ...base, CODEX_HOME: path.resolve('synthetic-relative-home') }, 'darwin', plist);
  assert.equal(relative.socket, resolved.socket);
});

test('MCP discovery retains only names and transport kinds; overrides never copy endpoints or secrets', async () => {
  const settings = config();
  const calls = [];
  const rows = [
    { name: 'remote.server', enabled: true, transport: { type: 'streamable_http', url: 'https://synthetic.invalid/private', bearer_token: 'synthetic-bearer-secret', http_headers: { Authorization: 'synthetic-header-secret' } } },
    { name: 'local"server', transport: { type: 'stdio', command: '/synthetic/private-command', args: ['synthetic-private-argument'], env: { TOKEN: 'synthetic-env-secret' } } },
    { name: 'cua_repl', transport: { type: 'stdio', command: '/synthetic/old-provider' } },
  ];
  const servers = await configuredMcpServers(settings, async (...args) => { calls.push(args); return { stdout: JSON.stringify(rows) }; });
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], settings.codex);
  assert.deepEqual(calls[0][1], ['mcp', 'list', '--json']);
  assert.strictEqual(calls[0][2].env, settings.env);
  assert.deepEqual(servers, [{ name: 'remote.server', type: 'streamable_http' }, { name: 'local"server', type: 'stdio' }]);
  const args = serverArguments({ ...settings, disabledMcpServers: servers });
  assert.equal(args.at(-1), 'mcp_servers={"remote.server"={url="http://127.0.0.1:9",enabled=false},"local\\"server"={command="/immutable/direct-provider",enabled=false},cua_repl={command="/immutable/direct-provider",args=[],enabled=true,enabled_tools=["js","js_reset","turn_ended"],startup_timeout_sec=120}}');
  const retained = JSON.stringify({ servers, args });
  for (const secret of ['synthetic.invalid', 'synthetic-bearer-secret', 'synthetic-header-secret', '/synthetic/private-command', 'synthetic-private-argument', 'synthetic-env-secret', '/synthetic/old-provider']) assert.equal(retained.includes(secret), false);
});

test('MCP discovery rejects malformed rows and redacts command and parse failures', async () => {
  const invalid = [
    'synthetic-invalid-json-secret', 'null', '{}', '[null]',
    JSON.stringify([{ name: '', transport: { type: 'stdio' } }]),
    JSON.stringify([{ name: 'bad\nname', transport: { type: 'stdio' } }]),
    JSON.stringify([{ name: 'x'.repeat(257), transport: { type: 'stdio' } }]),
    JSON.stringify([{ name: 'missing-transport' }]),
    JSON.stringify([{ name: 'unknown-transport', transport: { type: 'synthetic-unsupported-secret' } }]),
    JSON.stringify([{ name: 'duplicate', transport: { type: 'stdio' } }, { name: 'duplicate', transport: { type: 'streamable_http' } }]),
  ];
  const safeMessage = 'Cannot inspect MCP server names through the official CLI. Verify the normal Codex configuration.';
  for (const stdout of invalid) {
    await assert.rejects(configuredMcpServers(config(), async () => ({ stdout })), error => safeFailureMessage(error) === safeMessage);
  }
  await assert.rejects(configuredMcpServers(config(), async () => { throw new Error('synthetic-command-secret'); }), error => safeFailureMessage(error) === safeMessage);
});

test('MCP metadata child receives startup cancellation and preserves its safe reason', async () => {
  const controller = new AbortController();
  const entered = deferred();
  const reason = new Error('synthetic startup cancellation');
  const pending = configuredMcpServers(config(), async (_, __, options) => {
    assert.strictEqual(options.signal, controller.signal);
    entered.resolve();
    await new Promise((_, reject) => options.signal.addEventListener('abort', () => reject(new Error('synthetic-private-command-error')), { once: true }));
  }, controller.signal);
  const rejected = assert.rejects(pending, error => error === reason);
  await entered.promise;
  controller.abort(reason);
  await rejected;
});

for (const phase of ['metadata', 'endpoint', 'readyz']) for (const signalName of ['SIGINT', 'SIGTERM']) {
  test(`${signalName} during ${phase} startup cleans owned children and lock`, { timeout: 3000, skip: process.platform !== 'darwin' }, async t => {
    const root = await realpath(await mkdtemp(path.join(tmpdir(), 'native-startup-signal-')));
    const previousMask = process.umask();
    const signals = new EventEmitter();
    t.after(async () => { process.umask(previousMask); await rm(root, { recursive: true, force: true }); });
    const entered = deferred();
    const children = [];
    let metadataSignal;
    const settings = config({ state: root, profile: path.join(root, 'profile'), socket: path.join(root, 'host.sock'), lock: path.join(root, 'host.lock'), timeoutMs: 2000 });
    const dependencies = {
      signals,
      configuredMcpServers: async (_, __, signal) => {
        metadataSignal = signal;
        if (phase !== 'metadata') return [];
        entered.resolve();
        await new Promise((_, reject) => signal.addEventListener('abort', () => reject(signal.reason), { once: true }));
      },
      spawn() { const child = new Child(); children.push(child); return child; },
      serverEndpoint: async () => {
        if (phase !== 'endpoint') return 'ws://127.0.0.1:54321';
        entered.resolve();
        return new Promise(() => {});
      },
      waitReady: async () => { entered.resolve(); return new Promise(() => {}); },
    };
    const running = runDaemon(settings, dependencies);
    const rejected = assert.rejects(running, /Native host stopped/);
    await entered.promise;
    assert.equal(signals.listenerCount(signalName), 1);
    signals.emit(signalName);
    await rejected;
    assert.equal(metadataSignal.aborted, true);
    assert.equal(children.length, phase === 'metadata' ? 0 : 1);
    for (const child of children) assert.deepEqual(child.signals, ['SIGTERM']);
    assert.equal(signals.listenerCount('SIGINT'), 0);
    assert.equal(signals.listenerCount('SIGTERM'), 0);
    assert.deepEqual(await readdir(root), ['profile']);
  });
}

test('managed bridge uses the headless official server and never attaches a GUI', async () => {
  let configuration, dependencies;
  const lease = { endpoint: 'ws://127.0.0.1:12345', timeoutMs: 500,
    attach() { throw new Error('GUI attachment must not run'); }, close() {} };
  class FakeAppServer {
    constructor(config, deps) { configuration = config; dependencies = deps; }
    close() {}
  }
  bindBridge(lease, { AppServerImpl: FakeAppServer });
  assert.deepEqual(configuration, { url: lease.endpoint, openGui: false, headless: true, timeoutMs: 500 });
  assert.equal(dependencies.openGui, undefined);
});
