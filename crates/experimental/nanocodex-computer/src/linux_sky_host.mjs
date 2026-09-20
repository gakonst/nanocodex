// Desktop-user host for the unmodified @oai/sky/service on Linux.
import net from 'node:net';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fork, spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { fileURLToPath, pathToFileURL } from 'node:url';
const here = path.dirname(fileURLToPath(import.meta.url));
const record = v => v !== null && typeof v === 'object' && !Array.isArray(v);
export function validateRequest(r) {
  if (!record(r)) throw new Error('Invalid Sky request');
  if (r.type === 'setup') return;
  if (r.type === 'execute' && typeof r.method === 'string' && Array.isArray(r.args)) return;
  if (['drag_start', 'drag_move', 'drag_end'].includes(r.type) && typeof r.handle_id === 'string' && r.handle_id.length > 0 && r.handle_id.length <= 256 && (r.type === 'drag_end' || record(r.point))) return;
  throw new Error('Invalid Sky request');
}
export function frame(value) {
  const body = Buffer.from(JSON.stringify(value));
  if (body.length > 64 * 1024 * 1024) throw new Error('Sky response exceeds 64 MiB');
  const result = Buffer.allocUnsafe(body.length + 4);
  result.writeUInt32LE(body.length); body.copy(result, 4); return result;
}
export function createSession(servicePath, { cleanupMs = 3000 } = {}) {
  const child = fork(path.join(here, 'linux_sky_worker.mjs'), [servicePath], {
    detached: true, stdio: ['ignore', 'ignore', 'pipe', 'ipc'], execArgv: [],
  });
  // Drain bounded native diagnostics without placing desktop data in host logs.
  child.stderr.resume();
  let nextId = 1, closed = false, stopping;
  const pending = new Map();
  const fail = error => { for (const p of pending.values()) p.reject(error); pending.clear(); };
  child.on('error', fail);
  child.on('exit', () => fail(new Error('Sky desktop service exited')));
  child.on('message', message => {
    const p = pending.get(message.id);
    if (!p) return;
    pending.delete(message.id);
    message.error ? p.reject(new Error(message.error)) : p.resolve(message.value);
  });
  function send(message) {
    const id = nextId++;
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject });
      child.send({ id, ...message }, error => {
        if (error) { pending.delete(id); reject(error); }
      });
    });
  }
  return {
    pid: child.pid,
    async request(request) {
      if (closed) throw new Error('Sky session is closed');
      validateRequest(request);
      return send({ request });
    },
    stop() {
      if (stopping) return stopping;
      closed = true;
      stopping = (async () => {
        let timer;
        try {
          await Promise.race([send({ cleanup: true }), new Promise(resolve => { timer = setTimeout(resolve, cleanupMs); })]);
        } catch { /* Always reap the process group even after native failure. */ }
        finally { clearTimeout(timer); }
        fail(new Error('Sky session ended'));
        const kill = signal => { if (!child.pid) return; try { process.kill(-child.pid, signal); } catch (e) { if (e.code !== 'ESRCH') throw e; } };
        const exited = new Promise(resolve => { if (child.exitCode !== null || child.signalCode !== null) resolve(); else child.once('exit', resolve); });
        kill('SIGTERM');
        const force = setTimeout(() => kill('SIGKILL'), 500);
        await exited;
        clearTimeout(force);
        // Reap a helper that outlived its service parent.
        kill('SIGKILL');
      })();
      return stopping;
    },
  };
}
export async function startSkyHost({ servicePath, directory, makeSession = () => createSession(servicePath) }) {
  const ownDirectory = directory ?? fs.mkdtempSync(path.join(os.tmpdir(), 'nanocodex-sky-'));
  fs.chmodSync(ownDirectory, 0o700);
  const socketPath = path.join(ownDirectory, 'sky.sock');
  const peers = new Map();
  let queue = Promise.resolve(), disposed = false, disposing;
  const serialize = fn => { const result = queue.then(fn); queue = result.catch(() => {}); return result; };
  const server = net.createServer(socket => {
    if (disposed || peers.size >= 16) { socket.destroy(); return; }
    const session = makeSession();
    const peer = { session, closed: false, count: 0, stop: undefined };
    peers.set(socket, peer);
    const stop = () => {
      if (peer.stop) return peer.stop;
      peer.closed = true;
      peer.stop = session.stop().finally(() => peers.delete(socket));
      return peer.stop;
    };
    let buffer = Buffer.alloc(0);
    socket.on('error', () => {});
    socket.on('end', () => { void stop().catch(() => {}); });
    socket.on('close', () => { void stop().catch(() => {}); });
    socket.on('data', chunk => {
      try {
        buffer = Buffer.concat([buffer, chunk]);
        while (buffer.length >= 4) {
          const length = buffer.readUInt32LE();
          if (length > 8 * 1024 * 1024) throw new Error('Sky request exceeds 8 MiB');
          if (buffer.length < length + 4) break;
          const message = JSON.parse(buffer.subarray(4, length + 4));
          buffer = buffer.subarray(length + 4);
          if (!record(message) || !Number.isSafeInteger(message.id)) throw new Error('Invalid Sky frame');
          validateRequest(message.request);
          if (++peer.count > 16) throw new Error('Sky request queue is full');
          void serialize(async () => {
            if (peer.closed) return;
            let response;
            try { response = { id: message.id, value: await session.request(message.request) }; }
            catch (error) { response = { id: message.id, error: String(error.message ?? error) }; }
            if (!peer.closed && !socket.destroyed) socket.write(frame(response));
          }).catch(() => socket.destroy()).finally(() => { peer.count--; });
        }
      } catch { socket.destroy(); }
    });
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(socketPath, resolve); });
  fs.chmodSync(socketPath, 0o600);
  const endTurn = async () => {
    const closing = [];
    for (const [socket, peer] of peers) {
      peer.closed = true; socket.destroy();
      closing.push(peer.stop ??= peer.session.stop().finally(() => peers.delete(socket)));
    }
    await Promise.allSettled(closing);
  };
  return { socketPath, endTurn, dispose() {
    return disposing ??= Promise.resolve().then(async () => {
    disposed = true; await endTurn();
    await new Promise(resolve => server.close(resolve));
    fs.rmSync(socketPath, { force: true });
    if (!directory) fs.rmdirSync(ownDirectory);
    });
  } };
}
export async function runProvider(providerPath, providerArgs = []) {
  if (process.platform !== 'linux') throw new Error('Linux Sky host requires Linux');
  if (!process.env.CODEX_CLI_PATH) throw new Error('CODEX_CLI_PATH is required');
  const modules = process.env.NODE_REPL_NODE_MODULE_DIRS;
  if (!modules || !path.isAbsolute(modules)) throw new Error('An absolute verified OpenAI module directory is required');
  const servicePath = path.join(modules, '@oai/sky/dist/project/cua/sky_js/src/service.js');
  const host = await startSkyHost({ servicePath });
  const surfaces = (process.env.CUA_REPL_ENABLED_SURFACES ?? '').split(',');
  const services = process.env.NODE_REPL_TRUSTED_SERVICES ? JSON.parse(process.env.NODE_REPL_TRUSTED_SERVICES) :
    (surfaces.includes('browser') ? { browser: '@oai/browser-desktop/service' } : {});
  services.sky = path.join(here, 'linux_sky_proxy.mjs');
  const child = spawn(process.execPath, [providerPath, ...providerArgs], { stdio: ['pipe', 'pipe', 'inherit'], env: {
    ...process.env,
    NODE_REPL_TRUSTED_SERVICES: JSON.stringify(services),
    NODE_REPL_TRUSTED_CODE_PATHS: [...new Set((process.env.NODE_REPL_TRUSTED_CODE_PATHS ?? modules).split(path.delimiter).concat(here))].join(path.delimiter),
    NANOCODEX_LINUX_SKY_SOCKET: host.socketPath,
    NODE_REPL_UNTRUSTED_ENV_ALLOWLIST: [process.env.NODE_REPL_UNTRUSTED_ENV_ALLOWLIST, 'NANOCODEX_LINUX_SKY_SOCKET'].filter(Boolean).join(','),
  } });
  child.stdout.pipe(process.stdout);
  const input = createInterface({ input: process.stdin });
  let stopping;
  const stop = () => stopping ??= Promise.resolve().then(async () => { input.close(); process.stdin.pause(); child.kill(); await host.dispose(); });
  child.once('error', error => { console.error(error.message); process.exitCode = 1; void stop(); });
  child.once('exit', code => { if (!stopping) process.exitCode = code ?? 1; else process.exitCode ??= 0; void stop(); });
  child.stdin.on('error', () => { void stop(); });
  process.once('SIGINT', () => { void stop(); });
  process.once('SIGTERM', () => { void stop(); });
  input.on('close', () => { void stop(); });
  let forwarding = Promise.resolve();
  input.on('line', line => {
    forwarding = forwarding.then(async () => {
      const message = JSON.parse(line);
      if (message.method === 'notifications/cancelled' || (message.method === 'tools/call' && ['js_reset', 'turn_ended'].includes(message.params?.name))) await host.endTurn();
      if (!stopping) child.stdin.write(line + '\n');
    }).catch(error => { console.error(error.message); void stop(); });
  });
  return { child, host, stop };
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (!process.argv[2]) throw new Error('Expected the official CUA provider entry point');
  await runProvider(process.argv[2], process.argv.slice(3));
}
