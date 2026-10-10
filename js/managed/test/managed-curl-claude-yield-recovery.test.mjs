import assert from 'node:assert/strict';
import { test } from 'node:test';
import { execFileSync, fork, spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { createServer } from 'node:http';
import { appendFile, mkdir, readFile, writeFile } from 'node:fs/promises';
import { builtinModules } from 'node:module';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { build } from 'esbuild';
import { accountProxyWorker } from '../benchmark/account-proxy.mjs';
import { claudeProvider } from '../../egress/test/claude-provider.fixture.mjs';

// Public HTTP recovery journey: every API call is the curl executable against
// the normal account ingress, Managed API and Egress workers on persisted
// workerd SQLite/R2. The model provider and one synthetic effect endpoint are
// the only stubs. Owner loss is SIGKILL of the whole workerd process group at
// an exact provider boundary, never dispose() or private SQL/IPC seeding.
const root = fileURLToPath(new URL('../../..', import.meta.url));
const owner = '11111111-1111-4111-8111-111111111155';
const settings = { model: 'gpt-6.1-sol', thinking: 'low', reasoning_mode: 'standard', fast_mode: false };
const claudeSettings = { ...settings, model: 'claude-sonnet-4-6' };
const flags = ['nodejs_compat', 'durable_object_io_tasks_prevent_eviction', 'enable_request_signal'];

// Synthetic identity bootstrap only (as benchmark/curl-ttft.mjs); behavior
// under test is reached exclusively through the public /v1 routes.
const bootstrap = `import managed from './src/index.ts';export * from './src/index.ts';
import {ensureAccount,createApiKey} from './src/account-auth.ts';
import {DurableAgentSession as SessionBase} from './src/index.ts';
// The restart acknowledgement leaves the Session 300ms after its handler
// returns, as a held output gate or busy isolate can delay it in production.
export class DurableAgentSession extends SessionBase{async fetch(request){const response=await super.fetch(request);if(new URL(request.url).pathname==='/restart')await scheduler.wait(300);return response;}}
export default {async fetch(request,env,ctx){const path=new URL(request.url).pathname;
if(path==='/__fixture/identity'){await ensureAccount(env,'${owner}',true);
 const auth=await(await env.NANOCODEX_USERS.getByName('${owner}').fetch('https://user.internal/authorization')).json();
 return Response.json(await createApiKey(env,{kind:'api_key',userId:'${owner}',...auth.grant,subjectId:'fixture:${owner}',credentialId:'fixture'},'synthetic-curl-recovery'));}
if(path==='/__fixture/chatgpt'){const expires_at=(Math.ceil(Date.now()/1000)+3600)*1000;
 const claims={exp:Math.ceil(expires_at/1000),'https://api.openai.com/auth':{chatgpt_account_id:'synthetic-account',chatgpt_account_is_fedramp:false}};
 const jwt=btoa(JSON.stringify({alg:'none'})).replaceAll('=','')+'.'+btoa(JSON.stringify(claims)).replaceAll('=','')+'.fixture';
 return env.NANOCODEX.fetch('https://broker.internal/users/${owner}/credentials/chatgpt',{method:'PUT',headers:{'content-type':'application/json'},
  body:JSON.stringify({access_token:jwt,refresh_token:'synthetic-refresh',account_id:'synthetic-account',expires_at,fedramp:false})});}
return managed.fetch(request,env,ctx);}};`;

// Production WebSocket transport. Socket-local history follows
// previous_response_id; CONTROL (the parent test) owns every decision.
const providerSource = `export default {async fetch(request, env) {
  const url = new URL(request.url);
  if (url.origin !== 'https://chatgpt.com' || request.headers.get('upgrade') !== 'websocket') return env.CONTROL.fetch(request);
  if (request.headers.get('chatgpt-account-id') !== 'synthetic-account') throw new Error('fixture ChatGPT account mismatch');
  const [client, server] = Object.values(new WebSocketPair()); server.accept();
  let history = [];
  server.addEventListener('close', () => server.close(1000));
  server.addEventListener('message', async event => {
    const body = JSON.parse(event.data);
    if (body.type !== 'response.create') throw new Error('Unexpected provider frame ' + body.type);
    history = body.previous_response_id ? [...history, ...(body.input ?? [])] : [...(body.input ?? [])];
    const response = await env.CONTROL.fetch('https://control.internal/model', { method: 'POST',
      body: JSON.stringify({ model: body.model, previous_response_id: body.previous_response_id ?? null, history }) });
    for (const frame of await response.json()) server.send(JSON.stringify(frame));
  });
  return new Response(null, { status: 101, webSocket: client });
}};`;

async function bundle(output, name, source, cwd) {
  const wasm = new Set();
  const result = await build({ stdin: { contents: source, resolveDir: cwd }, bundle: true, write: false, format: 'esm', platform: 'browser', target: 'es2022',
    external: ['cloudflare:*', 'node:*'], alias: { 'node-rsa': join(root, 'js/nanocodex/tools/browser/unsupportedNodeRsa.mjs') }, logLevel: 'silent',
    plugins: [{ name: 'compiled-wasm', setup(b) {
      b.onResolve({ filter: /\.\/just-bash-lazy\.mjs$/ }, a => ({ path: resolve(a.resolveDir, a.path), external: true }));
      b.onResolve({ filter: /^[a-z][a-z_]*(?:\/[a-z_]+)?$/ }, a => builtinModules.includes(a.path) ? { path: 'node:' + a.path, external: true } : undefined);
      b.onResolve({ filter: /\.wasm(?:\?module)?$|^nanocodex\/wasm$/ }, a => {
        const path = a.path === 'nanocodex/wasm' ? join(root, 'js/nanocodex/pkg-web/nanocodex_bg.wasm') : resolve(a.resolveDir, a.path.replace(/\?module$/, ''));
        wasm.add(path); return { path, external: true };
      });
    } }] });
  const code = result.outputFiles[0].text;
  const requires = [...new Set([...code.matchAll(/__require\("(node:[^"]+)"\)/g)].map(m => m[1]))];
  const prelude = requires.map((n, i) => `import * as builtin${i} from ${JSON.stringify(n)};`).join('\n')
    + `\nconst requireMap={${requires.map((n, i) => `${JSON.stringify(n)}:builtin${i}`).join(',')}};const require=name=>{if(!requireMap[name])throw new Error('Unexpected require '+name);return requireMap[name];};\n`;
  const path = join(output, name + '.mjs'); await writeFile(path, prelude + code);
  const lazy = [...code.matchAll(/import\("([^"]*just-bash-lazy\.mjs)"\)/g)].map(m => m[1]);
  return [{ type: 'ESModule', path }, ...[...new Set(lazy)].map(path => ({ type: 'ESModule', path })), ...[...wasm].map(path => ({ type: 'CompiledWasm', path }))];
}

// Claude Code Mode across owner loss (production session 6ff61c2b): a cell
// yields, then the owner dies either while the model's next request is in
// flight or while its wait observes the cell. A recovered wait must reconcile
// durable evidence and a freshly generated exec must run; neither may repeat
// an effect the lost owner already dispatched.
test('curl: Claude yielded Code Mode cells reconcile across workerd SIGKILL without duplicate effects', { timeout: 180_000 }, async () => {
  const output = join(root, 'output/managed-curl-recovery', 'claude-yield-' + new Date().toISOString().replaceAll(':', '-') + '-' + randomUUID().slice(0, 8));
  await mkdir(join(output, 'curl'), { recursive: true });
  const git = args => { try { return execFileSync('git', ['-C', root, ...args], { encoding: 'utf8' }).trim(); } catch { return 'unavailable'; } };
  await writeFile(join(output, 'run.json'), JSON.stringify({ command: [process.execPath, ...process.argv.slice(1)], cwd: process.cwd(), git_head: git(['rev-parse', 'HEAD']),
    git_status: git(['status', '--short']), node: process.version, curl: execFileSync('curl', ['--version'], { encoding: 'utf8' }).split('\n')[0], started_at: new Date().toISOString() }, null, 2));
  const workers = [await accountProxyWorker(root),
    { name: 'managed', modulesRoot: '/', modules: await bundle(output, 'managed', bootstrap, join(root, 'js/managed')), compatibilityDate: '2026-07-29', compatibilityFlags: flags,
      bindings: { MANAGED_AGENT_DIRECT_CREDENTIALS: 'true' }, serviceBindings: { NANOCODEX: 'egress', NANOCODEX_SESSION_MODEL_EGRESS: { name: 'egress', entrypoint: 'SessionModelEgress' } },
      durableObjects: Object.fromEntries([['NANOCODEX_AUTH', 'NonceStorage'], ['NANOCODEX_USERS', 'UserAccount'], ['NANOCODEX_ORGANIZATIONS', 'Organization'], ['NANOCODEX_API_KEYS', 'ApiKeyRecord'],
        ['NANOCODEX_SESSIONS', 'DurableAgentSession'], ['NANOCODEX_ACCOUNT_TOOLS', 'AccountHostedTools'], ['NANOCODEX_VM_HOST_POOLS', 'VmHostPool'], ['NANOCODEX_MEMORY', 'MemoryScope']]
        .map(([binding, className]) => [binding, { className, useSQLite: true }])),
      r2Buckets: ['NANOCODEX_HISTORY', 'NANOCODEX_WORKSPACES'], outboundService: 'provider' },
    { name: 'egress', modulesRoot: '/', modules: await bundle(output, 'egress', `export * from './src/egress.ts';export {default} from './src/egress.ts';`, join(root, 'js/egress')),
      compatibilityDate: '2026-07-29', compatibilityFlags: ['nodejs_compat', 'enable_request_signal'],
      bindings: { ENVIRONMENT: 'test', CREDENTIAL_ENCRYPTION_KEY: 'MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY' }, serviceBindings: { MANAGED_AGENT_OWNERSHIP: { name: 'managed', entrypoint: 'ManagedAgentOwnership' } },
      durableObjects: Object.fromEntries([['USER_CREDENTIALS', 'UserCredentialBroker'], ['AGENT_SUBJECTS', 'AgentSubjectDirectory'], ['USER_CONNECTORS', 'UserConnectorBroker'],
        ['MCP_CONNECTIONS', 'McpConnectionDirectory'], ['SPOTIFY_RATE_LIMITS', 'SpotifyRateLimit'], ['GMAIL_PUSH_MAILBOXES', 'GmailPushMailbox']].map(([binding, className]) => [binding, { className, useSQLite: true }])),
      outboundService: 'provider' },
    { name: 'provider', modules: true, script: providerSource, compatibilityDate: '2026-07-29' }];
  await writeFile(join(output, 'workers.json'), JSON.stringify(workers));

  // ---- External stub boundary: Anthropic Messages and one effect host ----
  const claudeCalls = [], effects = [], unexpected = [], kills = [], discovery = [];
  let processNumber = 0, fixture, base, token, dead = Promise.resolve();
  const claudeSse = blocks => [
    { type: 'message_start', message: { id: 'msg_' + randomUUID(), type: 'message', role: 'assistant', model: claudeSettings.model, content: [], usage: { input_tokens: 10, output_tokens: 0 } } },
    ...blocks.flatMap((block, index) => block.type === 'tool_use'
      ? [{ type: 'content_block_start', index, content_block: { ...block, input: {} } }, { type: 'content_block_delta', index, delta: { type: 'input_json_delta', partial_json: JSON.stringify(block.input) } }, { type: 'content_block_stop', index }]
      : [{ type: 'content_block_start', index, content_block: { type: 'text', text: '' } }, { type: 'content_block_delta', index, delta: { type: 'text_delta', text: block.text } }, { type: 'content_block_stop', index }]),
    { type: 'message_delta', delta: { stop_reason: blocks.some(block => block.type === 'tool_use') ? 'tool_use' : 'end_turn', stop_sequence: null }, usage: { output_tokens: 1 } },
    { type: 'message_stop' },
  ].map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join('');
  const tool = (id, name, input) => claudeSse([{ type: 'tool_use', id, name, input }]);
  const say = text => claudeSse([{ type: 'text', text }]);
  const post = name => `await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/${name}"})`;
  // Like a live model, the stub acts only on what tool results show it.
  const decide = body => {
    const user = JSON.stringify(body.messages.filter(message => message.role === 'user').map(message => typeof message.content === 'string' ? message.content
      : message.content.filter(block => block.type === 'text')));
    const scenario = ['CURL_CLAUDE_WAIT_LOSS', 'CURL_CLAUDE_MODEL_LOSS'].find(marker => user.includes(marker));
    const results = body.messages.flatMap(message => Array.isArray(message.content) ? message.content : []).filter(block => block.type === 'tool_result');
    const shown = results.map(result => JSON.stringify(result.content ?? null));
    const cellId = [...shown.join('\n').matchAll(/Script running with cell ID ([0-9a-f-]+:[0-9]+)/g)].at(-1)?.[1];
    const call = { process: processNumber, scenario: scenario ?? null, tool_results: results.length, tool_use_ids: results.map(result => result.tool_use_id),
      shown: Object.fromEntries(results.map((result, index) => [result.tool_use_id, shown[index].slice(0, 4000)])), at: new Date().toISOString() };
    claudeCalls.push(call);
    const prior = claudeCalls.filter(entry => entry.scenario === scenario && entry.tool_results === results.length).length;
    if (scenario === 'CURL_CLAUDE_WAIT_LOSS') {
      // YA completes; YB is dispatched and never answered before owner loss.
      if (results.length === 0) return tool('toolu_wait_loss_exec', 'exec', { code: '// @exec: {"yield_time_ms": 1000}\n'
        + 'text("YA:", ' + post('YA') + ');\ntext("YB:", ' + post('YB') + ');' });
      if (results.length === 1 && cellId) {
        // The owner dies while this wait observes the still-running cell. The
        // same settled response also admits a new exec: after recovery that
        // exec may already have run, so it must stay refused.
        setTimeout(() => void kill('wait in flight on yielded cell'), 1500);
        return claudeSse([{ type: 'tool_use', id: 'toolu_wait_loss_wait', name: 'wait', input: { cell_id: cellId, yield_time_ms: 8000 } },
          { type: 'tool_use', id: 'toolu_wait_loss_sibling', name: 'exec', input: { code: 'text("YF:", ' + post('YF') + ');' } }]);
      }
      if (results.length === 3) return say(/CODE_CELL_RECOVERED_EVIDENCE/.test(shown[1]) ? 'WAIT_LOSS_RECONCILED' : 'WAIT_LOSS_UNRECONCILED');
    }
    if (scenario === 'CURL_CLAUDE_MODEL_LOSS') {
      if (results.length === 0) return tool('toolu_model_loss_exec', 'exec', { code: '// @exec: {"yield_time_ms": 1000}\n'
        + 'text("YC:", ' + post('YC') + ');\ntext("YD:", ' + post('YD') + ');' });
      // The first request after the yield dies in flight with its owner.
      if (results.length === 1 && prior === 1) return 'kill';
      // Its fresh replacement starts a new cell that has never run anywhere.
      if (results.length === 1) return tool('toolu_model_loss_fresh', 'exec', { code: 'text("YE:", ' + post('YE') + ');' });
      if (results.length === 2) return say(/EFFECT_YE_APPLIED/.test(shown[1]) ? 'MODEL_LOSS_FRESH_RAN' : 'MODEL_LOSS_FRESH_DENIED');
    }
    unexpected.push({ kind: 'claude-decision', scenario: scenario ?? null, tool_results: results.length, cell: cellId ?? null });
    return say('UNEXPECTED');
  };
  const sockets = new Set();
  const control = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const call = JSON.parse(raw), url = new URL(call.url);
    const send = (status, body, type = 'application/json') => { res.writeHead(status, { 'content-type': type }); res.end(typeof body === 'string' ? body : JSON.stringify(body)); };
    if (url.origin === 'https://api.anthropic.com' && url.pathname === '/v1/models') return send(200, { data: [{ id: claudeSettings.model, display_name: 'Synthetic Claude Sonnet' }], has_more: false });
    if (url.origin === 'https://api.anthropic.com' && url.pathname === '/v1/messages') {
      const body = JSON.parse(call.body);
      if (body.stream === true && /CURL_CLAUDE_(WAIT|MODEL)_LOSS/.test(JSON.stringify(body.messages))) {
        const decision = decide(body);
        if (decision === 'kill') return void kill('Claude model request in flight: ' + claudeCalls.at(-1).scenario);
        return send(200, decision, 'text/event-stream');
      }
      unexpected.push({ kind: 'claude', model: body.model, stream: body.stream ?? null }); return send(400, { type: 'error', error: { type: 'invalid_request_error', message: 'unexpected synthetic Claude request' } });
    }
    if (/^https:\/\/(platform\.claude\.com|claude\.ai|api\.anthropic\.com)\//.test(call.url)) {
      const response = await claudeProvider(new Request(call.url, { method: call.method, headers: call.headers, body: call.body ?? undefined }));
      if (response) { res.writeHead(response.status, Object.fromEntries(response.headers)); return void res.end(Buffer.from(await response.arrayBuffer())); }
    }
    if (url.hostname === 'effects.example') {
      const name = url.pathname.split('/').pop();
      effects.push({ process: processNumber, name, method: call.method, at: new Date().toISOString() });
      // YB and YD reach the external system and never answer: their outcome
      // is genuinely unknown when the owner dies.
      if (name === 'YB' || name === 'YD') return;
      return send(200, 'EFFECT_' + name + '_APPLIED\n', 'text/plain');
    }
    if (url.origin === 'https://chatgpt.com' && call.body?.includes('"gpt-6-luna"')) {
      return send(200, { id: 'title', output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'Synthetic recovery' }] }], usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } });
    }
    if (call.method === 'POST' && /\/mcp\/?$|^https:\/\/mcp\./.test(call.url)) { discovery.push(call.url); return send(503, { error: 'synthetic offline' }); }
    unexpected.push({ kind: 'http', method: call.method, url: call.url });
    send(404, { error: 'unexpected synthetic external request' });
  });
  control.on('connection', socket => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); });
  await new Promise(resolvePromise => control.listen(0, '127.0.0.1', resolvePromise));
  const controlBase = `http://127.0.0.1:${control.address().port}`;

  const start = () => dead.then(() => new Promise((resolvePromise, reject) => {
    const number = ++processNumber;
    const child = fork(fileURLToPath(new URL('./fixtures/managed-curl-recovery-process.mjs', import.meta.url)), [output, controlBase], { detached: true, stdio: ['ignore', 'pipe', 'pipe', 'ipc'] });
    for (const stream of ['stdout', 'stderr']) child[stream].on('data', chunk => void appendFile(join(output, `workerd-${number}.log`), chunk));
    child.once('error', reject); child.once('exit', code => reject(new Error('fixture exited before ready: ' + code)));
    child.once('message', message => { child.removeAllListeners('exit'); fixture = { child, number, exited: new Promise(r => child.once('exit', r)) }; base = message.ready; resolvePromise(); });
  }));
  async function kill(reason) {
    const current = fixture; if (!current) return; fixture = undefined;
    process.kill(-current.child.pid, 'SIGKILL'); dead = current.exited;
    kills.push({ process: current.number, reason, at: new Date().toISOString() }); await dead;
    // Requests the dead owner left open never receive a response.
    for (const socket of sockets) socket.destroy();
  }
  const waitFor = async (label, predicate, timeout = 30_000, interval = 50) => {
    const deadline = Date.now() + timeout;
    for (;;) { const value = await predicate(); if (value) return value; if (Date.now() > deadline) throw new Error('timed out waiting for ' + label); await delay(interval); }
  };

  // ---- curl: bearer travels on stdin config, never argv or saved evidence ----
  let sequence = 0;
  async function curl(label, path, { method = 'GET', body, headers = {}, expected, maxTime = 30 } = {}) {
    const name = `${String(++sequence).padStart(3, '0')}-${label}`, file = join(output, 'curl', name);
    if (body !== undefined) await writeFile(file + '.request.json', JSON.stringify(body, null, 2));
    const args = ['--silent', '--show-error', '--max-time', String(maxTime), '--request', method, '--dump-header', file + '.headers', '--config', '-',
      '-H', 'Content-Type: application/json', ...Object.entries(headers).flatMap(([key, value]) => ['-H', `${key}: ${value}`]),
      ...(body === undefined ? [] : ['--data-binary', '@' + file + '.request.json']), new URL(path, base).href];
    const started = Date.now(), child = spawn('curl', args, { stdio: ['pipe', 'pipe', 'pipe'] });
    child.stdin.end(`header = ${JSON.stringify('Authorization: Bearer ' + token)}\n`);
    let stdout = '', stderr = '';
    child.stdout.on('data', chunk => { stdout += chunk; });
    child.stderr.on('data', chunk => { stderr += chunk; });
    const exit = await new Promise(r => child.on('close', (code, signal) => r({ code, signal })));
    const head = await readFile(file + '.headers', 'utf8').catch(() => '');
    const status = Number([...head.matchAll(/^HTTP\/\S+ (\d+)/gm)].at(-1)?.[1] ?? 0);
    await writeFile(file + '.response', stdout);
    let value; try { value = JSON.parse(stdout); } catch { value = stdout; }
    await appendFile(join(output, 'transcript.jsonl'), JSON.stringify({ name, process: fixture?.number, command: ['curl', ...args], stdin: 'header = "Authorization: Bearer <redacted synthetic API key>"',
      status, exit, stderr, duration_ms: Date.now() - started, expected: expected ?? null, request: body ?? null, response: value }) + '\n');
    if (expected !== undefined) assert.equal(status, expected, `${name}: ${stderr} ${stdout.slice(0, 2000)}`);
    return { status, value };
  }
  const turnState = async (label, agent, turn) => (await curl(label, `/v1/agents/${agent}/turns/${turn}`, { expected: 200 })).value;
  const terminal = async (label, agent, turn) => waitFor(label + ' terminal', async () => {
    const value = await turnState(label, agent, turn);
    return ['completed', 'failed', 'cancelled'].includes(value.state) ? value : undefined;
  }, 45_000, 200);
  const history = async (label, agent) => JSON.stringify((await curl(label, `/v1/agents/${agent}/events/history?after=0&limit=256`, { expected: 200 })).value);
  const count = name => effects.filter(effect => effect.name === name).length;
  const summary = {};

  try {
    await start();
    const identity = await fetch(new URL('/__fixture/identity', base), { method: 'POST' });
    token = (await identity.json()).token; assert.ok(token, 'synthetic API key');
    const login = await curl('claude-login', '/v1/credentials/claude/login', { method: 'POST' });
    assert.ok(login.status >= 200 && login.status < 300 && login.value.authorization_url, 'Claude login starts: ' + login.status);
    const loginState = new URL(login.value.authorization_url).searchParams.get('state');
    const connected = await curl('claude-login-complete', '/v1/credentials/claude/login/complete', { method: 'POST', body: { code: 'curl-claude-yield#' + loginState } });
    assert.ok(connected.status >= 200 && connected.status < 300, 'Claude login completes: ' + connected.status + ' ' + JSON.stringify(connected.value));

    // A. The response holding wait was settled before the loss, so recovery
    // replays it and the wait itself is unreceipted. wait never evaluates
    // source: it reconciles the lost cell from durable evidence.
    const waitRun = (await curl('wait-loss-admit', '/v1/agent-runs', { method: 'POST', body: { input: 'CURL_CLAUDE_WAIT_LOSS: run the yielded synthetic effects once.', settings: claudeSettings }, headers: { 'Idempotency-Key': randomUUID() }, expected: 201 })).value;
    await waitFor('wait-loss owner loss', () => kills.length === 1 && !fixture);
    assert.deepEqual([count('YA'), count('YB')], [1, 1], 'YA completed and YB was dispatched before the loss');
    const siblingBeforeLoss = count('YF');
    assert.ok(siblingBeforeLoss <= 1, 'the sibling exec ran at most once before the loss');
    await start();
    await turnState('wait-loss-after-restart', waitRun.agent_id, waitRun.turn_id);
    const waitDone = await terminal('wait-loss', waitRun.agent_id, waitRun.turn_id);
    const waitHistory = await history('wait-loss-history', waitRun.agent_id);
    const waitCalls = claudeCalls.filter(call => call.scenario === 'CURL_CLAUDE_WAIT_LOSS');
    const recovered = waitCalls.find(call => call.tool_results === 3);
    summary.wait_loss = { terminal: waitDone, calls: waitCalls, effects: { YA: count('YA'), YB: count('YB'), YF: count('YF'), YF_before_loss: siblingBeforeLoss } };
    assert.equal(waitDone.state, 'completed', JSON.stringify(waitDone));
    assert.ok(recovered, 'the model receives the recovered wait result');
    assert.equal(recovered.process, 2, 'the wait result is produced by the restarted owner');
    assert.deepEqual(recovered.tool_use_ids, ['toolu_wait_loss_exec', 'toolu_wait_loss_wait', 'toolu_wait_loss_sibling'], 'the replayed calls keep their original identities');
    const waited = recovered.shown.toolu_wait_loss_wait, sibling = recovered.shown.toolu_wait_loss_sibling;
    assert.doesNotMatch(waited, /admission was lost during recovery/, 'a replayed wait is not refused as a lost admission');
    assert.match(waited, /CODE_CELL_RECOVERED_EVIDENCE/, 'the recovered wait reports durable cell evidence: ' + waited);
    assert.match(waited, /EFFECT_YA_APPLIED/, 'the completed nested receipt is retained as historical evidence');
    assert.match(waited, /pending_effect_count[^0-9]*1\b/, 'the dispatched, unanswered YB stays outcome unknown');
    if (siblingBeforeLoss === 0) assert.match(sibling, /admission was lost during recovery/, 'an unreceipted exec from a replayed response stays refused: ' + sibling);
    else assert.match(sibling, /admission was lost during recovery|EFFECT_YF_APPLIED/, 'the sibling exec reports its receipt or a refusal: ' + sibling);
    assert.match(waitHistory, /WAIT_LOSS_RECONCILED/);
    assert.deepEqual([count('YA'), count('YB'), count('YF')], [1, 1, siblingBeforeLoss], 'recovery never repeats YA, YB or the sibling exec');

    // B. The model request after the yield dies in flight. Its fresh
    // replacement response is a new round: its exec has never run, so it
    // executes once instead of being refused.
    const modelRun = (await curl('model-loss-admit', '/v1/agent-runs', { method: 'POST', body: { input: 'CURL_CLAUDE_MODEL_LOSS: run the yielded synthetic effects once.', settings: claudeSettings }, headers: { 'Idempotency-Key': randomUUID() }, expected: 201 })).value;
    await waitFor('model-loss owner loss', () => kills.length === 2 && !fixture);
    assert.deepEqual([count('YC'), count('YD'), count('YE')], [1, 1, 0]);
    await start();
    await turnState('model-loss-after-restart', modelRun.agent_id, modelRun.turn_id);
    const modelDone = await terminal('model-loss', modelRun.agent_id, modelRun.turn_id);
    const modelHistory = await history('model-loss-history', modelRun.agent_id);
    const modelCalls = claudeCalls.filter(call => call.scenario === 'CURL_CLAUDE_MODEL_LOSS');
    const fresh = modelCalls.find(call => call.tool_results === 2);
    const freshShown = fresh?.shown.toolu_model_loss_fresh ?? '';
    summary.model_loss = { terminal: modelDone, calls: modelCalls, effects: { YC: count('YC'), YD: count('YD'), YE: count('YE') } };
    assert.equal(modelDone.state, 'completed', JSON.stringify(modelDone));
    assert.ok(fresh, 'the model receives the fresh exec result');
    assert.equal(fresh.process, kills.at(-1).process + 1, 'the fresh response is generated by the restarted owner');
    assert.doesNotMatch(freshShown, /admission was lost during recovery/, 'a freshly generated exec is not refused');
    assert.match(freshShown, /EFFECT_YE_APPLIED/, 'the fresh exec ran: ' + freshShown);
    assert.match(modelHistory, /MODEL_LOSS_FRESH_RAN/);
    assert.deepEqual([count('YC'), count('YD'), count('YE')], [1, 1, 1], 'each effect is dispatched exactly once');
    assert.deepEqual(unexpected, []);
  } finally {
    await writeFile(join(output, 'trace.json'), JSON.stringify({ summary, kills, claude_calls: claudeCalls, effects, unexpected, offline_mcp_discovery: discovery.length }, null, 2));
    await kill('test cleanup');
    for (const socket of sockets) socket.destroy();
    await new Promise(r => control.close(r));
    console.log(JSON.stringify({ evidence: output, kills: kills.length, effects: effects.map(effect => effect.name), claude_calls: claudeCalls.length }));
  }
});

