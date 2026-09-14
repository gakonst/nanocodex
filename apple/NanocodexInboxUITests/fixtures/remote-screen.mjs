// Local view-only transport fixture. Reuses the workspace's existing ws package.
import { createRequire } from 'node:module';
import { createServer } from 'node:http';
import { readFileSync } from 'node:fs';
const require = createRequire(new URL('../../../../js/desktop-runtime/package.json', import.meta.url));
const { WebSocketServer } = require('ws');
const jpeg = readFileSync(new URL('./screen.jpg', import.meta.url)).toString('base64');
const surface = { id: 'desktop', machine_id: 'fixture', machine_name: 'Local workspace', name: 'Dashboard', kind: 'vm', width: 960, height: 600, controllable: false, generation: 'fixture-1', transport: 'frames-v1' };
const server = createServer((req, res) => {
  res.setHeader('Content-Type', 'application/json');
  res.end(JSON.stringify(req.url.endsWith('/screens') ? { surfaces: [surface] } : {}));
});
const sockets = new WebSocketServer({ server });
sockets.on('connection', socket => {
  socket.send(JSON.stringify({ type: 'ready', connection_id: 'fixture' }));
  socket.on('message', value => {
    const message = JSON.parse(value);
    if (message.type === 'frame_request') socket.send(JSON.stringify({ type: 'frame', width: 960, height: 600, jpeg }));
    if (message.type === 'ping') socket.send(JSON.stringify({ type: 'pong' }));
  });
});
server.listen(18965, '127.0.0.1', () => console.log('View-only screen fixture: http://127.0.0.1:18965'));
