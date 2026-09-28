// Native owner-key + PTY revoke journey against a local synthetic managed transport.
// Repro: cargo build -p nanocodex2-bin --bin nanocodex2 && node bin/nanocodex/tests/thread-share-tui-e2e.mjs
// The service is a fixture, not production authorization; run js/managed/test/thread-share-links.test.ts
// separately for the real Worker/DO authorization and storage boundary.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdirSync, writeFileSync, mkdtempSync, rmSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';
const require = createRequire(new URL('../../../package.json', import.meta.url));
const { WebSocketServer } = require('ws');
const agent = '019fc927-b280-79a7-8445-1b9996ad2fb0';
const linkId = '00000000-0000-4000-8000-000000000001';
const key = `ncx_live_${'a'.repeat(12)}_${'b'.repeat(43)}`;
const token = `nsl_${'c'.repeat(43)}`;
const outputDir = resolve('output/thread-share-tui'); mkdirSync(outputDir, { recursive: true });
const workspace = mkdtempSync(resolve(outputDir, 'run-'));
const route = `/v1/agents/${agent}`;
const requests = [];
let active = false;
const wss = new WebSocketServer({ noServer: true });
const server = createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  requests.push({ method: req.method, path: url.pathname, auth: req.headers.authorization ?? null });
  res.setHeader('content-type', 'application/json');
  const send = (status, value = {}) => { res.statusCode = status; res.end(status === 204 ? undefined : JSON.stringify(value)); };
  if (url.pathname.startsWith('/v1/shared/')) {
    if (!active || req.headers.authorization !== `Bearer ${token}`) return send(404, { error: 'not_found' });
    if (url.pathname === `/v1/shared/${agent}`) return send(200, { agent_id: agent, permission: 'read', title: 'Synthetic handoff' });
    if (url.pathname === `/v1/shared/${agent}/events/history`) return send(200, { data: [
      { cursor: '1', type: 'turn_accepted', id: 'turn-1', input: 'Synthetic owner prompt' },
      { cursor: '2', type: 'turn_completed', id: 'turn-1', final_message: 'Synthetic answer' },
    ], has_more: false, next_cursor: null });
    return send(404, { error: 'not_found' });
  }
  if (req.headers.authorization !== `Bearer ${key}`) return send(401, { error: 'unauthorized' });
  if (url.pathname === `${route}/share-links` && req.method === 'POST') {
    const body = await new Promise(resolve => { let bytes = ''; req.on('data', chunk => bytes += chunk); req.on('end', () => resolve(JSON.parse(bytes))); });
    assert.deepEqual(body, { permission: 'read' });
    active = true;
    return send(201, { id: linkId, permission: 'read', created_at: 1,
      url: `http://127.0.0.1:${server.address().port}/share/${agent}#token=${token}` });
  }
  if (url.pathname === `${route}/share-links` && req.method === 'GET') return send(200, { data: active ? [{ id: linkId, permission: 'read', created_at: 1 }] : [] });
  if (url.pathname === `${route}/share-links/${linkId}` && req.method === 'DELETE') {
    if (!active) return send(404, { error: 'not_found' });
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
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
const ownerFetch = (path, options = {}) => fetch(origin + path, { ...options, headers: { authorization: `Bearer ${key}`, ...(options.body ? { 'content-type': 'application/json' } : {}) } });
const guestFetch = suffix => fetch(`${origin}/v1/shared/${agent}${suffix}`, { headers: { authorization: `Bearer ${token}` } });
const trace = { command: 'cargo build -p nanocodex2-bin --bin nanocodex2 && node bin/nanocodex/tests/thread-share-tui-e2e.mjs',
  expected: 'owner API key creates, guest reads, TUI /share list and /share revoke, guest reads denied', stages: [] };
let terminal;
try {
  const creation = await ownerFetch(`${route}/share-links`, { method: 'POST', body: JSON.stringify({ permission: 'read' }) });
  assert.equal(creation.status, 201); const created = await creation.json();
  assert.equal(created.url, `${origin}/share/${agent}#token=${token}`);
  trace.stages.push({ action: 'owner create', status: creation.status, linkId });
  const meta = await guestFetch(''); assert.equal(meta.status, 200);
  assert.equal((await meta.json()).permission, 'read');
  const history = await guestFetch('/events/history'); assert.equal(history.status, 200);
  assert.equal((await history.json()).data.at(-1).final_message, 'Synthetic answer');
  trace.stages.push({ action: 'guest metadata/history', statuses: [meta.status, history.status] });
  terminal = spawn('python3', [new URL('./share-pty-bridge.py', import.meta.url).pathname, resolve('target/debug/nanocodex2'), 'attach', agent], {
    cwd: workspace, env: { ...process.env, HOME: workspace, NC_API_KEY: '', CODEX_HOME: resolve(workspace, '.codex'), NANOCODEX_RELOAD_DIR: resolve(workspace, '.reload'),
      NANOCODEX_DISABLE_HAND: '1', NANOCODEX_COMPUTER: 'off', NANOCODEX_MANAGED_URL: origin,
      NANOCODEX_API_KEY: key, TERM: 'xterm-256color', SSH_TTY: '/dev/synthetic-pty', TMUX: '', TMUX_PANE: '' },
    stdio: ['pipe', 'pipe', 'pipe'],
  });
  let screen = ''; let stderr = '';
  terminal.stdout.on('data', bytes => { screen += bytes; });
  terminal.stderr.on('data', bytes => { stderr += bytes; });
  const wait = async (predicate, stage) => { const deadline = Date.now() + 15000;
    while (!predicate()) { if (terminal.exitCode !== null || Date.now() >= deadline) throw new Error(`${stage}: PTY exit ${terminal.exitCode}; ${stderr}; output tail: ${screen.slice(-1800)}`); await new Promise(resolve => setTimeout(resolve, 25)); }
  };
  await wait(() => requests.some(r => r.method === 'WS' && r.path === `${route}/ws`)
    && requests.filter(r => r.method === 'GET' && r.path === route).length >= 2, 'attached');
  await new Promise(resolve => setTimeout(resolve, 100));
  const enter = command => terminal.stdin.write(`\x1b[200~${command}\x1b[201~\r`);
  enter('/share list');
  await wait(() => screen.includes(linkId) && screen.includes('Share · managed thread'), 'TUI list');
  assert.equal(screen.includes(token), false, 'listing must not expose bearer token');
  terminal.stdin.write('\x1b');
  await new Promise(resolve => setTimeout(resolve, 100));
  enter(`/share revoke ${linkId}`);
  await wait(() => requests.some(r => r.method === 'DELETE' && r.path === `${route}/share-links/${linkId}`), 'TUI DELETE');
  await wait(() => screen.includes('revoked'), 'TUI receipt');
  trace.stages.push({ action: 'TUI list/revoke', status: 204, linkId, ptyReceipt: true });
  assert.equal((await guestFetch('')).status, 404);
  assert.equal((await guestFetch('/events/history')).status, 404);
  assert.deepEqual((await (await ownerFetch(`${route}/share-links`)).json()).data, []);
  assert.equal(requests.some(r => /\/turns(?:\/|$)/.test(r.path)), false, 'slash commands must not become turns');
  assert.ok(requests.filter(r => r.path.includes('/share-links')).every(r => r.auth === `Bearer ${key}`));
  assert.ok(requests.filter(r => r.path.includes('/v1/shared/')).every(r => r.auth === `Bearer ${token}`));
  trace.stages.push({ action: 'guest denied, owner list empty', statuses: [404, 404, 200] });
  console.log('Native PTY share revoke journey passed.');
} finally {
  // Preserve a reproducible redacted artifact; the synthetic bearer need not appear in output.
  trace.requests = requests.map(({ auth, ...request }) => ({ ...request, auth: auth === `Bearer ${key}` ? 'owner' : auth === `Bearer ${token}` ? 'guest' : 'none' }));
  writeFileSync(resolve(outputDir, 'trace.json'), JSON.stringify(trace, null, 2) + '\n');
  if (terminal) { terminal.stdin.end(); await Promise.race([new Promise(resolve => terminal.once('close', resolve)), new Promise(resolve => setTimeout(resolve, 1500))]); if (terminal.exitCode === null) terminal.kill(); }
  for (const client of wss.clients) client.terminate();
  server.closeAllConnections(); server.close(); rmSync(workspace, { recursive: true, force: true });
}
