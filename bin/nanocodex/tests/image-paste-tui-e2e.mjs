// Real PTY image-paste journey; only the remote managed service is synthetic.
// Repro: cargo build --locked -p nanocodex2-bin --bin nanocodex2 && node bin/nanocodex/tests/image-paste-tui-e2e.mjs
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';

const require = createRequire(new URL('../../../js/managed/package.json', import.meta.url));
const { WebSocketServer } = require('ws');
const agent = '019fc927-b280-79a7-8445-1b9996ad2fb1';
const key = `ncx_live_${'a'.repeat(12)}_${'b'.repeat(43)}`;
const outputDir = resolve('output/image-paste-tui'); mkdirSync(outputDir, { recursive: true });
const workspace = mkdtempSync(resolve(outputDir, 'run-'));
const route = `/v1/agents/${agent}`;
const requests = [];

const wss = new WebSocketServer({ noServer: true });
const readBody = req => new Promise(done => { let bytes = ''; req.on('data', chunk => bytes += chunk); req.on('end', () => done(bytes ? JSON.parse(bytes) : undefined)); });
const server = createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  const body = req.method === 'POST' ? await readBody(req) : undefined;
  requests.push({ method: req.method, path: url.pathname, body, auth: req.headers.authorization ?? null });
  res.setHeader('content-type', 'application/json');
  const send = (status, value = {}) => { res.statusCode = status; res.end(status === 204 ? undefined : JSON.stringify(value)); };
  if (req.headers.authorization !== `Bearer ${key}`) return send(401, { error: 'unauthorized' });
  if (url.pathname === route) return send(200, { agent_id: agent, session_id: agent, has_snapshot: false,
    completed_turns: 1, last_active: 1, agent_loaded: true, connected_clients: 1, active_turns: [], active_turn_details: [],
    capabilities: { durable_turns: true, resumable_events: true, workspace: 'cloudflare-computer', execution_environments: true, execution_namespace: 'cwd-root-v1', native_cross_mounts: false },
    settings: { model: 'gpt-6-astra', thinking: 'low', reasoning_mode: 'standard', fast_mode: false },
    latest_event_cursor: '0', stream_error: null });
  if (url.pathname === `${route}/events/history`) return send(200, { data: [], has_more: false, latest_cursor: '0' });
  return send(404, { error: 'not_found' });
});
server.on('upgrade', (req, socket, head) => {
  requests.push({ method: 'WS', path: new URL(req.url, 'http://localhost').pathname, auth: req.headers.authorization ?? null });
  if (req.url?.split('?')[0] !== `${route}/ws` || req.headers.authorization !== `Bearer ${key}`) return socket.destroy();
  wss.handleUpgrade(req, socket, head, ws => {
    ws.on('message', bytes => {
      const body = JSON.parse(bytes.toString());
      if (body.type !== 'prompt') return;
      requests.push({ method: 'PROMPT', body });
      const cursor = requests.filter(r => r.method === 'PROMPT').length * 2;
      ws.send(JSON.stringify({ cursor: String(cursor), type: 'turn_accepted', id: body.id, turn_id: body.id, input: body.input, replayed: false }));
      ws.send(JSON.stringify({ cursor: String(cursor + 1), type: 'turn_completed', id: body.id, turn_id: body.id, final_message: 'Received', usage: null, citations: [] }));
    });
    ws.send(JSON.stringify({ type: 'ready', session_id: agent, restored: false, active_turns: [], active_turn_details: [], latest_event_cursor: '0', capabilities: { durable_turns: true, resumable_events: true, workspace: 'cloudflare-computer', execution_environments: true, execution_namespace: 'cwd-root-v1', native_cross_mounts: false }, settings: { model: 'gpt-6-astra', thinking: 'low', reasoning_mode: 'standard', fast_mode: false } }));
  });
});
await new Promise(done => server.listen(0, '127.0.0.1', done));
const origin = `http://127.0.0.1:${server.address().port}`;

const imagePath = resolve(workspace, 'image with spaces (1).png');
// A real, decodable one-pixel PNG, also checked at the outbound WebSocket boundary.
writeFileSync(imagePath, Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==', 'base64'));
// 12 MP noisy image: its clipboard RGBA representation exceeds the Worker's
// 40 MiB decode budget. Generate a real BMP without an image-tool dependency.
const largePath = resolve(workspace, 'large-photo.bmp');
const bmp = Buffer.alloc(54 + 4000 * 3000 * 3);
bmp.write('BM'); bmp.writeUInt32LE(bmp.length, 2); bmp.writeUInt32LE(54, 10);
bmp.writeUInt32LE(40, 14); bmp.writeInt32LE(4000, 18); bmp.writeInt32LE(3000, 22);
bmp.writeUInt16LE(1, 26); bmp.writeUInt16LE(24, 28);
let seed = 42;
for (let i = 54; i < bmp.length; i++) { seed ^= seed << 13; seed ^= seed >>> 17; seed ^= seed << 5; bmp[i] = seed & 255; }
writeFileSync(largePath, bmp);
const invalidPath = resolve(workspace, 'invalid.png');
writeFileSync(invalidPath, 'not an image');
const trace = { command: 'NANOCODEX_TEST_BINARY=target/debug/nanocodex2 node bin/nanocodex/tests/image-paste-tui-e2e.mjs',
  expected: 'Pasted image filenames become image content; ordinary text and missing paths remain text', stages: [] };
let terminal;
let screen = '', stderr = '';
try {
  terminal = spawn('python3', [new URL('./share-pty-bridge.py', import.meta.url).pathname,
    resolve(process.env.NANOCODEX_TEST_BINARY ?? 'target/debug/nanocodex2'), 'attach', agent], {
    cwd: workspace, env: { ...process.env, HOME: workspace, NC_API_KEY: '', CODEX_HOME: resolve(workspace, '.codex'), NANOCODEX_RELOAD_DIR: resolve(workspace, '.reload'),
      NANOCODEX_DISABLE_HAND: '1', NANOCODEX_COMPUTER: 'off', NANOCODEX_MANAGED_URL: origin,
      NANOCODEX_API_KEY: key, TERM: 'xterm-256color', SSH_TTY: '/dev/synthetic-pty', TMUX: '', TMUX_PANE: '' },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  terminal.stdout.on('data', bytes => { screen += bytes; });
  terminal.stderr.on('data', bytes => { stderr += bytes; });
  const wait = async (predicate, stage) => { const deadline = Date.now() + 15000;
    while (!predicate()) { if (terminal.exitCode !== null || Date.now() >= deadline) throw new Error(`${stage}: PTY exit ${terminal.exitCode}; ${stderr}; output tail: ${screen.slice(-1800)}`); await new Promise(done => setTimeout(done, 25)); }
  };
  await wait(() => requests.some(r => r.method === 'WS') && requests.filter(r => r.method === 'GET' && r.path === route).length >= 2, 'attached');
  const turns = () => requests.filter(r => r.method === 'PROMPT');
  const cases = [
    [imagePath, true],
    [imagePath.replaceAll(/([ ()])/g, String.fromCharCode(92) + '$1') + ' also this image', true, undefined, ' also this image'],
    ["'" + imagePath + "' caption with apostrophe's text", true, undefined, " caption with apostrophe's text"],
    [imagePath, true, ' describe this image'],
    [largePath, true],
    ["'" + imagePath + "'", true],
    [imagePath.replaceAll(' ', String.fromCharCode(92) + ' '), true],
    [new URL('file://' + imagePath).href, true],
    ['ordinary pasted text', false],
    [invalidPath, false],
    ['https://example.invalid/image.png', false],
    [resolve(workspace, 'missing.png'), false],
    ['Please inspect ' + imagePath, false],
  ];
  for (const [input, isImage, caption, pastedCaption] of cases) {
    const before = turns().length;
    terminal.stdin.write(`\x1b[200~${input}\x1b[201~`);
    await new Promise(done => setTimeout(done, 250));
    if (caption) terminal.stdin.write(`\x1b[200~${caption}\x1b[201~`);
    terminal.stdin.write('\r');
    await wait(() => turns().length > before, `submit ${input}`);
    const body = turns().at(-1).body;
    if (isImage) {
      const content = body.input;
      assert.ok(Array.isArray(content), JSON.stringify(body));
      if (pastedCaption) assert.ok(content.some(part => part.type === 'text' && part.text === pastedCaption), 'preserve same-paste caption exactly');
      if (caption) assert.ok(content.some(part => part.type === 'text' && part.text.includes(caption.trim())));
      const image = content.find(part => part.type === 'image');
      assert.ok(image?.image_url.startsWith('data:image/png;base64,'), JSON.stringify(body));
      const png = Buffer.from(image.image_url.split(',')[1], 'base64');
      assert.equal(png.subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
      assert.ok(png.length <= 4 * 1024 * 1024, 'encoded image byte budget');
      assert.ok(Math.max(png.readUInt32BE(16), png.readUInt32BE(20)) <= 2048, 'decoded image dimension budget');
      if (input === largePath) assert.ok(Math.abs(png.readUInt32BE(16) / png.readUInt32BE(20) - 4 / 3) < 0.01, 'preserve the photo aspect ratio');
    } else {
      assert.ok(JSON.stringify(body.input).includes(input), JSON.stringify(body));
      assert.ok(!JSON.stringify(body.input).includes('data:image/'));
    }
    trace.stages.push({ input, isImage, caption, body, observed: 'passed' });
    await new Promise(done => setTimeout(done, 250));
  }
  assert.equal(requests.filter(r => r.method === 'WS').length, 1, 'all submissions remain on one usable connection');
  assert.equal(turns().length, cases.length, 'each paste is submitted exactly once');
  trace.result = 'passed';
  console.log(`PASS: ${trace.stages.length} real PTY paste/submission journeys; evidence ${workspace}`);
} finally {
  terminal?.kill('SIGTERM');
  for (const client of wss.clients) client.terminate();
  wss.close(); server.close();
  writeFileSync(resolve(workspace, 'trace.json'), JSON.stringify({ ...trace, requests }, null, 2));
  writeFileSync(resolve(workspace, 'terminal.log'), screen + stderr);
}
