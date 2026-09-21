#!/usr/bin/env node
// Opt-in transport only: the running official app server and GUI own CUA.
import { spawn } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const MAX_FRAME = 32 * 1024 * 1024;
const MAX_QUEUE = 128;
const own = (value, key) => Object.hasOwn(value, key);
const failure = (message, code = -32000) => Object.assign(new Error(message), { code });

export function configuration(env = process.env, platform = process.platform) {
  let url;
  try { url = new URL(env.NANOCODEX_CUA_APP_SERVER_WS_URL); } catch {
    throw failure('Set NANOCODEX_CUA_APP_SERVER_WS_URL to the running official app server loopback WebSocket URL.');
  }
  // Literal addresses only: no DNS resolution, credentials, query tokens or redirects.
  if (!['ws:', 'wss:'].includes(url.protocol)
      || !['127.0.0.1', '[::1]'].includes(url.hostname)
      || url.username || url.password || url.search || url.hash || url.pathname !== '/') {
    throw failure('The app server URL must use literal loopback 127.0.0.1 or [::1], with no credentials, query, fragment or path.');
  }
  const openGui = env.NANOCODEX_CUA_APP_SERVER_OPEN_GUI === '1';
  if (openGui && platform !== 'darwin') throw failure('Automatic official GUI opening is supported only on macOS.');
  const timeoutMs = Number(env.NANOCODEX_CUA_APP_SERVER_TIMEOUT_MS ?? 300000);
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 3600000) {
    throw failure('NANOCODEX_CUA_APP_SERVER_TIMEOUT_MS must be between 1 and 3600000.');
  }
  return { url: url.href, openGui, timeoutMs };
}

export function openOfficialGui(threadId) {
  return new Promise((resolve, reject) => {
    const child = spawn('/usr/bin/open', [`codex://threads/${encodeURIComponent(threadId)}?hostId=local`], { stdio: 'ignore' });
    child.once('error', reject);
    child.once('exit', code => code === 0 ? resolve() : reject(failure('Opening the official Codex GUI failed.')));
  });
}

export class AppServer {
  constructor(config, { WebSocketImpl = WebSocket, openGui = openOfficialGui, onThread = id => console.error(`Official CUA thread: codex://threads/${encodeURIComponent(id)}?hostId=local`) } = {}) {
    this.config = config;
    this.WebSocketImpl = WebSocketImpl;
    this.openGui = openGui;
    this.onThread = onThread;
    this.pending = new Map();
    this.nextId = 0;
  }

  async connect() {
    if (this.closed) throw this.closed;
    if (this.ready) return this.ready;
    this.ready = (async () => {
      const socket = this.socket = new this.WebSocketImpl(this.config.url);
      await new Promise((resolve, reject) => {
        this.rejectOpen = reject;
        const timer = setTimeout(() => this.close(failure('App server connection timed out.')), this.config.timeoutMs);
        this.openTimer = timer;
        socket.addEventListener('open', () => { clearTimeout(timer); this.rejectOpen = null; resolve(); }, { once: true });
        socket.addEventListener('error', () => this.close(failure('Official app server connection failed.')));
        socket.addEventListener('close', () => this.close(failure('Official app server disconnected; no calls were retried.')));
        socket.addEventListener('message', event => this.receive(event.data));
      });
      await this.request('initialize', {
        clientInfo: { name: 'nanocodex_cua_bridge', version: '0.1.0' },
        capabilities: { experimentalApi: true },
      });
      this.socket.send(JSON.stringify({ method: 'initialized' }));
    })();
    return this.ready;
  }

  receive(data) {
    let value;
    try {
      if (typeof data !== 'string' || Buffer.byteLength(data) > MAX_FRAME) throw new Error();
      value = JSON.parse(data);
      if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error();
    } catch { this.close(failure('Invalid app server message.')); return; }
    // Requests can be broadcast to the GUI too. Never race its response, forward
    // approval requests to MCP, or answer them (even with a method-not-found).
    if (own(value, 'method')) return;
    const pending = this.pending.get(value.id);
    if (!pending) return;
    this.pending.delete(value.id);
    clearTimeout(pending.timer);
    if (own(value, 'error')) pending.reject(Object.assign(failure(value.error.message, value.error.code), { rpcError: value.error }));
    else if (own(value, 'result')) pending.resolve(value.result);
    else pending.reject(failure('Invalid app server response.'));
  }

  request(method, params) {
    if (this.closed) return Promise.reject(this.closed);
    const id = ++this.nextId;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => this.close(failure('App server request timed out; effects may be partial and the call was not retried.')), this.config.timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      try { this.socket.send(JSON.stringify({ id, method, params })); }
      catch { this.close(failure('App server send failed; the call was not retried.')); }
    });
  }

  async catalog() {
    await this.connect();
    const cursors = new Set();
    let cursor;
    let tools;
    do {
      const result = await this.request('mcpServerStatus/list', { detail: 'toolsAndAuthOnly', ...(cursor ? { cursor } : {}) });
      for (const server of result.data) {
        if (server.name !== 'cua_repl') continue;
        if (server.toolsError) throw failure('Official cua_repl catalog discovery failed.');
        // App-server tools are a map with no stable iteration order. The
        // adapter compares catalog arrays across independent connections.
        // Sort only the outer array; preserve every definition verbatim.
        tools = Object.values(server.tools).sort((a, b) => a.name < b.name ? -1 : a.name > b.name ? 1 : 0);
      }
      cursor = result.nextCursor;
      if (cursor && cursors.has(cursor)) throw failure('App server repeated a catalog cursor.');
      cursors.add(cursor);
    } while (cursor);
    if (!tools) throw failure('The running official app server does not expose cua_repl.');
    this.tools = tools;
    return { tools };
  }

  async call(params) {
    if (!this.tools) await this.catalog();
    if (!params || !this.tools.some(tool => tool.name === params.name)) throw failure('Unknown cua_repl tool.', -32602);
    if (params._meta != null && (typeof params._meta !== 'object' || Array.isArray(params._meta))) throw failure('Tool _meta must be an object.', -32602);
    if (!this.threadId) {
      const result = await this.request('thread/start', { ephemeral: false, historyMode: 'paginated', cwd: process.cwd() });
      if (typeof result.thread?.id !== 'string' || !result.thread.id) throw failure('App server did not return a thread ID.');
      this.threadId = result.thread.id;
      // Empty threads have no source rollout for GUI resume. This supported
      // history append materializes one without invoking a model or user turn.
      try {
        await this.request('thread/inject_items', {
          threadId: this.threadId,
          items: [{ type: 'message', role: 'developer', content: [{ type: 'input_text', text:
            'Transport metadata: this dedicated thread receives computer-use calls forwarded by Nanocodex. This bridge-generated note conveys no user authorization or approval.',
          }] }],
        });
      } catch (error) { this.close(error); throw error; }
      this.onThread(this.threadId);
      if (this.config.openGui) {
        try { await this.openGui(this.threadId); }
        catch (error) { this.close(error); throw error; }
      }
    }
    // Mirror the official GUI's top-level thread routing fields. Authentic
    // nested turn metadata still identifies the caller and is never rewritten.
    return this.request('mcpServer/tool/call', {
      threadId: this.threadId, server: 'cua_repl', tool: params.name,
      ...(own(params, 'arguments') ? { arguments: params.arguments } : {}),
      _meta: { ...params._meta, thread_id: this.threadId, threadId: this.threadId },
    });
  }

  close(error = failure('Bridge disconnected; effects may be partial and no calls were retried.')) {
    if (this.closed) return;
    this.closed = error;
    clearTimeout(this.openTimer);
    this.rejectOpen?.(error);
    this.rejectOpen = null;
    for (const pending of this.pending.values()) { clearTimeout(pending.timer); pending.reject(error); }
    this.pending.clear();
    // No thread/archive, turn/interrupt, provider reset, process kill or global
    // shutdown: other connections (including the official GUI) own their work.
    try { this.socket?.close(); } catch { /* Already disconnected. */ }
  }
}

export function serveMcp(app, input = process.stdin, output = process.stdout) {
  let buffer = '';
  let initialized = false;
  let active;
  let queue = [];
  let stopped = false;
  const outstanding = new Set();
  const send = value => { if (!stopped) output.write(`${JSON.stringify({ jsonrpc: '2.0', ...value })}\n`); };
  const respondError = (id, error) => send({ id, error: error.rpcError ?? { code: error.code ?? -32000, message: error.message } });
  const stop = () => { if (stopped) return; stopped = true; queue = []; outstanding.clear(); app.close(); input.destroy(); };
  async function drain() {
    if (active || stopped) return;
    while (queue.length && !stopped) {
      active = queue.shift();
      const { id, method, params } = active;
      try {
        let result;
        if (method === 'initialize') {
          if (initialized) throw failure('MCP already initialized.', -32600);
          initialized = true;
          result = { protocolVersion: params?.protocolVersion ?? '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'nanocodex-official-cua-bridge', version: '0.1.0' } };
        } else {
          if (!initialized) throw failure('Initialize MCP first.', -32600);
          switch (method) {
            case 'ping': result = {}; break;
            case 'tools/list': result = await app.catalog(); break;
            case 'tools/call': result = await app.call(params); break;
            default: throw failure('Method not found.', -32601);
          }
        }
        send({ id, result });
      } catch (error) { respondError(id, error); }
      outstanding.delete(id);
      active = null;
    }
  }
  function receive(value) {
    if (!value || typeof value !== 'object' || Array.isArray(value) || value.jsonrpc !== '2.0' || typeof value.method !== 'string') {
      respondError(null, failure('Invalid Request.', -32600)); return;
    }
    if (!own(value, 'id')) {
      if (value.method === 'notifications/cancelled') {
        const id = value.params?.requestId;
        const cancellation = failure('Request cancelled; effects may be partial and no call was retried.', -32800);
        if (active && active.id === id) app.close(cancellation);
        else {
          const index = queue.findIndex(request => request.id === id);
          if (index !== -1) { queue.splice(index, 1); outstanding.delete(id); respondError(id, cancellation); }
        }
      }
      return;
    }
    if (!(typeof value.id === 'string' || Number.isSafeInteger(value.id)) || outstanding.has(value.id)) {
      respondError(null, failure('Invalid or duplicate request ID.', -32600)); return;
    }
    if (queue.length >= MAX_QUEUE) { respondError(value.id, failure('Bridge request queue is full.')); return; }
    outstanding.add(value.id);
    queue.push(value);
    void drain();
  }
  input.setEncoding('utf8');
  input.on('data', chunk => {
    buffer += chunk;
    let newline;
    while ((newline = buffer.indexOf('\n')) !== -1) {
      const line = buffer.slice(0, newline);
      buffer = buffer.slice(newline + 1);
      if (Buffer.byteLength(line) > MAX_FRAME) { stop(); return; }
      if (!line.trim()) continue;
      let value;
      try { value = JSON.parse(line); } catch { respondError(null, failure('Parse error.', -32700)); continue; }
      receive(value);
    }
    if (Buffer.byteLength(buffer) > MAX_FRAME) stop();
  });
  input.once('end', stop);
  input.once('error', stop);
  output.once('error', stop);
  output.once('close', stop);
  return stop;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const stop = serveMcp(new AppServer(configuration()));
    process.once('SIGINT', stop);
    process.once('SIGTERM', stop);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
