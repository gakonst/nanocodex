// End-to-end CLI boundary: real child process -> local HTTP/WebSocket API.
// Run after: cargo build -p nanocodex2-bin --bin nanocodex2
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createServer } from 'node:http';
import { resolve } from 'node:path';

const binary = resolve(process.argv[2] || 'target/debug/nanocodex2');
const key = `ncx2_${'A'.repeat(43)}`;
const accepted = new Map();
const cursors = [];
let unauthorized = 0;
const server = createServer(async (request, response) => {
  if (request.headers.authorization !== `Bearer ${key}`) {
    unauthorized++;
    response.writeHead(401).end();
    return;
  }
  const url = new URL(request.url, 'http://localhost');
  response.setHeader('content-type', 'application/json');
  if (request.method === 'POST') {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const body = JSON.parse(Buffer.concat(chunks).toString());
    const id = request.headers['idempotency-key'];
    assert.match(id, /^[0-9a-f-]{36}$/);
    assert.equal(body.input, 'hello');
    if (url.pathname === '/v1/agents') {
      accepted.set(id, { id, state: 'accepted' });
      response.writeHead(202).end(JSON.stringify({ agent_id: id, turn_id: id, state: 'accepted' }));
      return;
    }
  }
  const match = /^\/v1\/agents\/([0-9a-f-]{36})\/turns\/([0-9a-f-]{36})$/.exec(url.pathname);
  if (match && accepted.has(match[2])) {
    response.writeHead(200).end(JSON.stringify({ turn_id: match[2], state: accepted.get(match[2]).state,
      message: accepted.get(match[2]).state === 'completed' ? 'Hello world' : undefined }));
    return;
  }
  response.writeHead(404).end('{}');
});
const sockets = new Set();
function send(socket, opcode, payload) {
  const bytes = Buffer.from(payload);
  const header = bytes.length < 126 ? Buffer.from([0x80 | opcode, bytes.length])
    : Buffer.from([0x80 | opcode, 126, bytes.length >> 8, bytes.length & 255]);
  socket.write(Buffer.concat([header, bytes]));
}
server.on('upgrade', (request, socket) => {
  socket.on('error', () => {}); // Peer may reset after the 1013 replay fence.
  if (request.headers.authorization !== `Bearer ${key}`) { socket.destroy(); return; }
  const url = new URL(request.url, 'http://localhost');
  const match = /^\/v1\/agents\/([0-9a-f-]{36})\/events$/.exec(url.pathname);
  if (!match || !accepted.has(match[1])) { socket.destroy(); return; }
  const cursor = url.searchParams.get('cursor');
  cursors.push(cursor);
  const accept = createHash('sha1').update(`${request.headers['sec-websocket-key']}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`).digest('base64');
  socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
  sockets.add(socket);
  socket.on('close', () => sockets.delete(socket));
  const id = match[1];
  const event = (n, type, payload) => send(socket, 1, JSON.stringify({ cursor: String(n), event: { type, payload } }));
  if (cursor === '0') {
    event(1, 'input.accepted', { request_id: id, turn_id: 'internal-turn' });
    event(2, 'assistant.delta', { turn_id: 'internal-turn', phase: 'final_answer', text: 'Hello ' });
    send(socket, 1, JSON.stringify({ type: 'replay_paused', cursor: '2', latest_cursor: '4' }));
    send(socket, 8, Buffer.from([0x03, 0xf5])); // 1013: replay continuation required
    socket.end();
  } else if (cursor === '2') {
    event(3, 'assistant.delta', { turn_id: 'internal-turn', phase: 'final_answer', text: 'world' });
    event(4, 'run.completed', { turn_id: 'internal-turn' });
    accepted.get(id).state = 'completed';
  } else { socket.destroy(); }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const url = `http://127.0.0.1:${server.address().port}`;
async function run(args, credential = key) {
  const child = spawn(binary, args, { env: { ...process.env, NANOCODEX_MANAGED2_URL: url,
    NANOCODEX_MANAGED2_API_KEY: credential }, stdio: ['ignore', 'pipe', 'pipe'] });
  let out = '', err = '';
  child.stdout.on('data', chunk => { out += chunk; });
  child.stderr.on('data', chunk => { err += chunk; });
  const timer = setTimeout(() => child.kill(), 15_000);
  const code = await new Promise((resolve, reject) => { child.on('error', reject); child.on('exit', resolve); });
  clearTimeout(timer);
  return { code, out, err };
}
try {
  const denied = await run(['--managed2', 'run', 'hello'], `ncx2_${'B'.repeat(43)}`);
  assert.notEqual(denied.code, 0);
  assert.equal(unauthorized, 1, denied.err);
  const unsupported = await run(['--managed2', 'list']);
  assert.notEqual(unsupported.code, 0);
  assert.match(unsupported.err, /unavailable/);
  const ok = await run(['--managed2', 'run', 'hello']);
  assert.equal(ok.code, 0, ok.err);
  assert.match(ok.out, /Hello world/);
  assert.deepEqual(cursors, ['0', '2']);
  assert.match(ok.err, /Managed2 agent: [0-9a-f-]{36}/);
  console.log('Managed2 CLI E2E passed: auth failure, unsupported command, create, event replay continuation, durable result.');
} finally {
  for (const socket of sockets) socket.destroy();
  server.closeAllConnections(); server.close();
}
