// Nanocodex host for the unmodified OpenAI WindowsHelperTransport.
// Verified against OpenAI Sky 26.915.4065.0 and Codex Desktop build 9922.
// This runs beside the sandboxed provider, never inside its trusted worker.
import net from 'node:net';
import { randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const LIMIT = 8 * 1024 * 1024;
const record = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const turnKey = meta => record(meta) && ['session_id', 'turn_id'].every(k => typeof meta[k] === 'string' && meta[k].trim())
  ? `${meta.session_id}\0${meta.turn_id}` : null;

export function frame(value) {
  const body = Buffer.from(JSON.stringify(value));
  if (body.length > 64 * 1024 * 1024) throw new Error('Computer Use native pipe response exceeds 64 MiB');
  const result = Buffer.allocUnsafe(body.length + 4);
  result.writeUInt32LE(body.length);
  body.copy(result, 4);
  return result;
}

export async function startSkyHost({ makeTransport, pipePath = `\\\\.\\pipe\\nanocodex-sky-${randomUUID()}`, approvalTimeoutMs = 300000 }) {
  const sockets = new Set();
  const approvals = new Map();
  let transport = null, active = null, generation = 0, closing = null, disposed = false;
  const send = (socket, value) => { if (!socket.destroyed) socket.write(frame(value)); };
  const rejectApprovals = reason => {
    for (const entry of approvals.values()) { clearTimeout(entry.timer); entry.reject(new Error(reason)); }
    approvals.clear();
  };
  async function closeHelper(reason = 'Computer Use turn ended') {
    generation++;
    rejectApprovals(reason);
    const previous = transport;
    transport = null;
    active = null;
    if (closing) await closing;
    if (previous) {
      const operation = Promise.resolve().then(() => previous.close());
      closing = operation;
      try { await operation; } finally { if (closing === operation) closing = null; }
    }
  }
  function approve(socket, params, epoch) {
    if (disposed || generation !== epoch || socket.destroyed) return Promise.reject(new Error('Computer Use request ended'));
    return new Promise((resolve, reject) => {
      const id = `computer-use-approval:${randomUUID()}`;
      const timer = setTimeout(() => { approvals.delete(id); reject(new Error('Computer Use app approval timed out')); }, approvalTimeoutMs);
      approvals.set(id, { socket, resolve, reject, timer });
      send(socket, { jsonrpc: '2.0', id, method: 'requestComputerUseApproval', params });
    });
  }
  async function dispatch(socket, message) {
    if (!record(message) || message.jsonrpc !== '2.0') throw new Error('Invalid native pipe message');
    if (typeof message.id === 'string' && message.id.startsWith('computer-use-approval:')) {
      const pending = approvals.get(message.id);
      // Replies are bound to the connection that requested human approval.
      if (!pending || pending.socket !== socket || message.method !== undefined) return;
      approvals.delete(message.id); clearTimeout(pending.timer);
      if (message.error) pending.reject(new Error(String(message.error.message)));
      else if (!record(message.result) || !['accept', 'decline', 'cancel'].includes(message.result.action)) pending.reject(new Error('Invalid approval response'));
      else pending.resolve(message.result);
      return;
    }
    if (!['string', 'number'].includes(typeof message.id)) return;
    const reply = result => send(socket, { jsonrpc: '2.0', id: message.id, result });
    let owned = null;
    try {
      if (message.method === 'close') { await closeHelper(); reply(null); return; }
      if (message.method === 'request' && message.params?.method === 'end_turn') {
        if (active && turnKey(message.params.codexTurnMetadata) !== active.key) throw new Error('Computer Use turn mismatch');
        await closeHelper(); reply(null); return;
      }
      if (active?.busy || closing) throw new Error('Computer Use helper already has an active request');
      if (message.method === 'ping') { reply('pong'); return; }
      const request = message.params;
      if (message.method !== 'request' || !record(request) || typeof request.method !== 'string' || !request.method.trim() || !record(request.params)) throw new Error('Invalid Computer Use request');
      const key = turnKey(request.codexTurnMetadata);
      if (!key) throw new Error('Computer Use requires host turn metadata');
      if (Object.hasOwn(request.codexTurnMetadata, 'x-oai-cua-approved-app')) throw new Error('Computer Use approval must come from the host elicitation');
      if (active && active.key !== key) await closeHelper();
      const epoch = generation;
      active = owned = { key, socket, busy: true };
      transport ??= makeTransport();
      const result = await transport.request(request.method, request.params, {
        codexTurnMetadata: request.codexTurnMetadata,
        createElicitation: params => approve(socket, params, epoch),
      });
      if (generation === epoch && !socket.destroyed) reply(result);
    } catch (error) {
      send(socket, { jsonrpc: '2.0', id: message.id, error: { code: -32000, message: error.message } });
    } finally {
      if (owned && active === owned) active.busy = false;
    }
  }
  const server = net.createServer(socket => {
    sockets.add(socket);
    let buffer = Buffer.alloc(0);
    socket.on('data', chunk => {
      buffer = Buffer.concat([buffer, chunk]);
      while (buffer.length >= 4) {
        const length = buffer.readUInt32LE();
        if (length > LIMIT) { socket.destroy(); return; }
        if (buffer.length < length + 4) break;
        const body = buffer.subarray(4, length + 4); buffer = buffer.subarray(length + 4);
        try { void dispatch(socket, JSON.parse(body)).catch(() => socket.destroy()); }
        catch { socket.destroy(); return; }
      }
    });
    socket.on('error', () => {});
    socket.on('close', () => {
      sockets.delete(socket);
      if (active?.socket === socket || sockets.size === 0) void closeHelper('Computer Use native pipe client disconnected').catch(() => {});
    });
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(pipePath, resolve); });
  return {
    pipePath,
    endTurn: closeHelper,
    async dispose() {
      if (disposed) return;
      disposed = true;
      for (const socket of sockets) socket.destroy();
      await closeHelper('Computer Use native pipe is shutting down');
      await new Promise(resolve => server.close(resolve));
    },
  };
}

// Provider stdin/stdout remain the official MCP protocol, including elicitation.
// No approval is generated here: the SDK sends it through node_repl to the host.
export async function runProvider(providerPath, providerArgs = []) {
  if (process.platform !== 'win32') throw new Error('The Sky native pipe host is Windows-only');
  if (!process.env.CODEX_CLI_PATH) throw new Error('CODEX_CLI_PATH is required');
  const modules = process.env.NODE_REPL_NODE_MODULE_DIRS;
  if (!modules || !path.isAbsolute(modules)) throw new Error('A verified OpenAI module directory is required');
  const sky = path.join(modules, '@oai', 'sky');
  const modulePath = path.join(sky, 'dist/project/cua/sky_js/src/targets/windows/internal/helper_transport.js');
  const helperCommand = path.join(sky, 'bin/windows', process.arch === 'arm64' ? 'codex-computer-use-arm64.exe' : 'codex-computer-use.exe');
  const { WindowsHelperTransport } = await import(pathToFileURL(modulePath).href);
  const host = await startSkyHost({ makeTransport: () => new WindowsHelperTransport({
    helperCommand, helperArgs: ['--parent-pid', String(process.pid)],
    helperEnv: { CODEX_CLI_PATH: process.env.CODEX_CLI_PATH },
  }) });
  const child = spawn(process.execPath, [providerPath, ...providerArgs], { windowsHide: true, stdio: ['pipe', 'pipe', 'inherit'], env: {
    ...process.env, SKY_CUA_NATIVE_PIPE: '1', SKY_CUA_NATIVE_PIPE_DIRECTORY: host.pipePath,
    NODE_REPL_UNTRUSTED_ENV_ALLOWLIST: [...new Set((process.env.NODE_REPL_UNTRUSTED_ENV_ALLOWLIST ?? '').split(',').filter(Boolean).concat(['SKY_CUA_NATIVE_PIPE', 'SKY_CUA_NATIVE_PIPE_DIRECTORY']))].join(','),
  } });
  child.stdout.pipe(process.stdout);
  let ended = false;
  const stop = async () => { if (ended) return; ended = true; input.close(); process.stdin.pause(); child.kill(); await host.dispose(); };
  child.once('error', error => { console.error(error.message); void stop(); process.exitCode = 1; });
  child.once('exit', code => { void stop(); process.exitCode = code ?? 1; });
  child.stdin.on('error', () => { void stop(); });
  process.once('SIGTERM', () => { void stop(); });
  process.once('SIGINT', () => { void stop(); });
  const input = createInterface({ input: process.stdin });
  input.on('close', () => { void stop(); });
  // Serialize lifecycle cleanup with subsequent input. Elicitation replies must
  // still reach the provider while the pending tools/call is awaiting a human.
  let forwarding = Promise.resolve();
  input.on('line', line => {
    forwarding = forwarding.then(async () => {
      const message = JSON.parse(line);
      if (message.method === 'notifications/cancelled' ||
          (message.method === 'tools/call' && ['turn_ended', 'js_reset'].includes(message.params?.name))) await host.endTurn();
      if (!ended) child.stdin.write(line + '\n');
    }).catch(error => { console.error(error.message); void stop(); });
  });
  return { child, host, stop };
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (!process.argv[2]) throw new Error('Expected the verified OpenAI CUA provider entry point');
  await runProvider(process.argv[2], process.argv.slice(3));
}
