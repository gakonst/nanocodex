// Loopback-only synthetic transport. Input is recorded in memory, never injected.
// Start explicitly for NANOCODEX_SCREEN_FIXTURE=1 UI tests.
import { createRequire } from 'node:module';
import { createServer } from 'node:http';
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
const require = createRequire(new URL('../../../../js/desktop-runtime/package.json', import.meta.url));
const { WebSocketServer } = require('ws');
const jpeg = readFileSync(new URL('./screen.jpg', import.meta.url)).toString('base64');
const surface = { id: 'desktop', machine_id: 'fixture', machine_name: 'Local workspace', name: 'Dashboard', kind: 'vm', width: 960, height: 600, controllable: false, generation: 'fixture-1', transport: 'frames-v1' };
const controller = { ...surface, id: 'controller', name: 'Synthetic controller', controllable: true };
const events = [];
const connections = new Map();
let cursor = 0;
function record(connection, event) {
  events.push({ ...event, connection, cursor: ++cursor });
  if (events.length > 512) events.shift();
}
const server = createServer((req, res) => {
  const url = new URL(req.url, 'http://127.0.0.1');
  res.setHeader('Content-Type', 'application/json');
  res.setHeader('Cache-Control', 'no-store');
  if (req.method === 'POST' && url.pathname.endsWith('/renew')) {
    let body = '';
    req.on('data', chunk => { body += chunk; if (body.length > 1024) req.destroy(); });
    req.on('end', () => {
      try {
        const socket = connections.get(JSON.parse(body).connection_id);
        if (socket?.readyState === 1) socket.send(JSON.stringify({ type: 'renewed' }));
        res.end('{}');
      } catch { res.statusCode = 400; res.end('{}'); }
    });
    return;
  }
  if (req.method === 'GET' && url.pathname === '/fixture/events') {
    const after = Number(url.searchParams.get('after') || 0);
    res.end(JSON.stringify({ cursor, events: events.filter(event => event.cursor > after) }));
  } else res.end(JSON.stringify(url.pathname.endsWith('/screens') ? { surfaces: [surface, controller] } : {}));
});
const sockets = new WebSocketServer({ server, maxPayload: 8192 });
sockets.on('connection', (socket, req) => {
  const connection = randomUUID();
  connections.set(connection, socket);
  const synthetic = new URL(req.url, 'http://127.0.0.1').searchParams.get('surface_id') === 'controller';
  let generation;
  let lastSequence = 0;
  const send = message => socket.send(JSON.stringify(message));
  send({ type: 'ready', connection_id: connection });
  socket.on('message', value => {
    let message;
    try { message = JSON.parse(value); } catch { socket.close(1003); return; }
    if (message.type === 'frame_request') send({ type: 'frame', width: 960, height: 600, jpeg });
    if (message.type === 'ping') send({ type: 'pong' });
    if (message.type === 'control') {
      const control = message.data;
      if (!synthetic || !control) { send({ type: 'control', data: { type: 'denied' } }); return; }
      if (control.type === 'acquire' && !generation) {
        generation = randomUUID(); lastSequence = 0;
        record(connection, { type: 'acquire', generation });
        send({ type: 'control', data: { type: 'granted', generation, relativePointer: true } });
      } else if (generation && control.generation === generation) {
        if (control.type === 'renew') record(connection, { type: 'renew', generation });
        if (control.type === 'release') {
          record(connection, { type: 'release', generation });
          generation = undefined;
          send({ type: 'control', data: { type: 'revoked' } });
        }
      }
    }
    if (message.type === 'input' && synthetic && generation) {
      const event = message.data;
      if (!event || event.generation !== generation || !Number.isSafeInteger(event.sequence) || event.sequence <= lastSequence) return;
      if (!['move', 'relativeMove', 'button', 'scroll', 'key', 'text', 'releaseAll'].includes(event.kind)) return;
      lastSequence = event.sequence;
      // Bound both count and payload, including text. No OS input APIs exist here.
      const input = Object.fromEntries(Object.entries(event).filter(([key]) => ['kind', 'sequence', 'generation', 'x', 'y', 'button', 'down', 'key', 'deltaX', 'deltaY'].includes(key)));
      if (typeof event.text === 'string') input.text = event.text.slice(0, 128);
      record(connection, { type: 'input', input });
    }
  });
  socket.on('close', () => {
    connections.delete(connection);
    if (synthetic) record(connection, { type: 'disconnect', generation: generation ?? null });
  });
});
server.listen(18965, '127.0.0.1', () => console.log('Synthetic screen fixture: http://127.0.0.1:18965'));
