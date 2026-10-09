// Native owner-key + PTY /sites journey against a local synthetic managed transport.
// Repro: cargo build -p nanocodex-bin --bins && node bin/nanocodex/tests/sites-tui-e2e.mjs
// The service is a fixture, not production authorization or storage; run
// `pnpm --filter nanocodex-managed-service run test:sites` for the real Worker,
// Durable Object, R2, and Sites Worker boundary.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';

const require = createRequire(new URL('../../../js/managed/package.json', import.meta.url));
const { WebSocketServer } = require('ws');
const agent = '019fc927-b280-79a7-8445-1b9996ad2fb1';
const key = `ncx_live_${'a'.repeat(12)}_${'b'.repeat(43)}`;
const shareId = '00000000-0000-4000-8000-000000000002';
const shareUrl = `https://${'k'.repeat(26)}.sites.example/`;
const viewUrl = `https://${'v'.repeat(26)}.sites.example/`;
const outputDir = resolve('output/sites-tui'); mkdirSync(outputDir, { recursive: true });
const workspace = mkdtempSync(resolve(outputDir, 'run-'));
const route = `/v1/agents/${agent}`;
const requests = [];
let published = false;
let active = false;
const wss = new WebSocketServer({ noServer: true });
const readBody = req => new Promise(done => { let bytes = ''; req.on('data', chunk => bytes += chunk); req.on('end', () => done(bytes ? JSON.parse(bytes) : undefined)); });
const site = () => ({ id: 'launch', title: 'Launch page', latest_version: 1, created_at: 1, updated_at: 1,
  versions: [{ version: 1, entry: 'index.html', files: 4, bytes: 2048, source: '/workspace/app/dist', created_at: 1 }],
  shares: active ? [{ id: shareId, site_id: 'launch', version: 1, url: shareUrl, created_at: 2, expires_at: null }] : [] });
const server = createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  const body = req.method === 'POST' ? await readBody(req) : undefined;
  requests.push({ method: req.method, path: url.pathname, body, auth: req.headers.authorization ?? null });
  res.setHeader('content-type', 'application/json');
  const send = (status, value = {}) => { res.statusCode = status; res.end(status === 204 ? undefined : JSON.stringify(value)); };
  if (req.headers.authorization !== `Bearer ${key}`) return send(401, { error: 'unauthorized' });
  if (url.pathname === `${route}/sites` && req.method === 'POST') {
    if (body.path === '/workspace/missing') return send(404, { error: 'site_source_not_found', message: '/workspace/missing does not exist' });
    assert.deepEqual(body, { path: '/workspace/app/dist', id: 'launch' });
    published = true;
    return send(201, { type: 'nanocodex.site', site_id: 'launch', title: 'Launch page', version: 1, entry: 'index.html', files: 4, bytes: 2048, excluded: 2, created: true });
  }
  if (url.pathname === `${route}/sites` && req.method === 'GET') return send(200, { data: published ? [site()] : [] });
  if (url.pathname === `${route}/sites/launch/open` && req.method === 'POST') return send(200, { site_id: 'launch', version: 1, url: viewUrl, expires_at: Date.now() + 3_600_000 });
  if (url.pathname === `${route}/sites/launch/shares` && req.method === 'POST') {
    assert.deepEqual(body, {});
    active = true;
    return send(201, { id: shareId, site_id: 'launch', version: 1, url: shareUrl, created_at: 2, expires_at: null });
  }
  if (url.pathname === `${route}/sites/launch/shares/${shareId}` && req.method === 'DELETE') {
    if (!active) return send(404, { error: 'not_found', message: 'Site link not found' });
    active = false; return send(204);
  }
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
    ws.send(JSON.stringify({ type: 'ready', session_id: agent, restored: false, active_turns: [], active_turn_details: [], latest_event_cursor: '0', capabilities: { durable_turns: true, resumable_events: true, workspace: 'cloudflare-computer', execution_environments: true, execution_namespace: 'cwd-root-v1', native_cross_mounts: false }, settings: { model: 'gpt-6-astra', thinking: 'low', reasoning_mode: 'standard', fast_mode: false } }));
  });
});
await new Promise(done => server.listen(0, '127.0.0.1', done));
const origin = `http://127.0.0.1:${server.address().port}`;
// A recording opener stands in for the system browser so the journey has no desktop side effect.
const bin = resolve(workspace, 'bin'); mkdirSync(bin);
const opened = resolve(workspace, 'opened.txt');
for (const name of ['open', 'xdg-open']) {
  writeFileSync(resolve(bin, name), `#!/bin/sh\nprintf '%s\\n' "$1" >> '${opened}'\n`);
  chmodSync(resolve(bin, name), 0o755);
}
const trace = { command: 'cargo build -p nanocodex-bin --bins && node bin/nanocodex/tests/sites-tui-e2e.mjs',
  expected: 'TUI /sites publish, open, share, list, revoke reach the owner routes; slash commands never become turns', stages: [] };
let terminal;
try {
  terminal = spawn('python3', [new URL('./share-pty-bridge.py', import.meta.url).pathname, resolve('target/debug/nanocodex'), 'attach', agent], {
    cwd: workspace, env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, HOME: workspace, NC_API_KEY: '', CODEX_HOME: resolve(workspace, '.codex'), NANOCODEX_RELOAD_DIR: resolve(workspace, '.reload'),
      NANOCODEX_DISABLE_HAND: '1', NANOCODEX_COMPUTER: 'off', NANOCODEX_MANAGED_URL: origin,
      NANOCODEX_API_KEY: key, TERM: 'xterm-256color', SSH_TTY: '/dev/synthetic-pty', TMUX: '', TMUX_PANE: '' },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  let screen = ''; let stderr = '';
  terminal.stdout.on('data', bytes => { screen += bytes; });
  terminal.stderr.on('data', bytes => { stderr += bytes; });
  const wait = async (predicate, stage) => { const deadline = Date.now() + 15000;
    while (!predicate()) { if (terminal.exitCode !== null || Date.now() >= deadline) throw new Error(`${stage}: PTY exit ${terminal.exitCode}; ${stderr}; output tail: ${screen.slice(-1800)}`); await new Promise(done => setTimeout(done, 25)); }
  };
  // Cursor movement replaces the spaces between words, so match against visible text.
  const visible = () => screen.replace(/\x1b\[[0-9;?]*[ -\/]*[@-~]/g, ' ').replace(/\s+/g, ' ');
  const sent = (method, path) => requests.some(r => r.method === method && r.path === path);
  const enter = command => terminal.stdin.write(`\x1b[200~${command}\x1b[201~\r`);
  const close = async () => { terminal.stdin.write('\x1b'); await new Promise(done => setTimeout(done, 150)); };
  await wait(() => sent('WS', `${route}/ws`) && requests.filter(r => r.method === 'GET' && r.path === route).length >= 2, 'attached');
  await new Promise(done => setTimeout(done, 100));

  enter('/sites publish /workspace/missing');
  await wait(() => visible().includes('/workspace/missing does not exist'), 'publish failure');
  enter('/sites publish /workspace/app/dist launch');
  await wait(() => visible().includes('Published launch v1'), 'publish receipt');
  trace.stages.push({ action: 'publish', receipts: ['404 site_source_not_found', '201 v1'] });

  enter('/sites open launch');
  await wait(() => existsSync(opened) && readFileSync(opened, 'utf8').includes(viewUrl), 'open private view');
  trace.stages.push({ action: 'open', opened: readFileSync(opened, 'utf8').trim() });

  enter('/sites share launch');
  await wait(() => visible().includes(shareUrl) && visible().includes('Sites · managed thread'), 'share receipt');
  await close();
  enter('/sites');
  await wait(() => visible().includes(shareId) && visible().includes('Launch page'), 'list');
  await close();
  trace.stages.push({ action: 'share and list', shareId });

  enter(`/sites revoke launch ${shareId}`);
  await wait(() => sent('DELETE', `${route}/sites/launch/shares/${shareId}`) && visible().includes('Site link revoked'), 'revoke');
  enter('/sites share Not-A-Site');
  await wait(() => visible().includes('Usage: /sites'), 'invalid command rejected locally');
  trace.stages.push({ action: 'revoke and reject invalid input' });

  assert.equal(active, false);
  assert.equal(requests.some(r => /\/turns(?:\/|$)/.test(r.path)), false, 'slash commands must not become turns');
  assert.equal(requests.filter(r => r.path.endsWith('/shares')).length, 1, 'a share is created exactly once');
  assert.equal(requests.some(r => r.path.includes('Not-A-Site')), false, 'invalid IDs never reach the service');
  assert.ok(requests.filter(r => r.path.includes('/sites')).every(r => r.auth === `Bearer ${key}`));
  console.log('Native PTY /sites journey passed.');
} finally {
  trace.requests = requests.map(({ auth, ...request }) => ({ ...request, auth: auth === `Bearer ${key}` ? 'owner' : 'none' }));
  writeFileSync(resolve(outputDir, 'trace.json'), JSON.stringify(trace, null, 2) + '\n');
  if (terminal) { terminal.stdin.end(); await Promise.race([new Promise(done => terminal.once('close', done)), new Promise(done => setTimeout(done, 1500))]); if (terminal.exitCode === null) terminal.kill(); }
  for (const client of wss.clients) client.terminate();
  server.closeAllConnections(); server.close(); rmSync(workspace, { recursive: true, force: true });
}
