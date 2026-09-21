import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { createHash } from 'node:crypto';
import { PassThrough } from 'node:stream';
import { spawn } from 'node:child_process';
import { AppServer, configuration, serveMcp } from '../openai-cua-app-server.mjs';

// Minimal synthetic server for actual Node WebSocket traffic (no npm packages).
async function fixture(t, handler) {
  const sockets = new Set();
  const messages = [];
  const server = createServer();
  server.on('upgrade', (request, socket) => {
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
    const accept = createHash('sha1').update(request.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
    socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
    const send = value => {
      const data = Buffer.from(JSON.stringify(value));
      const head = data.length < 126 ? Buffer.from([0x81, data.length]) : Buffer.from([0x81, 126, data.length >> 8, data.length & 255]);
      socket.write(Buffer.concat([head, data]));
    };
    let buffer = Buffer.alloc(0);
    socket.on('data', data => {
      buffer = Buffer.concat([buffer, data]);
      while (buffer.length >= 2) {
        const opcode = buffer[0] & 15;
        let size = buffer[1] & 127;
        let offset = 2;
        if (size === 126) { if (buffer.length < 4) return; size = buffer.readUInt16BE(2); offset = 4; }
        else if (size === 127) { if (buffer.length < 10) return; size = Number(buffer.readBigUInt64BE(2)); offset = 10; }
        if (buffer.length < offset + 4 + size) return;
        const mask = buffer.subarray(offset, offset + 4);
        const body = Buffer.from(buffer.subarray(offset + 4, offset + 4 + size));
        buffer = buffer.subarray(offset + 4 + size);
        for (let i = 0; i < body.length; i++) body[i] ^= mask[i % 4];
        if (opcode === 8) { socket.end(Buffer.from([0x88, 0])); return; }
        const value = JSON.parse(body.toString());
        messages.push({ socket, value });
        handler(value, { send, socket, reply: result => send({ id: value.id, result }) });
      }
    });
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(async () => {
    for (const socket of sockets) socket.destroy();
    await new Promise(resolve => server.close(resolve));
  });
  return { messages, url: `ws://127.0.0.1:${server.address().port}`, sockets };
}

const tools = [
  { name: 'js', description: 'Exact upstream description', inputSchema: { type: 'object', properties: { code: { type: 'string' } } }, annotations: { readOnlyHint: false }, _meta: { extra: ['keep', 1] }, outputSchema: { type: 'object' } },
  { name: 'turn_ended', inputSchema: {}, _meta: { ui: { visibility: [] } } },
];
const result = { content: [{ type: 'image', data: 'synthetic', mimeType: 'image/png', _meta: { detail: 'original' } }], structuredContent: { kept: true }, _meta: { trace: ['unchanged'] }, isError: false };
function defaults(value, io) {
  if (value.method === 'initialize') io.reply({});
  if (value.method === 'mcpServerStatus/list') io.reply({ data: [{ name: 'cua_repl', tools: Object.fromEntries(tools.map(tool => [tool.name, tool])) }], nextCursor: null });
  if (value.method === 'thread/start') io.reply({ thread: { id: 'synthetic-thread' } });
  if (value.method === 'thread/inject_items') io.reply({});
}
function client(t, url, extra = {}) {
  const app = new AppServer({ url, timeoutMs: 2000, openGui: false, ...extra }, { onThread: () => {} });
  t.after(() => app.close());
  return app;
}
function mcp(t, app) {
  const input = new PassThrough();
  const output = new PassThrough();
  const responses = [];
  let pending = [];
  output.on('data', data => {
    for (const line of data.toString().trim().split('\n')) responses.push(JSON.parse(line));
    for (const resolve of pending) resolve();
    pending = [];
  });
  t.after(serveMcp(app, input, output));
  return {
    input, responses,
    send: (id, method, params) => input.write(JSON.stringify({ jsonrpc: '2.0', ...(id === undefined ? {} : { id }), method, params }) + '\n'),
    async response(id) {
      while (!responses.some(value => value.id === id)) await new Promise(resolve => pending.push(resolve));
      return responses.find(value => value.id === id);
    },
  };
}

// Bound each async test independently so a protocol regression cannot hang CI.
const options = { timeout: 10000 };
test('configuration accepts literal loopback only and rejects credential-bearing URLs', () => {
  for (const url of ['ws://127.0.0.1:1234', 'ws://[::1]:1234']) assert.ok(configuration({ NANOCODEX_CUA_APP_SERVER_WS_URL: url }));
  for (const url of [undefined, 'ws://localhost:1234', 'ws://example.test', 'ws://127.0.0.1.example.test', 'ws://user:secret@127.0.0.1', 'ws://127.0.0.1/?token=secret', 'ws://127.0.0.1/#secret', 'https://127.0.0.1', 'ws://127.0.0.1/path']) {
    assert.throws(() => configuration({ NANOCODEX_CUA_APP_SERVER_WS_URL: url }));
  }
  assert.throws(() => configuration({ NANOCODEX_CUA_APP_SERVER_WS_URL: 'ws://127.0.0.1', NANOCODEX_CUA_APP_SERVER_OPEN_GUI: '1' }, 'linux'));
});

test('stdio entry fails closed without trusted configuration', options, async () => {
  const env = { ...process.env };
  delete env.NANOCODEX_CUA_APP_SERVER_WS_URL;
  const child = spawn(process.execPath, [new URL('../openai-cua-app-server.mjs', import.meta.url).pathname], { env, stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = ''; let stderr = '';
  child.stdout.on('data', data => stdout += data);
  child.stderr.on('data', data => stderr += data);
  const code = await new Promise(resolve => child.once('exit', resolve));
  assert.equal(code, 1); assert.equal(stdout, ''); assert.match(stderr, /NANOCODEX_CUA_APP_SERVER_WS_URL/);
});

test('MCP exact paginated catalog discovery creates no thread and advertises no elicitation', options, async t => {
  const host = await fixture(t, (value, io) => {
    if (value.method !== 'mcpServerStatus/list') return defaults(value, io);
    io.reply(value.params.cursor ? { data: [{ name: 'cua_repl', tools: { js: tools[0], turn_ended: tools[1] } }] } : { data: [{ name: 'unrelated', tools: {} }], nextCursor: 'page-two' });
  });
  const peer = mcp(t, client(t, host.url));
  peer.send(1, 'initialize', { protocolVersion: '2024-11-05', capabilities: { elicitation: {} } });
  assert.deepEqual((await peer.response(1)).result.capabilities, { tools: {} });
  peer.send(undefined, 'notifications/cancelled', {});
  peer.send(2, 'tools/list', {});
  assert.deepEqual((await peer.response(2)).result, { tools });
  assert.deepEqual(host.messages.map(x => x.value.method), ['initialize', 'initialized', 'mcpServerStatus/list', 'mcpServerStatus/list']);
  assert.deepEqual(host.messages[0].value.params.capabilities, { experimentalApi: true });
});

test('catalog order is deterministic across connections without changing definitions', options, async t => {
  let discoveries = 0;
  const host = await fixture(t, (value, io) => {
    if (value.method !== 'mcpServerStatus/list') return defaults(value, io);
    const ordered = ++discoveries % 2 ? [...tools].reverse() : tools;
    io.reply({ data: [{ name: 'cua_repl', tools: Object.fromEntries(ordered.map(tool => [tool.name, tool])) }] });
  });
  const first = client(t, host.url);
  const second = client(t, host.url);
  assert.deepEqual(await first.catalog(), { tools });
  assert.deepEqual(await second.catalog(), { tools });
  assert.deepEqual(await first.catalog(), { tools });
  assert.equal(host.messages.some(x => x.value.method === 'thread/start'), false);
});

test('calls materialize one own thread, preserve metadata/results, and ignore ALL server requests', options, async t => {
  let answered;
  let calls = 0;
  const host = await fixture(t, (value, io) => {
    defaults(value, io);
    if (value.method === 'mcpServer/tool/call') {
      calls++;
      io.send({ id: 'approval', method: 'mcpServer/elicitation/request', params: { secret: 'synthetic' } });
      io.send({ id: 'unsupported', method: 'unknown/request', params: {} });
      io.reply(result);
    }
    if (value.id === 'approval' || value.id === 'unsupported') answered = value;
  });
  const app = client(t, host.url);
  const metadata = { 'x-codex-turn-metadata': { session_id: 'original-session', turn_id: 'original-turn' }, thread_id: 'original-thread', custom: [null, 3] };
  const peer = mcp(t, app);
  peer.send(1, 'initialize', {}); await peer.response(1);
  peer.send(2, 'tools/call', { name: 'js', arguments: { code: 'synthetic' }, _meta: metadata });
  assert.deepEqual((await peer.response(2)).result, result);
  peer.send(3, 'tools/call', { name: 'turn_ended', arguments: {} });
  await peer.response(3);
  assert.equal(calls, 2); assert.equal(answered, undefined);
  const sent = host.messages.map(x => x.value);
  assert.equal(sent.filter(x => x.method === 'thread/start').length, 1);
  assert.deepEqual(sent.find(x => x.method === 'thread/start').params, { ephemeral: false, historyMode: 'paginated', cwd: process.cwd() });
  assert.match(sent.find(x => x.method === 'thread/inject_items').params.items[0].content[0].text, /no user authorization or approval/);
  assert.deepEqual(sent.find(x => x.method === 'mcpServer/tool/call').params, { threadId: 'synthetic-thread', server: 'cua_repl', tool: 'js', arguments: { code: 'synthetic' }, _meta: { ...metadata, thread_id: 'synthetic-thread', threadId: 'synthetic-thread' } });
  assert.deepEqual(sent.filter(x => x.method === 'mcpServer/tool/call')[1].params._meta, { thread_id: 'synthetic-thread', threadId: 'synthetic-thread' });
  assert.equal(peer.responses.length, 3);
});

test('calls serialize; cancellation closes only this connection and never retries', options, async t => {
  let threads = 0;
  let releaseFirst;
  const firstReceived = new Promise(resolve => releaseFirst = resolve);
  const host = await fixture(t, (value, io) => {
    if (value.method === 'thread/start') return io.reply({ thread: { id: `thread-${++threads}` } });
    defaults(value, io);
    if (value.method === 'mcpServer/tool/call') {
      if (value.params.arguments.hold) releaseFirst();
      else io.reply(result);
    }
  });
  const a = mcp(t, client(t, host.url));
  const b = mcp(t, client(t, host.url));
  for (const peer of [a, b]) { peer.send(1, 'initialize', {}); await peer.response(1); }
  a.send(2, 'tools/call', { name: 'js', arguments: { hold: true } });
  await firstReceived;
  a.send(3, 'tools/call', { name: 'js', arguments: { queued: true } });
  b.send(2, 'tools/call', { name: 'js', arguments: {} });
  assert.deepEqual((await b.response(2)).result, result);
  a.send(undefined, 'notifications/cancelled', { requestId: 2 });
  assert.equal((await a.response(2)).error.code, -32800);
  assert.ok((await a.response(3)).error);
  b.send(3, 'tools/call', { name: 'js', arguments: {} });
  assert.deepEqual((await b.response(3)).result, result);
  const calls = host.messages.filter(x => x.value.method === 'mcpServer/tool/call');
  assert.deepEqual(calls.map(x => x.value.params.threadId), ['thread-1', 'thread-2', 'thread-2']);
  assert.equal(new Set(calls.map(x => x.socket)).size, 2);
  assert.equal(host.messages.some(x => /interrupt|archive|shutdown|reset/.test(x.value.method ?? '')), false);
});

test('queued cancellation does not disconnect active work; remote disconnect fails without replay', options, async t => {
  let release;
  let ioCall;
  const received = new Promise(resolve => release = resolve);
  const host = await fixture(t, (value, io) => {
    defaults(value, io);
    if (value.method === 'mcpServer/tool/call') { ioCall = io; release(); }
  });
  const peer = mcp(t, client(t, host.url));
  peer.send(1, 'initialize', {}); await peer.response(1);
  peer.send(2, 'tools/call', { name: 'js', arguments: {} }); await received;
  peer.send(3, 'tools/call', { name: 'js', arguments: {} });
  peer.send(undefined, 'notifications/cancelled', { requestId: 3 });
  assert.equal((await peer.response(3)).error.code, -32800);
  ioCall.socket.destroy();
  assert.match((await peer.response(2)).error.message, /disconnected|connection failed/);
  peer.send(4, 'tools/call', { name: 'js', arguments: {} });
  assert.ok((await peer.response(4)).error);
  assert.equal(host.messages.filter(x => x.value.method === 'mcpServer/tool/call').length, 1);
});

test('upstream errors retain data; timeout and EOF close only owned transport', options, async t => {
  let held;
  const received = new Promise(resolve => held = resolve);
  const host = await fixture(t, (value, io) => {
    defaults(value, io);
    if (value.method === 'mcpServer/tool/call') {
      if (value.params.arguments.error) io.send({ id: value.id, error: { code: -32042, message: 'synthetic upstream error', data: { preserved: true } } });
      else held(io);
    }
  });
  const app = client(t, host.url, { timeoutMs: 100 });
  const peer = mcp(t, app);
  peer.send(1, 'initialize', {}); await peer.response(1);
  peer.send(2, 'tools/call', { name: 'js', arguments: { error: true } });
  assert.deepEqual((await peer.response(2)).error, { code: -32042, message: 'synthetic upstream error', data: { preserved: true } });
  peer.send(3, 'tools/call', { name: 'js', arguments: {} }); await received;
  assert.match((await peer.response(3)).error.message, /timed out/);
  const other = client(t, host.url);
  const otherPeer = mcp(t, other);
  otherPeer.send(1, 'initialize', {}); await otherPeer.response(1);
  otherPeer.send(2, 'tools/list', {}); await otherPeer.response(2);
  otherPeer.input.end();
  await new Promise(resolve => setImmediate(resolve));
  assert.ok(other.closed);
  assert.equal(host.messages.some(x => /interrupt|archive|shutdown/.test(x.value.method ?? '')), false);
});

test('GUI opens only after thread materialization and before first forwarded call', options, async t => {
  const order = [];
  const host = await fixture(t, (value, io) => {
    order.push(value.method);
    defaults(value, io);
    if (value.method === 'mcpServer/tool/call') io.reply(result);
  });
  const app = new AppServer({ url: host.url, timeoutMs: 2000, openGui: true }, {
    onThread: () => {},
    openGui: async id => { assert.equal(id, 'synthetic-thread'); order.push('open-gui'); },
  });
  t.after(() => app.close());
  await app.catalog();
  assert.equal(order.includes('open-gui'), false);
  assert.deepEqual(await app.call({ name: 'js', _meta: null }), result);
  assert.deepEqual(order.slice(-4), ['thread/start', 'thread/inject_items', 'open-gui', 'mcpServer/tool/call']);
  assert.deepEqual(host.messages.at(-1).value.params._meta, { thread_id: 'synthetic-thread', threadId: 'synthetic-thread' });
});

test('failed materialization cannot reuse a partially initialized thread or replay the call', options, async t => {
  const host = await fixture(t, (value, io) => {
    if (value.method === 'thread/inject_items') return io.send({ id: value.id, error: { code: -32001, message: 'synthetic append failure' } });
    defaults(value, io);
  });
  const app = client(t, host.url);
  await assert.rejects(app.call({ name: 'js' }), /synthetic append failure/);
  await assert.rejects(app.call({ name: 'js' }), /synthetic append failure/);
  assert.equal(host.messages.filter(x => x.value.method === 'thread/inject_items').length, 1);
  assert.equal(host.messages.some(x => x.value.method === 'mcpServer/tool/call'), false);
});
