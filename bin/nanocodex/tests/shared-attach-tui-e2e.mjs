// Real guest CLI + PTY journeys; only the remote managed service is synthetic.
// Repro (build separately): node bin/nanocodex/tests/shared-attach-tui-e2e.mjs
// Override the executable with NANOCODEX2_BIN. No account or real share link needed.
// Worker authorization/storage is covered separately by thread-share-links.test.ts.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdirSync, mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const require = createRequire(new URL('../../../js/managed/package.json', import.meta.url));
const { Terminal } = require('@xterm/headless');

const binary = resolve(process.env.NANOCODEX2_BIN || 'target/debug/nanocodex2');
const bridge = fileURLToPath(new URL('./share-pty-bridge.py', import.meta.url));
const output = resolve('output/shared-attach-tui');
mkdirSync(output, { recursive: true });
const run = mkdtempSync(resolve(output, 'run-'));
const agent = '019fc927-b280-79a7-8445-1b9996ad2fb0';
const token = `nsl_${'g'.repeat(43)}`;
const base = `/v1/shared/${agent}`;
const redact = value => String(value).replaceAll(token, '[synthetic-share-token]')
  .replace(/nsl_[A-Za-z0-9_-]+/g, '[redacted-share-token]');
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const trace = { command: 'node bin/nanocodex/tests/shared-attach-tui-e2e.mjs', binary,
  boundary: 'Real CLI/PTy; synthetic HTTP service; isolated empty HOME, no account key',
  expected: 'history and live SSE visible; read blocks writes; write submits once with same origin; history activity blocks writes until completion; uncertain delivery retries only explicitly with the same identity; terminal 404 stops; malformed links redact tokens',
  stages: [], requests: [] };
let current;
const event = (cursor, type, id, fields) => ({ cursor: String(cursor), created_at: 1, turn_id: id, type, id, ...fields });
const newest = [event(5, 'turn_accepted', 'history-new', { input: 'Recent synthetic question' }),
  event(6, 'turn_completed', 'history-new', { final_message: 'RECENT_HISTORY_VISIBLE' })];
const oldest = [event(1, 'turn_accepted', 'history-old', { input: 'Older synthetic question' }),
  event(2, 'turn_completed', 'history-old', { final_message: 'OLDER_HISTORY_VISIBLE' })];
const live = [event(7, 'turn_accepted', 'live-turn', { input: 'Live synthetic question' }),
  event(8, 'turn_completed', 'live-turn', { final_message: 'LIVE_SSE_VISIBLE' })];
function emit(res, item) {
  res.write(`id: ${item.cursor}\nevent: ${item.type}\ndata: ${JSON.stringify(item)}\n\n`);
}
const server = createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  const entry = { scenario: current?.name, method: req.method, path: url.pathname, query: url.search,
    auth: req.headers.authorization === `Bearer ${token}` ? 'guest' : req.headers.authorization ? 'unexpected' : 'none',
    origin: req.headers.origin ?? null, contentType: req.headers['content-type'] ?? null, lastEventId: req.headers['last-event-id'] ?? null };
  trace.requests.push(entry);
  const send = (status, body) => { entry.status = status; res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(body)); };
  try {
    if (!current || !url.pathname.startsWith(base) || entry.auth !== 'guest') {
      entry.forbidden = true; return send(403, { error: 'unexpected_route_or_auth' });
    }
    if (current.revoked || current.name === 'missing') return send(404, { error: 'not_found' });
    if (req.method === 'GET' && url.pathname === base)
      return send(200, { agent_id: agent, permission: current.permission, title: 'Synthetic shared conversation', latest_event_cursor: '6' });
    if (req.method === 'GET' && url.pathname === `${base}/events/history`) {
      const before = url.searchParams.get('before');
      const latestCursor = current.name === 'active-history' ? '7' : '6';
      // The empty middle page models owner-only events removed by projection.
      if (before === null) {
        // This acceptance occurs after metadata (cursor 6), before history.
        const active = current.name === 'active-history';
        return send(200, { data: active ? [...newest, event(7, 'turn_accepted', 'already-running', { input: 'Initial active synthetic turn' })] : newest,
          has_more: true, next_cursor: '5', latest_cursor: latestCursor });
      }
      if (before === '5') return send(200, { data: [], has_more: true, next_cursor: '3', latest_cursor: latestCursor });
      if (before === '3') return send(200, { data: oldest, has_more: false, next_cursor: null, latest_cursor: latestCursor });
      entry.forbidden = true; return send(400, { error: 'unexpected_history_cursor' });
    }
    if (req.method === 'GET' && url.pathname === `${base}/events`) {
      entry.status = 200;
      entry.after = url.searchParams.get('after');
      res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache', connection: 'keep-alive' });
      res.write(': connected\n\n');
      if (current.name === 'active-history') {
        // Replay overlaps the initial history: it must not add a second active turn.
        emit(res, event(7, 'turn_accepted', 'already-running', { input: 'Initial active synthetic turn' }));
      }
      current.streams.add(res);
      res.on('close', () => current?.streams.delete(res));
      return;
    }
    if (req.method === 'POST' && url.pathname === `${base}/turns`) {
      let raw = ''; for await (const chunk of req) raw += chunk;
      entry.body = JSON.parse(raw);
      entry.rawBody = raw;
      current.posts.push(entry);
      if (current.permission !== 'write' || req.headers.origin !== origin) return send(403, { error: 'forbidden' });
      const replayed = current.admitted.has(entry.body.id);
      if (!replayed) current.admitted.set(entry.body.id, entry.rawBody);
      entry.admission = replayed ? 'replayed' : 'new';
      if (current.name === 'uncertain-submit' && current.posts.length === 1) {
        // Admission has happened, but neither its HTTP receipt nor SSE evidence
        // reaches the guest. The journey controls when the response socket dies.
        entry.response = 'pending-disconnect';
        current.disconnectAdmission = () => {
          entry.response = 'socket-closed-without-receipt';
          req.socket.destroy();
        };
        return;
      }
      send(202, { turn_id: entry.body.id, state: 'accepted', accepted_cursor: '9', replayed });
      for (const stream of current.streams) {
        emit(stream, event(9, 'turn_accepted', entry.body.id, { input: entry.body.input }));
        emit(stream, event(10, 'turn_completed', entry.body.id, { final_message: 'WRITE_RESPONSE_VISIBLE' }));
      }
      return;
    }
    entry.forbidden = true; send(403, { error: 'unexpected_route' });
  } catch (error) {
    entry.fixtureError = redact(error.stack);
    if (!res.headersSent) send(500, { error: 'fixture_error' }); else res.end();
  }
});
server.on('upgrade', (req, socket) => {
  trace.requests.push({ scenario: current?.name, method: 'WS', path: redact(req.url), forbidden: true });
  socket.destroy();
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
const url = `${origin}/share/${agent}#token=${token}`;

function launch(name, shareUrl = url, permission = 'read') {
  const home = resolve(run, name); mkdirSync(home, { recursive: true });
  // Use a small allowlist rather than inherit credentials, connector settings or proxies.
  const env = { PATH: process.env.PATH, LANG: 'en_US.UTF-8', HOME: home,
    TMPDIR: home, CODEX_HOME: resolve(home, '.codex'), XDG_CONFIG_HOME: resolve(home, '.config'),
    NANOCODEX_RELOAD_DIR: resolve(home, '.reload'), TERM: 'xterm-256color', SSH_TTY: '/dev/synthetic-pty',
    NANOCODEX_API_KEY: '', NC_API_KEY: '', OPENAI_API_KEY: '', TMUX: '', TMUX_PANE: '',
    NANOCODEX_MANAGED_URL: origin };
  // Do not disable Hand with an env flag: the guest path itself must prevent it.
  const child = spawn('python3', [bridge, binary, 'attach', shareUrl], { cwd: home, env, stdio: ['pipe', 'pipe', 'pipe'] });
  const terminal = new Terminal({ cols: 160, rows: 32, allowProposedApi: true });
  const state = { terminal, rendered: '', name, permission, child, screen: '', stderr: '', closed: false, streams: new Set(), posts: [], admitted: new Map(), disconnectAdmission: null, revoked: false };
  current = state;
  child.stdout.on('data', bytes => {
    state.screen += bytes;
    terminal.write(bytes, () => {
      const buffer = terminal.buffer.active;
      state.rendered = Array.from({ length: buffer.length }, (_, row) => buffer.getLine(row)?.translateToString(true) ?? '').join('\n');
    });
  });
  child.stderr.on('data', bytes => { state.stderr += bytes; });
  child.on('error', error => { state.stderr += String(error); state.closed = true; });
  child.on('close', () => { state.closed = true; });
  child.stdin.on('error', () => {});
  return state;
}
const plain = state => state.rendered + state.stderr;
async function wait(state, predicate, label, timeout = 15000) {
  const deadline = Date.now() + timeout;
  while (!predicate()) {
    if (state.closed || Date.now() > deadline) throw new Error(redact(`${state.name}: ${label}; closed=${state.closed}; tail=${plain(state).slice(-2000)}`));
    await delay(25);
  }
}
const enter = (state, text) => state.child.stdin.write(`\x1b[200~${text}\x1b[201~\r`);
function boundaries(state) {
  const requests = trace.requests.filter(r => r.scenario === state.name);
  assert.ok(requests.every(r => !r.forbidden && !r.fixtureError), `${state.name}: only documented guest routes allowed`);
  assert.ok(requests.every(r => r.auth === 'guest'), `${state.name}: only share bearer allowed`);
  assert.ok(!state.screen.includes(token) && !state.stderr.includes(token), `${state.name}: bearer leaked to terminal`);
}
async function stop(state) {
  if (!state) return;
  writeFileSync(resolve(run, `${state.name}.terminal.txt`), redact(plain(state)));
  state.child.stdin.end();
  for (let i = 0; i < 40 && !state.closed; i++) await delay(25);
  if (!state.closed) { state.child.kill('SIGTERM'); await delay(100); }
  for (const stream of state.streams) stream.destroy();
  // Keep only redacted evidence, not generated user configuration.
  rmSync(resolve(run, state.name), { recursive: true, force: true });
  state.terminal.dispose();
}
async function journey(name, permission, action) {
  const state = launch(name, url, permission);
  try {
    await wait(state, () => plain(state).includes('OLDER_HISTORY_VISIBLE') && plain(state).includes('RECENT_HISTORY_VISIBLE'), 'paginated history visible');
    await wait(state, () => state.streams.size > 0, 'SSE connection');
    await action(state);
    boundaries(state);
    trace.stages.push({ name, outcome: 'passed' });
  } finally { await stop(state); }
}
try {
  await journey('read', 'read', async state => {
    for (const stream of state.streams) for (const item of live) emit(stream, item);
    await wait(state, () => plain(state).includes('LIVE_SSE_VISIBLE'), 'live answer visible');
    enter(state, 'Read-only input must stay local');
    await wait(state, () => /read.only|view.only|cannot.*send|cannot.*submit/i.test(plain(state)), 'read-only indication');
    await delay(300);
    assert.equal(state.posts.length, 0, 'read-only input must not POST');
    // Force a real reconnect and deliver a replay with a poison marker. Applying
    // this duplicate cursor would visibly corrupt the earlier answer.
    for (const stream of state.streams) stream.end();
    await wait(state, () => trace.requests.filter(r => r.scenario === 'read' && r.path === `${base}/events`).length >= 2 && state.streams.size > 0, 'SSE reconnect');
    const reconnect = trace.requests.filter(r => r.scenario === 'read' && r.path === `${base}/events`).at(-1);
    assert.equal(reconnect.after, '8', 'resume at last observed cursor');
    for (const stream of state.streams) {
      emit(stream, event(8, 'turn_completed', 'live-turn', { final_message: 'DUPLICATE_CURSOR_MUST_NOT_RENDER' }));
      emit(stream, event(9, 'turn_accepted', 'reconnected-turn', { input: 'Reconnect question' }));
      emit(stream, event(10, 'turn_completed', 'reconnected-turn', { final_message: 'RECONNECTED_ANSWER_VISIBLE' }));
    }
    await wait(state, () => plain(state).includes('RECONNECTED_ANSWER_VISIBLE'), 'reconnected answer');
    assert.ok(!plain(state).includes('DUPLICATE_CURSOR_MUST_NOT_RENDER'), 'duplicate cursor ignored');
    state.revoked = true;
    for (const stream of state.streams) stream.end();
    await wait(state, () => trace.requests.some(r => r.scenario === 'read' && r.status === 404), 'revoked stream rejected');
    await wait(state, () => /revok|not.found|unavailable|404|no longer|expired/i.test(plain(state)), 'revocation visible');
    const count = trace.requests.filter(r => r.scenario === 'read').length;
    await delay(2500);
    assert.equal(trace.requests.filter(r => r.scenario === 'read').length, count, 'terminal 404 must stop retrying');
    assert.equal(state.posts.length, 0);
  });
  await journey('write', 'write', async state => {
    enter(state, 'Synthetic guest write exactly once');
    await wait(state, () => plain(state).includes('WRITE_RESPONSE_VISIBLE'), 'guest response visible');
    await delay(500);
    assert.equal(state.posts.length, 1, 'one user submission must produce exactly one POST');
    assert.equal(state.posts[0].origin, origin);
    assert.equal(state.posts[0].contentType?.split(';')[0], 'application/json');
    assert.equal(state.posts[0].body.input, 'Synthetic guest write exactly once');
    assert.ok(typeof state.posts[0].body.id === 'string' && state.posts[0].body.id.length > 0);
  });
  await journey('active-history', 'write', async state => {
    // Exercise the public terminal guard, not an internal activity counter.
    // A blocked draft must remain editable and become sendable on completion.
    const prompt = 'Send only after initial active turn completes';
    enter(state, prompt);
    await wait(state, () => /wait.*(?:active|turn)|thinking|working|busy/i.test(plain(state)), 'initial history activity visible');
    await delay(500);
    assert.equal(state.posts.length, 0, 'initial active history must block submission');
    const streamRequest = trace.requests.find(r => r.scenario === state.name && r.path === `${base}/events`);
    assert.equal(streamRequest.after, '6', 'live replay starts before acceptance in initial history');
    for (const stream of state.streams)
      emit(stream, event(8, 'turn_completed', 'already-running', { final_message: 'INITIAL_ACTIVE_COMPLETED' }));
    await wait(state, () => plain(state).includes('INITIAL_ACTIVE_COMPLETED'), 'initial active turn completion visible');
    await delay(250);
    assert.equal(state.posts.length, 0, 'blocked Enter must not queue an automatic submission');
    state.child.stdin.write('\r');
    await wait(state, () => plain(state).includes('WRITE_RESPONSE_VISIBLE'), 'write allowed after active turn completes');
    assert.equal(state.posts.length, 1, 'one explicit Enter after completion submits once');
    assert.equal(state.posts[0].body.input, prompt, 'blocked submission preserves the draft');
  });
  await journey('uncertain-submit', 'write', async state => {
    const prompt = 'Retry this uncertain synthetic delivery';
    enter(state, prompt);
    await wait(state, () => state.disconnectAdmission !== null, 'first POST admitted without response');
    // Allow the cleared composer to render before the error restores its draft.
    await delay(250);
    state.disconnectAdmission();
    await wait(state, () => /not.confirmed|delivery|retry/i.test(plain(state))
      && plain(state).includes(prompt), 'unconfirmed delivery and restored draft visible');
    await delay(2500);
    assert.equal(state.posts.length, 1, 'an uncertain admission must not automatically retry');
    assert.equal(state.admitted.size, 1, 'first POST represents one admitted turn');
    // No repaste or history recall: Enter acts directly on the restored draft.
    state.child.stdin.write('\r');
    await wait(state, () => state.posts.length >= 2 && plain(state).includes('WRITE_RESPONSE_VISIBLE'), 'explicit retry receipt and SSE completion');
    await delay(500);
    assert.equal(state.posts.length, 2, 'only initial POST and explicit retry are sent');
    assert.equal(state.posts[1].rawBody, state.posts[0].rawBody, 'retry preserves exact UUID and request body');
    assert.match(state.posts[0].body.id, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i);
    assert.equal(state.posts[1].body.input, prompt);
    assert.equal(state.posts[0].response, 'socket-closed-without-receipt');
    assert.equal(state.posts[1].admission, 'replayed', 'retry resolves the original admission');
    assert.equal(state.posts[1].status, 202);
    assert.equal(state.admitted.size, 1, 'retry must not create a second real turn');
  });
  const missing = launch('missing');
  try {
    await wait(missing, () => /not.found|revok|unavailable|404|expired/i.test(plain(missing)), 'missing link error visible');
    await delay(2500);
    assert.equal(trace.requests.filter(r => r.scenario === 'missing').length, 1, 'missing link must fail without retry');
    boundaries(missing); trace.stages.push({ name: 'missing', outcome: 'passed' });
  } finally { await stop(missing); }
  for (const [name, malformed] of [
    ['bad-path', `${origin}/wrong/${agent}#token=${token}`],
    ['query-token', `${origin}/share/${agent}?token=${token}`],
    ['bad-token', `${origin}/share/${agent}#token=${token}extra`],
  ]) {
    const state = launch(name, malformed);
    try {
      await wait(state, () => /invalid|malformed|expected|unsupported|missing|error/i.test(plain(state)), 'malformed link error visible');
      await delay(150);
      assert.equal(trace.requests.filter(r => r.scenario === name).length, 0, 'invalid link must not contact server');
      boundaries(state); trace.stages.push({ name, outcome: 'passed' });
    } finally { await stop(state); }
  }
  trace.outcome = 'passed';
  console.log(`Guest shared attach PTY journeys passed. Evidence: ${run}`);
} catch (error) {
  trace.outcome = 'failed'; trace.error = redact(error.stack);
  console.error(trace.error); process.exitCode = 1;
} finally {
  server.closeAllConnections(); server.close();
  writeFileSync(resolve(run, 'trace.json'), redact(JSON.stringify(trace, null, 2)) + '\n');
  console.log(`Redacted trace: ${resolve(run, 'trace.json')}`);
}
