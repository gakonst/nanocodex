// Chrome native messaging <-> upstream browser-use native pipe. This host owns
// transport only. The unchanged extension owns tabs, groups, leases and UI.
// It has no Codex app-server, model, account, HTTP proxy or process launcher.
import { createServer } from 'node:net';
import { mkdir, lstat, chmod, unlink } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { endianness } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const MAX_FRAME = 64 * 1024 * 1024;
const CHROME_RECEIVE_LIMIT = 1024 * 1024;
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value);
const hasId = value => typeof value.id === 'string' || (typeof value.id === 'number' && Number.isFinite(value.id));
const little = endianness() === 'LE';
function frame(value, limit = MAX_FRAME) {
  const body = Buffer.from(JSON.stringify(value));
  if (body.length > limit) throw new Error('Browser message exceeds the native messaging limit.');
  const header = Buffer.alloc(4);
  little ? header.writeUInt32LE(body.length) : header.writeUInt32BE(body.length);
  return Buffer.concat([header, body]);
}
function send(stream, value, limit) {
  const bytes = frame(value, limit);
  if (stream.destroyed || stream.writableEnded) throw new Error('Browser transport closed.');
  if (stream.writableLength + bytes.length > MAX_FRAME) throw new Error('Browser transport output queue is full.');
  stream.write(bytes);
}
function read(stream, receive, fail) {
  let pending = Buffer.alloc(0), ended = false;
  stream.on('data', chunk => {
    if (ended) return;
    try {
      pending = Buffer.concat([pending, chunk]);
      while (pending.length >= 4) {
        const length = little ? pending.readUInt32LE() : pending.readUInt32BE();
        if (!length || length > MAX_FRAME) throw new Error('Invalid browser frame length.');
        if (pending.length < length + 4) break;
        const value = JSON.parse(pending.subarray(4, length + 4).toString('utf8'));
        pending = pending.subarray(length + 4);
        if (!object(value) || value.jsonrpc !== '2.0') throw new Error('Invalid browser RPC message.');
        receive(value);
      }
    } catch { ended = true; fail(); }
  });
  stream.once('error', fail);
  stream.once('close', () => { ended = true; fail(); });
  stream.once('end', () => { ended = true; fail(); });
}

export async function runBrowserHost({ directory = process.env.NANOCODEX_CUA_BROWSER_SOCKET_DIRECTORY ?? '/tmp/codex-browser-use', input = process.stdin, output = process.stdout } = {}) {
  if (process.platform === 'win32') throw new Error('The direct browser host requires Unix sockets.');
  if (!path.isAbsolute(directory) || directory !== path.normalize(directory)) throw new Error('Browser socket directory must be absolute.');
  await mkdir(directory, { mode: 0o700, recursive: true });
  const parent = await lstat(directory);
  if (!parent.isDirectory() || parent.isSymbolicLink() || parent.uid !== process.getuid() || (parent.mode & 0o022)) throw new Error('Browser socket directory must be owned by the current user and not writable by others.');
  const socketPath = path.join(directory, `nc-${process.pid}-${randomUUID().slice(0, 8)}.sock`);
  if (Buffer.byteLength(socketPath) > 103) throw new Error('Browser socket path is too long.');
  const clients = new Set(), requests = new Map(), heartbeats = new Map();
  let nextId = 0, closed = false, resolveDone;
  const done = new Promise(resolve => { resolveDone = resolve; });
  const native = value => send(output, value, CHROME_RECEIVE_LIMIT);
  const reject = (id, message) => native({ jsonrpc: '2.0', id, error: { code: -32601, message } });
  const finishHeartbeat = (id, result) => {
    const heartbeat = heartbeats.get(id);
    if (!heartbeat) return;
    clearTimeout(heartbeat.timer); heartbeats.delete(id);
    try { native({ jsonrpc: '2.0', id: heartbeat.original, result }); } catch { stop(); }
  };
  const remove = socket => {
    clients.delete(socket);
    for (const [id, request] of requests) if (request.socket === socket) requests.delete(id);
    for (const [id, heartbeat] of heartbeats) if (heartbeat.waiting.delete(socket) && !heartbeat.waiting.size) finishHeartbeat(id, false);
  };
  const server = createServer(socket => {
    clients.add(socket);
    socket.once('close', () => remove(socket));
    read(socket, message => {
      if (closed) return;
      if (!message.method && hasId(message)) {
        const heartbeat = heartbeats.get(message.id);
        if (heartbeat?.waiting.delete(socket)) {
          if (message.result) finishHeartbeat(message.id, message.result);
          else if (!heartbeat.waiting.size) finishHeartbeat(message.id, false);
        }
        return;
      }
      if (typeof message.method !== 'string') { socket.destroy(); return; }
      if (hasId(message)) {
        if (requests.size >= 4096) { socket.destroy(); return; }
        const id = `nanocodex-browser-${++nextId}`;
        requests.set(id, { socket, original: message.id, method: message.method });
        // This local host always identifies controlled-tab requests as agent
        // traffic. The extension implements the header and popup policy.
        const params = object(message.params) && typeof message.params.session_id === 'string' && !['getInfo', 'turnEnded'].includes(message.method)
          ? { ...message.params, agent_request_header_enabled: true } : message.params;
        try { native({ ...message, params, id }); }
        catch {
          requests.delete(id);
          try { send(socket, { jsonrpc: '2.0', id: message.id, error: { code: -32000, message: 'Browser request could not be sent; no retry was attempted.' } }); } catch { socket.destroy(); }
        }
      } else {
        try { native(message); } catch { socket.destroy(); }
      }
    }, () => socket.destroy());
  });
  const stop = () => {
    if (closed) return; closed = true;
    for (const heartbeat of heartbeats.values()) clearTimeout(heartbeat.timer);
    heartbeats.clear(); requests.clear();
    for (const socket of clients) socket.destroy();
    server.close(() => { void unlink(socketPath).catch(e => { if (e.code !== 'ENOENT') console.error('Nanocodex browser socket cleanup failed.'); }).finally(resolveDone); });
  };
  try {
    await new Promise((resolve, reject) => { server.once('error', reject); server.listen(socketPath, resolve); });
    await chmod(socketPath, 0o600);
  } catch (error) { stop(); await done; throw error; }
  server.on('error', stop);
  output.once('error', stop); output.once('close', stop);
  read(input, message => {
    if (closed) return;
    if (typeof message.method === 'string') {
      if (hasId(message)) {
        // Extension heartbeat must reach a live upstream kernel. A host alone
        // must not keep browser-control leases alive after its client exits.
        if (message.method === 'ping') {
          if (heartbeats.size >= 4096) { stop(); return; }
          const id = `nanocodex-heartbeat-${++nextId}`;
          const heartbeat = { original: message.id, waiting: new Set(clients), timer: setTimeout(() => finishHeartbeat(id, false), 2000) };
          heartbeats.set(id, heartbeat);
          if (!clients.size) finishHeartbeat(id, false);
          else for (const socket of clients) try { send(socket, { ...message, id }); } catch { socket.destroy(); }
        } else reject(message.id, 'This Nanocodex host provides browser control only; Codex app-server operations are unavailable.');
      } else for (const socket of clients) try { send(socket, message); } catch { socket.destroy(); }
      return;
    }
    if (!hasId(message)) return;
    const request = requests.get(message.id);
    if (!request) return;
    requests.delete(message.id);
    let response = { ...message, id: request.original };
    if (request.method === 'getInfo' && object(message.result) && message.result.type === 'extension') {
      if (typeof message.result.agentRequestHeaderEnabled !== 'boolean') {
        response = { jsonrpc: '2.0', id: request.original, error: { code: -32000, message: 'Update the browser extension: Nanocodex requires support for agent request headers.' } };
      } else {
        // Report the effective policy enforced by this bridge, retaining the
        // genuine extension identity and complete capability catalog. No
        // OpenAI account or remote feature-gate lookup is needed for this.
        response.result = { ...message.result, agentRequestHeaderEnabled: true };
      }
    }
    try { send(request.socket, response); } catch { request.socket.destroy(); }
  }, stop);
  const signals = ['SIGINT', 'SIGTERM', 'SIGHUP'];
  for (const signal of signals) process.on(signal, stop);
  await done;
  for (const signal of signals) process.off(signal, stop);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  runBrowserHost().catch(() => {
    console.error('Nanocodex browser transport failed. No browser input was replayed.');
    process.exitCode = 1;
  });
}
