#!/usr/bin/env node
// Managed lifecycle for the unmodified official macOS host. No auth material is read.
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { createHash } from 'node:crypto';
import { createServer, createConnection } from 'node:net';
import { lstat, mkdir, chmod, unlink, readFile, access, realpath } from 'node:fs/promises';
import { constants } from 'node:fs';
import path from 'node:path';
import { homedir } from 'node:os';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { AppServer, serveMcp } from './openai-cua-app-server.mjs';

const exec = promisify(execFile);
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const fail = message => Object.assign(new Error(message), { nativeHostSafe: true });
export function safeFailureMessage(error) {
  if (error?.nativeHostSafe) return error.message;
  if (['ENOENT', 'EACCES', 'EPERM'].includes(error?.code)) return 'Native host cannot access its configured bundle, provider, or private state directory. Verify the installed paths and permissions.';
  return 'Managed official CUA host failed. Verify the installed bundle and private state permissions.';
}
const MAX_LINE = 16384;
export const DEFAULTS = { timeoutMs: 120000, idleMs: 60000, startupGraceMs: 120000 };

function absolute(value, label) {
  if (typeof value !== 'string' || !path.isAbsolute(value) || /[\x00-\x1f\x7f]/.test(value) || value !== path.resolve(value)) throw fail(`${label} must be a normalized absolute filesystem path.`);
  return value;
}

export async function configuration(env = process.env, platform = process.platform, plist = async (file, key) => (await exec('/usr/libexec/PlistBuddy', ['-c', `Print :${key}`, file])).stdout.trim()) {
  if (platform !== 'darwin') throw fail('The managed official CUA host requires macOS.');
  const app = absolute(env.NANOCODEX_CUA_NATIVE_APP, 'NANOCODEX_CUA_NATIVE_APP');
  const provider = absolute(env.NANOCODEX_CUA_NATIVE_PROVIDER, 'NANOCODEX_CUA_NATIVE_PROVIDER');
  const state = absolute(env.NANOCODEX_CUA_NATIVE_STATE ?? path.join(homedir(), '.nanocodex', 's'), 'NANOCODEX_CUA_NATIVE_STATE');
  const info = path.join(app, 'Contents', 'Info.plist');
  const [version, executable] = await Promise.all([plist(info, 'CFBundleVersion'), plist(info, 'CFBundleExecutable')]);
  if (!/^[A-Za-z0-9._-]+$/.test(version) || !/^[A-Za-z0-9._ -]+$/.test(executable) || ['.', '..'].includes(executable)) throw fail('Invalid official bundle metadata.');
  // Resolve the selected home for identity only; preserve the caller's environment
  // and never inspect its auth/config files. Relative homes are cwd-dependent.
  const codexHome = path.resolve(env.CODEX_HOME || path.join(env.HOME || homedir(), '.codex'));
  const key = createHash('sha256').update(JSON.stringify([app, version, provider, state, codexHome])).digest('hex').slice(0, 24);
  const socketRoot = path.join(homedir(), '.nanocodex', 's');
  const socket = path.join(socketRoot, `${key}.sock`);
  if (Buffer.byteLength(socket) > 103) throw fail('Native host Unix socket path is too long.');
  return { ...DEFAULTS, app, provider, state, socketRoot, version, key, socket, lock: path.join(socketRoot, `${key}.lock`),
    profile: path.join(state, `${key}.profile`), node: path.join(app, 'Contents/Resources/cua_node/bin/node'),
    codex: path.join(app, 'Contents/Resources/codex'), gui: path.join(app, 'Contents/MacOS', executable), env };
}

// Reject symlink traversal and state directories belonging to another user.
export async function secureDirectory(directory, uid = process.getuid()) {
  const resolved = absolute(directory, 'State directory');
  const parts = resolved.split(path.sep).filter(Boolean);
  let current = path.parse(resolved).root;
  for (const part of parts) {
    current = path.join(current, part);
    try { await mkdir(current, { mode: 0o700 }); } catch (error) { if (error.code !== 'EEXIST') throw error; }
    const stat = await lstat(current);
    if (stat.isSymbolicLink() || !stat.isDirectory()) throw fail('State path must contain only real directories.');
    if (stat.uid !== uid && stat.uid !== 0) throw fail('State path has an unexpected owner.');
    if ((stat.mode & 0o022) && !(stat.uid === 0 && (stat.mode & 0o1000))) throw fail('State path is writable by another user.');
    if (current === resolved && (stat.uid !== uid || (stat.mode & 0o077))) throw fail('State directory must be owned by the current user with mode 0700.');
  }
}

async function socketStat(socket) {
  try {
    const stat = await lstat(socket);
    if (!stat.isSocket() || stat.isSymbolicLink() || stat.uid !== process.getuid() || (stat.mode & 0o077)) throw fail('Unsafe native host socket.');
    return stat;
  } catch (error) { if (error.code === 'ENOENT') return null; throw error; }
}

export function bounded(operation, timeoutMs, signal, label = 'Native host operation') {
  return new Promise((resolve, reject) => {
    let timer;
    const finish = (fn, value) => { clearTimeout(timer); signal?.removeEventListener('abort', abort); fn(value); };
    const abort = () => finish(reject, signal.reason ?? fail(`${label} cancelled.`));
    if (signal?.aborted) { abort(); return; }
    timer = setTimeout(() => finish(reject, fail(`${label} timed out.`)), timeoutMs);
    signal?.addEventListener('abort', abort, { once: true });
    Promise.resolve().then(operation).then(value => finish(resolve, value), error => finish(reject, error));
  });
}

const delay = (ms, signal) => new Promise((resolve, reject) => {
  const abort = () => { clearTimeout(timer); reject(signal.reason); };
  if (signal?.aborted) { reject(signal.reason); return; }
  const timer = setTimeout(() => { signal?.removeEventListener('abort', abort); resolve(); }, ms);
  signal?.addEventListener('abort', abort, { once: true });
});

// Query metadata through the official CLI without opening normal auth/config files.
// Consume names and transport kinds only; never log or persist endpoints or secrets.
export async function configuredMcpServers(config, execute = exec, signal) {
  try {
    const { stdout } = await execute(config.codex, ['mcp', 'list', '--json'], { env: config.env, timeout: config.timeoutMs, maxBuffer: 4 * 1024 * 1024, signal });
    const rows = JSON.parse(stdout);
    if (!Array.isArray(rows)) throw new Error();
    const servers = rows.map(row => ({ name: row?.name, type: row?.transport?.type }));
    if (servers.some(({ name, type }) => typeof name !== 'string' || !name || name.length > 256 || /[\x00-\x1f\x7f]/.test(name) || !['stdio', 'streamable_http'].includes(type))) throw new Error();
    if (new Set(servers.map(row => row.name)).size !== servers.length) throw new Error();
    return servers.filter(row => row.name !== 'cua_repl');
  } catch {
    if (signal?.aborted) throw signal.reason;
    throw fail('Cannot inspect MCP server names through the official CLI. Verify the normal Codex configuration.');
  }
}

export function serverArguments(config) {
  // The official server validates CLI entries before merging stored settings,
  // then merges again for each connection. Disabled entries still need a matching
  // transport kind. Their inert values avoid copying user endpoints or credentials.
  const disabled = (config.disabledMcpServers ?? []).map(({ name, type }) => {
    if (!['stdio', 'streamable_http'].includes(type)) throw fail('Unsupported configured MCP transport.');
    const transport = type === 'stdio' ? `command=${JSON.stringify(config.provider)}` : 'url="http://127.0.0.1:9"';
    return `${JSON.stringify(name)}={${transport},enabled=false}`;
  });
  disabled.push(`cua_repl={command=${JSON.stringify(config.provider)},args=[],enabled=true,enabled_tools=["js","js_reset","turn_ended"],startup_timeout_sec=120}`);
  return ['app-server', '--listen', 'ws://127.0.0.1:0', '-c', `mcp_servers={${disabled.join(',')}}`];
}

export function serverEndpoint(child, signal) {
  return new Promise((resolve, reject) => {
    let buffer = '';
    const cleanup = () => { child.stderr.off('data', data); signal?.removeEventListener('abort', abort); };
    const abort = () => { cleanup(); reject(signal.reason); };
    const data = chunk => {
      buffer += chunk.toString();
      let index;
      while ((index = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, index).replace(/\r$/, ''); buffer = buffer.slice(index + 1);
        const match = /^  listening on: (ws:\/\/127\.0\.0\.1:([0-9]+))$/.exec(line);
        if (match && Number(match[2]) > 0 && Number(match[2]) < 65536) { cleanup(); resolve(match[1]); return; }
      }
      if (Buffer.byteLength(buffer) > MAX_LINE) { cleanup(); reject(fail('Invalid app server startup metadata.')); }
    };
    if (signal?.aborted) { abort(); return; }
    child.stderr.on('data', data);
    signal?.addEventListener('abort', abort, { once: true });
  });
}

export async function waitReady(endpoint, { signal, fetchImpl = fetch, pause = delay } = {}) {
  const url = new URL(endpoint);
  if (url.protocol !== 'ws:' || url.hostname !== '127.0.0.1' || !url.port || url.username || url.password || url.search || url.hash || url.pathname !== '/') throw fail('Invalid app server endpoint.');
  url.protocol = 'http:'; url.pathname = '/readyz';
  while (!signal?.aborted) {
    try {
      const response = await fetchImpl(url, { signal, redirect: 'error' });
      await response.body?.cancel();
      if (response.ok) return;
    } catch (error) { if (signal?.aborted) throw signal.reason; }
    await pause(50, signal);
  }
  throw signal.reason;
}

// OS delivery only. Readiness is established separately from owned live stdout.
export const PID_GURL_SCRIPT = String.raw`ObjC.import('AppKit');
function run(argv) {
  if (argv.length !== 2 || !/^[1-9][0-9]*$/.test(argv[0])) throw new Error('Usage: dispatch-jxa.js PID CODEX_URL');
  const pid = Number(argv[0]);
  if (!Number.isSafeInteger(pid) || pid > 2147483647) throw new Error('Invalid PID');
  if (!/^codex:\/\/threads\/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\?hostId=local$/.test(argv[1])) throw new Error('Invalid thread URL');
  const app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(pid);
  if (app.isNil()) throw new Error('Target PID is not a running application');
  if (ObjC.unwrap(app.bundleIdentifier) !== 'com.openai.codex') throw new Error('Target PID is not the official Codex application');
  const target = $.NSAppleEventDescriptor.descriptorWithProcessIdentifier(pid);
  const event = $.NSAppleEventDescriptor.appleEventWithEventClassEventIDTargetDescriptorReturnIDTransactionID(0x4755524c, 0x4755524c, target, -1, 0);
  event.setParamDescriptorForKeyword($.NSAppleEventDescriptor.descriptorWithString(argv[1]), 0x2d2d2d2d);
  // kAENoReply | kAENeverInteract. A nil ObjC proxy is truthy: use isNil().
  const result = event.sendEventWithOptionsTimeoutError(0x00000001 | 0x00000010, 2, null);
  if (result.isNil()) throw new Error('PID-targeted GURL send failed');
  return 'GURL accepted for PID ' + pid;
}
`;

export async function dispatchGuiUrl(pid, url, signal, execute = exec) {
  if (!Number.isSafeInteger(pid) || pid < 1 || pid > 2147483647 || !/^codex:\/\/threads\/[0-9a-f-]{36}\?hostId=local$/i.test(url)) throw fail('Invalid owned GUI target.');
  await execute('/usr/bin/osascript', ['-l', 'JavaScript', '-e', PID_GURL_SCRIPT, String(pid), url], { signal, timeout: 5000, maxBuffer: 16384 });
}

export class NativeHost {
  constructor(config, dependencies) {
    this.config = config;
    this.deps = { spawn, serverEndpoint, waitReady, ...dependencies };
    this.leases = new Set(); this.children = new Set(); this.abort = new AbortController();
    this.attachTail = Promise.resolve(); this.generation = 0;
    this.startedAt = Date.now();
  }
  watch(child) {
    this.children.add(child);
    child.once('error', () => { this.children.delete(child); this.close(fail('Official host child failed to start.')); });
    child.once('exit', () => { child.stdout?.destroy(); child.stderr?.destroy(); this.children.delete(child); this.close(fail('Official host child exited; no calls were retried.')); });
    return child;
  }
  async start() {
    if (this.closed) throw this.closed;
    if (this.starting) return this.starting;
    this.starting = (async () => {
      try {
        this.server = this.watch(this.deps.spawn(this.config.codex, serverArguments(this.config), { env: this.config.env, stdio: ['ignore', 'pipe', 'pipe'] }));
        this.server.stdout.resume();
        this.endpoint = await bounded(() => this.deps.serverEndpoint(this.server, this.abort.signal), this.config.timeoutMs, this.abort.signal, 'App server endpoint discovery');
        this.server.stderr.resume();
        await bounded(() => this.deps.waitReady(this.endpoint, { signal: this.abort.signal }), this.config.timeoutMs, this.abort.signal, 'App server readiness');
        this.scheduleIdle();
        return this.endpoint;
      } catch (error) { this.close(error); throw error; }
    })();
    return this.starting;
  }
  acquire(close) {
    if (this.closed) throw this.closed;
    clearTimeout(this.idleTimer);
    const lease = { close }; this.leases.add(lease);
    return () => { if (this.leases.delete(lease)) this.scheduleIdle(); };
  }
  scheduleIdle() {
    clearTimeout(this.idleTimer);
    if (this.leases.size || this.closed) return;
    const remainingGrace = this.config.startupGraceMs - (Date.now() - this.startedAt);
    this.idleTimer = setTimeout(() => this.close(), Math.max(this.config.idleMs, remainingGrace));
  }
  attach(threadId) {
    if (typeof threadId !== 'string' || !UUID.test(threadId)) return Promise.reject(fail('Attach requires a thread UUID.'));
    const run = async () => {
      if (this.closed) throw this.closed;
      await this.start();
      if (!this.gui) {
        this.readiness = this.deps.createReadiness({ generation: ++this.generation, onReady: id => {
          if (this.expectedReady?.id === id) this.expectedReady.resolve();
        } });
      }
      const ready = new Promise(resolve => { this.expectedReady = { id: threadId, resolve }; });
      this.readiness.expect(threadId);
      // Start listening before the URL can produce the readiness event.
      const waiting = bounded(() => ready, this.config.timeoutMs, this.abort.signal, 'Official GUI readiness');
      waiting.catch(() => {});
      try {
        const url = `codex://threads/${threadId}?hostId=local`;
        if (!this.gui) {
          this.gui = this.watch(this.deps.spawn(this.config.gui, [`--user-data-dir=${this.config.profile}`, url], {
            env: { ...this.config.env, CODEX_ELECTRON_USER_DATA_PATH: this.config.profile, CODEX_APP_SERVER_WS_URL: this.endpoint }, stdio: ['ignore', 'pipe', 'pipe'],
          }));
          this.gui.stdout.on('data', chunk => this.readiness.push(chunk, { channel: 'stdout', generation: this.generation }));
          this.gui.stderr.resume();
        } else {
          // dispatchUrl targets only this live child PID, never LaunchServices/global open.
          if (this.gui.exitCode !== null || this.gui.signalCode != null || this.gui.killed || this.closed) throw fail('Owned GUI is no longer running.');
          await bounded(() => this.deps.dispatchUrl(this.gui.pid, url, this.abort.signal), this.config.timeoutMs, this.abort.signal, 'Official GUI URL dispatch');
        }
        await waiting;
        this.expectedReady = null;
      } catch (error) { this.close(error); await waiting.catch(() => {}); throw error; }
    };
    const result = this.attachTail.then(run);
    this.attachTail = result.catch(() => {});
    return result;
  }
  close(error = fail('Native host stopped; no calls were retried.')) {
    if (this.closed) return;
    this.closed = error; clearTimeout(this.idleTimer); this.abort.abort(error); this.readiness?.close();
    for (const lease of this.leases) lease.close(error);
    this.leases.clear();
    this.stopped = Promise.all([...this.children].map(child => new Promise(resolve => {
      // Retain the handle through exit; never kill a PID discovered elsewhere.
      const timer = setTimeout(() => child.kill('SIGKILL'), this.deps.killGraceMs ?? 5000);
      child.once('exit', () => { clearTimeout(timer); resolve(); });
      child.once('error', () => { clearTimeout(timer); resolve(); });
      child.kill('SIGTERM');
    })));
    this.deps.onClose?.(error);
  }
}

function lines(socket, receive, terminate) {
  let buffer = '';
  socket.setEncoding('utf8');
  socket.on('data', chunk => {
    buffer += chunk;
    let index;
    while ((index = buffer.indexOf('\n')) >= 0) {
      const line = buffer.slice(0, index); buffer = buffer.slice(index + 1);
      if (Buffer.byteLength(line) > MAX_LINE) { terminate(); return; }
      try { receive(JSON.parse(line)); } catch { terminate(); return; }
    }
    if (Buffer.byteLength(buffer) > MAX_LINE) terminate();
  });
}

export function serveLeases(host, netServer = createServer()) {
  netServer.on('connection', socket => {
    let release;
    try { release = host.acquire(() => socket.destroy()); } catch { socket.destroy(); return; }
    socket.on('error', () => socket.destroy()); socket.once('close', release);
    socket.write(`${JSON.stringify({ endpoint: host.endpoint })}\n`);
    let pending = 0;
    lines(socket, value => {
      if (!value || value.method !== 'attach' || !Number.isSafeInteger(value.id) || !UUID.test(value.threadId) || pending >= 128) throw fail('Invalid control request.');
      pending++;
      host.attach(value.threadId).then(() => { if (!socket.destroyed) socket.write(`${JSON.stringify({ id: value.id, ready: true })}\n`); }, () => socket.destroy()).finally(() => pending--);
    }, () => socket.destroy());
  });
  return netServer;
}

export class ControlLease {
  constructor(socket, timeoutMs) {
    this.socket = socket; this.timeoutMs = timeoutMs; this.pending = new Map(); this.nextId = 0;
    this.ready = new Promise((resolve, reject) => { this.resolveReady = resolve; this.rejectReady = reject; });
    this.ready.catch(() => {});
    this.timer = setTimeout(() => this.close(fail('Native daemon readiness timed out.')), timeoutMs);
    socket.on('error', () => this.close(fail('Native daemon connection failed.')));
    socket.on('close', () => this.close(fail('Native daemon disconnected; no calls were retried.')));
    lines(socket, value => {
      if (!this.endpoint) {
        if (!value || typeof value.endpoint !== 'string' || !/^ws:\/\/127\.0\.0\.1:[1-9][0-9]{0,4}$/.test(value.endpoint) || Number(new URL(value.endpoint).port) > 65535) throw fail('Invalid native endpoint.');
        this.endpoint = value.endpoint; clearTimeout(this.timer); this.resolveReady(this.endpoint); return;
      }
      const call = this.pending.get(value.id);
      if (!call || value.ready !== true) throw fail('Invalid native attach response.');
      this.pending.delete(value.id); clearTimeout(call.timer); call.resolve();
    }, () => this.close(fail('Invalid native daemon protocol.')));
  }
  attach(threadId) {
    if (this.closed) return Promise.reject(this.closed);
    if (typeof threadId !== 'string' || !UUID.test(threadId)) return Promise.reject(fail('Attach requires a thread UUID.'));
    const id = ++this.nextId;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => this.close(fail('Native GUI readiness timed out.')), this.timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      this.socket.write(`${JSON.stringify({ id, method: 'attach', threadId })}\n`);
    });
  }
  close(error = fail('Native client ended; no calls were retried.')) {
    if (this.closed) return;
    this.closed = error; clearTimeout(this.timer); this.rejectReady(error);
    for (const call of this.pending.values()) { clearTimeout(call.timer); call.reject(error); }
    this.pending.clear(); this.socket.destroy(); this.onClose?.(error);
  }
}

async function connectSocket(socket, signal) {
  await socketStat(socket);
  return new Promise((resolve, reject) => {
    if (signal?.aborted) { reject(signal.reason); return; }
    const connection = createConnection(socket);
    const abort = () => { connection.destroy(); reject(signal.reason); };
    const cleanup = () => signal?.removeEventListener('abort', abort);
    signal?.addEventListener('abort', abort, { once: true });
    connection.once('error', error => { cleanup(); reject(error); });
    connection.once('connect', () => { cleanup(); resolve(connection); });
  });
}

export async function connectLease(config, { connect = connectSocket, spawnDaemon = () => {
  const child = spawn(config.node, [fileURLToPath(import.meta.url), '--daemon'], { env: config.env, detached: true, stdio: 'ignore' });
  // Egress is not involved; the child inherits the normal CODEX_HOME unchanged.
  child.unref(); return child;
}, pause = delay } = {}) {
  const deadline = Date.now() + config.timeoutMs;
  let spawned = false;
  let daemonFailure;
  const failed = new Promise((_, reject) => { daemonFailure = reject; });
  failed.catch(() => {});
  while (true) {
    let socket;
    const attempt = new AbortController();
    try { socket = await Promise.race([bounded(() => connect(config.socket, attempt.signal), Math.max(1, deadline - Date.now()), undefined, 'Native daemon connection'), failed]); }
    catch (error) {
      attempt.abort(error);
      if (!['ENOENT', 'ECONNREFUSED'].includes(error.code)) throw error;
      if (!spawned) {
        spawned = true;
        const child = spawnDaemon();
        child?.once('error', () => daemonFailure(fail('Native daemon could not start. Verify the official bundled Node runtime.')));
        child?.once('exit', (code, signal) => {
          if (code !== 0) daemonFailure(fail(`Native daemon exited during startup (status ${Number.isInteger(code) ? code : 'signal'}). Verify the pinned bundle, direct provider and private state permissions.`));
        });
      }
      if (Date.now() >= deadline) throw fail('Native daemon startup timed out.');
      await Promise.race([pause(50), failed]); continue;
    }
    const lease = new ControlLease(socket, Math.max(1, deadline - Date.now()));
    try { await Promise.race([lease.ready, failed]); } catch (error) { lease.close(error); throw error; }
    lease.timeoutMs = config.timeoutMs;
    return lease;
  }
}

export function bindBridge(lease, { AppServerImpl = AppServer, onThread = () => {} } = {}) {
  // The official server resolves noninteractive confirmations under its own
  // effective policy. A GUI attachment is not needed to call the CUA provider.
  const app = new AppServerImpl({ url: lease.endpoint, openGui: false, headless: true, timeoutMs: lease.timeoutMs }, { onThread });
  const close = app.close.bind(app);
  app.close = error => { close(error); lease.close(error); };
  lease.onClose = error => close(error);
  if (lease.closed) close(lease.closed);
  return app;
}

export async function runDaemon(config, dependencies) {
  const lifecycle = new AbortController();
  const signals = dependencies?.signals ?? process;
  let host, server, serverClosed, ownedSocket, ownedLock = false;
  const stop = () => {
    const error = fail('Native host stopped; no calls were retried.');
    lifecycle.abort(error);
    host?.close(error);
  };
  const cleanup = async () => {
    server?.close();
    await host?.stopped;
    await serverClosed;
    if (ownedSocket) {
      const current = await socketStat(config.socket);
      if (current?.ino === ownedSocket.ino) await unlink(config.socket);
    }
    if (ownedLock) {
      try { if ((await readFile(config.lock, 'utf8')).trim() === String(process.pid)) await unlink(config.lock); } catch (error) { if (error.code !== 'ENOENT') throw error; }
    }
  };
  let cleaned;
  const finish = () => cleaned ??= cleanup();
  // Cover every startup await, including metadata inspection, before spawning
  // the official server. Signals close only children owned by this lifecycle.
  signals.on('SIGINT', stop); signals.on('SIGTERM', stop);
  try {
    await secureDirectory(config.state);
    await secureDirectory(config.socketRoot ?? config.state);
    await secureDirectory(config.profile);
    process.umask(0o077);
    lifecycle.signal.throwIfAborted();
    try {
      const lock = await lstat(config.lock);
      if (!lock.isFile() || lock.isSymbolicLink() || lock.uid !== process.getuid()) throw fail('Unsafe native host lock.');
    } catch (error) { if (error.code !== 'ENOENT') throw error; }
    lifecycle.signal.throwIfAborted();
    // shlock owns stale-PID handling. Let acquisition settle before cleanup so
    // a signal cannot leave a newly acquired lock behind. Never remove a loser.
    try { await exec('/usr/bin/shlock', ['-p', String(process.pid), '-f', config.lock]); }
    catch { return; }
    ownedLock = true;
    lifecycle.signal.throwIfAborted();
    const stale = await socketStat(config.socket);
    if (stale) await unlink(config.socket);
    lifecycle.signal.throwIfAborted();
    const disabledMcpServers = await (dependencies?.configuredMcpServers ?? configuredMcpServers)(config, undefined, lifecycle.signal);
    lifecycle.signal.throwIfAborted();
    host = new NativeHost({ ...config, disabledMcpServers }, { ...dependencies, onClose: () => { void finish().catch(() => { process.exitCode = 1; }); } });
    await host.start();
    if (host.closed) throw host.closed;
    server = serveLeases(host);
    serverClosed = new Promise(resolve => server.once('close', resolve));
    server.on('error', () => host.close(fail('Native control server failed.')));
    await new Promise((resolve, reject) => { server.once('error', reject); server.listen(config.socket, resolve); });
    if (host.closed) throw host.closed;
    await chmod(config.socket, 0o600); ownedSocket = await socketStat(config.socket);
    await serverClosed;
  } catch (error) { host?.close(error); throw error; }
  finally {
    try { await finish(); }
    finally { signals.off('SIGINT', stop); signals.off('SIGTERM', stop); }
  }
}

export async function main(args = process.argv.slice(2)) {
  if (args.some(arg => arg !== '--daemon') || args.length > 1) throw fail('Usage: openai-cua-native-host.mjs [--daemon]');
  const config = await configuration();
  await secureDirectory(config.state);
  await secureDirectory(config.socketRoot);
  for (const executable of [config.node, config.codex, config.gui, config.provider]) await access(executable, constants.X_OK);
  // Implementations are separately reviewed against the pinned official build.
  const { KNOWN_GUI_BUILD, createGuiReadiness } = await import('./openai-cua-gui-readiness.mjs');
  if (config.version !== KNOWN_GUI_BUILD) throw fail('Unsupported official GUI readiness build.');
  if (args[0] === '--daemon') {
    if (await realpath(process.execPath) !== await realpath(config.node)) throw fail('Daemon mode requires the official bundled Node runtime.');
    await runDaemon(config, { createReadiness: createGuiReadiness, dispatchUrl: dispatchGuiUrl });
  } else {
    const lease = await connectLease(config);
    const stop = serveMcp(bindBridge(lease));
    process.once('SIGINT', stop); process.once('SIGTERM', stop);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch(error => { console.error(safeFailureMessage(error)); process.exitCode = 1; });
}
