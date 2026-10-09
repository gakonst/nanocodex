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
const exhausted = /MANAGED_RECOVERY_EXHAUSTED[\s\S]*outcome unknown/;

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

test('curl: yielded Code Mode nested calls terminate exactly once after completion and cancellation', { timeout: 180_000 }, async () => {
  const output = join(root, 'output/managed-curl-code-relay', new Date().toISOString().replaceAll(':', '-') + '-' + randomUUID().slice(0, 8));
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

  // ---- External stub boundary (the parent owns all provider decisions) ----
  const modelCalls = [], effects = [], unexpected = [], kills = [], discovery = [];
  let processNumber = 0, fixture, base, token, dead = Promise.resolve();
  const respond = (output, end) => { const id = 'resp_' + randomUUID();
    return [{ type: 'response.created', response: { id, status: 'in_progress' } },
      ...output.filter(item => item.type === 'message').map(item => ({ type: 'response.output_text.delta', output_index: 0, delta: item.content[0].text })),
      { type: 'response.completed', response: { id, status: 'completed', ...(end ? { end_turn: true } : {}), output, usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } }]; };
  const say = text => respond([{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] }], true);
  const exec = (callId, source) => respond([{ type: 'custom_tool_call', name: 'exec', call_id: callId, input: source }], false);
  // Most specific first: a follow-up turn's history still contains its predecessor's marker.
  const markers = ['CURL_CLAUDE_CHILD', 'CURL_WIDE_TASK', 'CURL_ROOT_WIDE', 'CURL_FOLLOWUP_TASK', 'CURL_CHILD_FOLLOWUP', 'CURL_CHILD_TASK', 'CURL_LOOP_TASK', 'CURL_BUDGET_NEXT', 'CURL_LOOP_NEXT', 'CURL_BUDGET', 'CURL_EFFECTS', 'CURL_ROOT_SPAWN', 'CURL_ROOT_LOOP'];
  const baselines = {};
  const delegate = (task, marker) => [
    () => exec(marker + '-spawn', 'text(await tools.spawn_agent(' + JSON.stringify({ role: 'Curl child', task, model: 'sol', thinking: 'low', output_contract: { kind: 'string' } }) + '));'),
    () => exec(marker + '-wait', 'text(await tools.wait_agent({agent_ids:[1],timeout_ms:20000}));'),
  ];
  // The model answers without waiting on the yielded cell, as live models do.
  const cancelInference = {}; cancelInference.wait = new Promise(resolve => { cancelInference.started = resolve; });
  const decide = async ({ history }) => {
    // Decide from the current turn only: later turns retain earlier markers.
    const since = history.findLastIndex(item => item.role === 'user' && /CURL_[A-Z_]+:/.test(JSON.stringify(item.content)));
    const user = JSON.stringify(history[since]?.content ?? null);
    const scenario = ['CURL_UNAWAITED_NEXT', 'CURL_CANCEL_NEXT', 'CURL_CANCEL', 'CURL_UNAWAITED', 'CURL_WAIT_CANCEL_WAIT', 'CURL_WAIT_CANCEL_NEXT', 'CURL_WAIT_CANCEL'].find(marker => user.includes(marker + ':'));
    const outputs = history.slice(since + 1).filter(item => /_call_output$/.test(item.type ?? ''));
    modelCalls.push({ process: processNumber, scenario, tool_outputs: outputs.length, last_output: outputs.at(-1), at: new Date().toISOString() });
    if (scenario === 'CURL_UNAWAITED') return outputs.length === 0 ? exec('curl-unawaited-cell',
      '// @exec: {"yield_time_ms": 200}\ntext(await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/SLOW"}));')
      : say('UNAWAITED_DONE');
    if (scenario === 'CURL_UNAWAITED_NEXT') return say('UNAWAITED_NEXT_OK');
    // The cell yields on a hanging effect; the next inference is still in
    // flight when the client cancels the turn.
    if (scenario === 'CURL_CANCEL') { if (outputs.length === 0) return exec('curl-cancel-cell',
      '// @exec: {"yield_time_ms": 200}\ntext(await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/HANG"}));');
      cancelInference.started(); await delay(20_000); return say('CANCEL_NOT_CANCELLED'); }
    if (scenario === 'CURL_CANCEL_NEXT') return say('CANCEL_NEXT_OK');
    // An earlier turn leaves its cell running; a later turn waits on it and
    // is cancelled during that wait.
    if (scenario === 'CURL_WAIT_CANCEL') return outputs.length === 0 ? exec('curl-wait-cancel-cell',
      '// @exec: {"yield_time_ms": 200}\ntext(await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/HANG"}));')
      : say('WAIT_CANCEL_YIELDED');
    if (scenario === 'CURL_WAIT_CANCEL_WAIT') {
      const cellId = [...JSON.stringify(history).matchAll(/Script running with cell ID ([0-9a-f-]+:[0-9]+)/g)].at(-1)?.[1];
      if (outputs.length === 0 && cellId) return respond([{ type: 'function_call', name: 'wait', call_id: 'curl-wait-cancel-wait', arguments: JSON.stringify({ cell_id: cellId, yield_time_ms: 60_000 }) }], false);
      unexpected.push({ kind: 'wait-cancel', cellId: cellId ?? null, outputs: outputs.length }); return say('WAIT_NOT_CANCELLED');
    }
    if (scenario === 'CURL_WAIT_CANCEL_NEXT') return say('WAIT_CANCEL_NEXT_OK');
    unexpected.push({ kind: 'model', scenario: scenario ?? null }); return say('UNEXPECTED');
  };

  const decideClaude = body => {
    const results = body.messages.flatMap(message => Array.isArray(message.content) ? message.content : []).filter(block => block.type === 'tool_result');
    const last = results.at(-1), shown = JSON.stringify(last?.content ?? null);
    const receipt = shown.match(/CURL_CLAUDE_SPAWN: \{\\"agent_id\\":(\d+)/);
    claudeCalls.push({ process: processNumber, tool_results: results.length, last_tool_use_id: last?.tool_use_id ?? null, shown: shown.slice(0, 2000), receipt_agent_id: receipt ? Number(receipt[1]) : null });
    if (results.length === 0) return claudeExec('toolu_curl_claude_spawn', 'const child = await tools.spawn_agent(' + JSON.stringify({ role: 'Curl Codex child', task: 'CURL_CLAUDE_CHILD: submit the synthetic result.', harness: 'codex', model: 'gpt-6.1-sol', thinking: 'low', output_contract: { kind: 'string' } }) + ');\n'
      + 'text("CURL_CLAUDE_SPAWN:", JSON.stringify(child));');
    if (results.length === 1) return receipt
      ? claudeExec('toolu_curl_claude_wait', `const done = await tools.wait_agent({agent_ids:[${receipt[1]}],timeout_ms:20000});\ntext("CURL_CLAUDE_WAIT:", done);`)
      // A real model that cannot see its receipt probes and respawns; stop visibly instead.
      : claudeSse([{ type: 'text', text: 'CLAUDE_SPAWN_RECEIPT_HIDDEN' }]);
    return claudeSse([{ type: 'text', text: /CURL_CLAUDE_WAIT: .*CLAUDE_CHILD_OK/.test(shown) ? 'CLAUDE_MIXED_DONE' : 'CLAUDE_CHILD_RESULT_HIDDEN' }]);
  };
  const sockets = new Set();
  const control = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const call = JSON.parse(raw), url = new URL(call.url);
    const send = (status, body, type = 'application/json') => { res.writeHead(status, { 'content-type': type }); res.end(typeof body === 'string' ? body : JSON.stringify(body)); };
    if (url.href === 'https://control.internal/model') {
      const decision = await decide(JSON.parse(call.body));
      if (decision === 'kill') return void kill('model call in flight: ' + modelCalls.at(-1).scenario);
      return send(200, decision);
    }
    if (url.origin === 'https://api.anthropic.com' && url.pathname === '/v1/models') return send(200, { data: [{ id: claudeSettings.model, display_name: 'Synthetic Claude Sonnet' }], has_more: false });
    if (url.origin === 'https://api.anthropic.com' && url.pathname === '/v1/messages') {
      const body = JSON.parse(call.body);
      if (body.stream === true && JSON.stringify(body.messages).includes('CURL_CLAUDE_ROOT')) return send(200, decideClaude(body), 'text/event-stream');
      unexpected.push({ kind: 'claude', model: body.model, stream: body.stream ?? null }); return send(400, { type: 'error', error: { type: 'invalid_request_error', message: 'unexpected synthetic Claude request' } });
    }
    if (/^https:\/\/(platform\.claude\.com|claude\.ai|api\.anthropic\.com)\//.test(call.url)) {
      const response = await claudeProvider(new Request(call.url, { method: call.method, headers: call.headers, body: call.body ?? undefined }));
      if (response) { res.writeHead(response.status, Object.fromEntries(response.headers)); return void res.end(Buffer.from(await response.arrayBuffer())); }
    }
    if (url.hostname === 'effects.example') {
      const name = url.pathname.split('/').pop();
      effects.push({ process: processNumber, name, method: call.method, at: new Date().toISOString() });
      // B and the child's C reach the external system, then the owner dies
      // before any receipt can return: the outcome is genuinely unknown.
      if (name === 'B' || (['C', 'E'].includes(name) && effects.filter(effect => effect.name === name).length === 1)) return void kill('effect ' + name + ' dispatched, response never returned');
      if (name === 'SLOW') await delay(3000);
      if (name === 'HANG') await delay(30_000);
      return send(200, 'EFFECT_' + name + '_APPLIED\n', 'text/plain');
    }
    if (url.origin === 'https://chatgpt.com' && call.body?.includes('"gpt-6-luna"')) {
      return send(200, { id: 'title', output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'Synthetic recovery' }] }], usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } });
    }
    // Codex children of a Claude root use the stateless Responses HTTPS SSE
    // transport; the same CONTROL policy decides from the full input.
    if (url.href === 'https://chatgpt.com/backend-api/codex/responses' && call.method === 'POST') {
      const body = JSON.parse(call.body);
      if (body.previous_response_id) { unexpected.push({ kind: 'responses-https-previous', url: call.url }); return send(400, { error: 'stateless fixture' }); }
      const decision = await decide({ history: body.input ?? [] });
      if (decision === 'kill') return void kill('HTTPS model call in flight: ' + modelCalls.at(-1).scenario);
      return send(200, decision.map(frame => `event: ${frame.type}\ndata: ${JSON.stringify(frame)}\n\n`).join(''), 'text/event-stream');
    }
    // Default hosted MCP catalog discovery is optional background work; it
    // stays offline here and is recorded, never answered with fabricated tools.
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
  }
  const waitFor = async (label, predicate, timeout = 30_000, interval = 50) => {
    const deadline = Date.now() + timeout;
    for (;;) { const value = await predicate(); if (value) return value; if (Date.now() > deadline) throw new Error('timed out waiting for ' + label); await delay(interval); }
  };

  // ---- curl: bearer travels on stdin config, never argv or saved evidence ----
  let sequence = 0;
  async function curl(label, path, { method = 'GET', body, headers = {}, expected, sse, maxTime = 30 } = {}) {
    const name = `${String(++sequence).padStart(3, '0')}-${label}`, file = join(output, 'curl', name);
    if (body !== undefined) await writeFile(file + '.request.json', JSON.stringify(body, null, 2));
    const args = ['--silent', '--show-error', '--no-buffer', '--max-time', String(maxTime), '--request', method, '--dump-header', file + '.headers', '--config', '-',
      '-H', 'Content-Type: application/json', ...Object.entries(headers).flatMap(([key, value]) => ['-H', `${key}: ${value}`]),
      ...(body === undefined ? [] : ['--data-binary', '@' + file + '.request.json']), new URL(path, base).href];
    const started = Date.now(), child = spawn('curl', args, { stdio: ['pipe', 'pipe', 'pipe'] });
    child.stdin.end(`header = ${JSON.stringify('Authorization: Bearer ' + token)}\n`);
    let stdout = '', stderr = '', buffer = ''; const frames = [];
    child.stdout.on('data', chunk => { stdout += chunk; if (!sse) return; buffer += chunk;
      for (let end; (end = buffer.indexOf('\n\n')) >= 0;) { const frame = buffer.slice(0, end); buffer = buffer.slice(end + 2);
        const data = frame.split('\n').filter(line => line.startsWith('data:')).map(line => line.slice(5).trimStart()).join('\n');
        if (data) { const value = JSON.parse(data); frames.push(value); if (sse(value)) child.kill('SIGTERM'); } } });
    child.stderr.on('data', chunk => { stderr += chunk; });
    const exit = await new Promise(r => child.on('close', (code, signal) => r({ code, signal })));
    const head = await readFile(file + '.headers', 'utf8').catch(() => '');
    const status = Number([...head.matchAll(/^HTTP\/\S+ (\d+)/gm)].at(-1)?.[1] ?? 0);
    await writeFile(file + '.response', stdout);
    let value; try { value = JSON.parse(stdout); } catch { value = stdout; }
    await appendFile(join(output, 'transcript.jsonl'), JSON.stringify({ name, process: fixture?.number, command: ['curl', ...args], stdin: 'header = "Authorization: Bearer <redacted synthetic API key>"',
      status, exit, stderr, duration_ms: Date.now() - started, expected: expected ?? null, request: body ?? null, response: sse ? { frames } : value }) + '\n');
    if (expected !== undefined) assert.equal(status, expected, `${name}: ${stderr} ${stdout.slice(0, 2000)}`);
    return { status, value, frames };
  }
  const turnState = async (label, agent, turn) => (await curl(label, `/v1/agents/${agent}/turns/${turn}`, { expected: 200 })).value;
  const terminal = async (label, agent, turn) => waitFor(label + ' terminal', async () => {
    const value = await turnState(label, agent, turn);
    return ['completed', 'failed', 'cancelled'].includes(value.state) ? value : undefined;
  }, 30_000, 200);
  const history = async (label, agent) => (await curl(label, `/v1/agents/${agent}/events/history?after=0&limit=256`, { expected: 200 })).value;
  // A client renders a tool.call as running until its tool.result, so a call
  // whose owner died must still reach a terminal receipt in durable history.
  const openToolCalls = page => {
    const open = new Map();
    for (const row of page.data) {
      const key = (row.agent_id ?? 'root') + ':' + row.event?.payload?.call_id;
      if (row.event?.type === 'tool.call') open.set(key, { cursor: row.cursor, tool: row.event.payload.tool });
      if (row.event?.type === 'tool.result') open.delete(key);
    }
    assert.equal(page.has_more, false, 'history fits one page');
    return [...open].map(([call, value]) => ({ call, ...value }));
  };
  const summary = {};

  try {
    await start();
    const identity = await fetch(new URL('/__fixture/identity', base), { method: 'POST' });
    token = (await identity.json()).token; assert.ok(token, 'synthetic API key');
    assert.equal((await fetch(new URL('/__fixture/chatgpt', base), { method: 'POST' })).status, 204);

    const terminals = page => {
      const counts = {};
      for (const row of page.data) if (row.event?.type === 'tool.result') {
        const key = (row.agent_id ?? 'root') + ':' + row.event.payload.call_id; counts[key] = (counts[key] ?? 0) + 1;
      }
      return counts;
    };
    // 1. A streamed turn yields its cell and ends; the effect is still running.
    const runKey = randomUUID();
    const admitted = await curl('unawaited-run-sse', '/v1/agent-runs', { method: 'POST', body: { input: 'CURL_UNAWAITED: start the slow effect and answer.', settings },
      headers: { 'Idempotency-Key': runKey, Accept: 'text/event-stream' }, sse: value => value.type === 'turn_completed', maxTime: 60 });
    const receipt = admitted.frames.find(frame => frame.agent_id && frame.turn_id) ?? admitted.frames[0];
    const agent = receipt.agent_id, turn = receipt.turn_id;
    assert.ok(agent && turn, 'streamed admission receipt: ' + JSON.stringify(admitted.frames[0]));
    assert.ok(admitted.frames.some(frame => frame.type === 'turn_completed' && frame.turn_id === turn), 'the turn completes before the effect');
    const atCompletion = await history('unawaited-history-at-completion', agent);
    assert.ok(effects.filter(effect => effect.name === 'SLOW').length === 1, 'the slow effect was dispatched once');
    const nested = 'root:curl-unawaited-cell/code-1';
    assert.ok(atCompletion.data.some(row => row.event?.type === 'tool.call' && row.event.payload.call_id === 'curl-unawaited-cell/code-1'), 'nested call started');
    // 2. A client reconnecting to the public event stream after the turn sees
    // the late terminal result of the nested call, without another wait.
    const lastCursor = atCompletion.data.at(-1).cursor;
    const late = await curl('unawaited-late-result-sse', '/v1/agents/' + agent + '/events', { headers: { Accept: 'text/event-stream', 'Last-Event-ID': lastCursor },
      sse: value => value.type === 'event' && value.event?.type === 'tool.result' && value.event.payload.call_id === 'curl-unawaited-cell/code-1', maxTime: 30 });
    const lateResult = late.frames.find(frame => frame.event?.type === 'tool.result' && frame.event.payload.call_id === 'curl-unawaited-cell/code-1');
    assert.ok(lateResult, 'late nested tool.result is published after the turn');
    assert.equal(lateResult.event.payload.status, 'completed');
    assert.match(JSON.stringify(lateResult.event.payload), /EFFECT_SLOW_APPLIED/);
    const afterRelay = await history('unawaited-history-after-relay', agent);
    assert.deepEqual(openToolCalls(afterRelay), [], 'every started call has a terminal result');
    assert.equal(terminals(afterRelay)[nested], 1, 'the nested call terminates exactly once');
    // 3. Subsequent work on the same agent is usable and adds no duplicate terminal.
    const next = (await curl('unawaited-next-turn', '/v1/agents/' + agent + '/turns', { method: 'POST', body: { input: 'CURL_UNAWAITED_NEXT: answer.' }, headers: { 'Idempotency-Key': randomUUID() }, expected: 202 })).value;
    assert.equal((await terminal('unawaited-next-terminal', agent, next.turn_id)).state, 'completed');
    const final = await history('unawaited-history-final', agent);
    assert.deepEqual(openToolCalls(final), []);
    assert.ok(Object.values(terminals(final)).every(count => count === 1), 'no call has two terminal results: ' + JSON.stringify(terminals(final)));
    assert.equal(effects.filter(effect => effect.name === 'SLOW').length, 1, 'no effect is repeated');
    // 4. The cell yields, then the turn is cancelled while the model is in
    // flight: the nested call gets exactly one failed, unknown-outcome result.
    const cancelRun = (await curl('cancel-run', '/v1/agents/' + agent + '/turns', { method: 'POST', body: { input: 'CURL_CANCEL: start the hanging effect.' }, headers: { 'Idempotency-Key': randomUUID() }, expected: 202 })).value;
    await cancelInference.wait;
    await curl('cancel-active-turn', '/v1/agents/' + agent + '/turns/' + cancelRun.turn_id + '/cancel', { method: 'POST', headers: { 'Idempotency-Key': randomUUID() }, expected: 202 });
    assert.ok(['cancelled', 'failed'].includes((await terminal('cancel-terminal', agent, cancelRun.turn_id)).state));
    const cancelled = await history('cancel-history', agent);
    const cancelResults = cancelled.data.filter(row => row.event?.type === 'tool.result' && row.event.payload.call_id === 'curl-cancel-cell/code-1');
    assert.equal(cancelResults.length, 1, 'the cancelled nested call terminates exactly once');
    assert.equal(cancelResults[0].event.payload.status, 'failed');
    assert.match(JSON.stringify(cancelResults[0].event.payload), /unknown/);
    assert.deepEqual(openToolCalls(cancelled), [], 'no call stays running after cancellation');
    assert.ok(Object.values(terminals(cancelled)).every(count => count === 1), JSON.stringify(terminals(cancelled)));
    const cancelNext = (await curl('cancel-next-turn', '/v1/agents/' + agent + '/turns', { method: 'POST', body: { input: 'CURL_CANCEL_NEXT: answer.' }, headers: { 'Idempotency-Key': randomUUID() }, expected: 202 })).value;
    assert.equal((await terminal('cancel-next-terminal', agent, cancelNext.turn_id)).state, 'completed');
    const settled = await history('cancel-history-final', agent);
    assert.deepEqual(openToolCalls(settled), []);
    assert.ok(Object.values(terminals(settled)).every(count => count === 1), JSON.stringify(terminals(settled)));
    // 5. Cancelling a later turn during its wait on an earlier turn's cell
    // settles that cell's nested call and the wait exactly once.
    const yieldTurn = (await curl('wait-cancel-yield', '/v1/agents/' + agent + '/turns', { method: 'POST', body: { input: 'CURL_WAIT_CANCEL: start the hanging effect and answer.' }, headers: { 'Idempotency-Key': randomUUID() }, expected: 202 })).value;
    assert.equal((await terminal('wait-cancel-yield-terminal', agent, yieldTurn.turn_id)).state, 'completed');
    const waitTurn = (await curl('wait-cancel-wait', '/v1/agents/' + agent + '/turns', { method: 'POST', body: { input: 'CURL_WAIT_CANCEL_WAIT: wait on the running cell.' }, headers: { 'Idempotency-Key': randomUUID() }, expected: 202 })).value;
    await waitFor('wait call started', async () => (await history('wait-cancel-poll', agent)).data.some(row => row.event?.type === 'tool.call' && row.event.payload.call_id === 'curl-wait-cancel-wait'), 30_000, 200);
    await curl('wait-cancel-active-turn', '/v1/agents/' + agent + '/turns/' + waitTurn.turn_id + '/cancel', { method: 'POST', headers: { 'Idempotency-Key': randomUUID() }, expected: 202 });
    assert.ok(['cancelled', 'failed'].includes((await terminal('wait-cancel-terminal', agent, waitTurn.turn_id)).state));
    const waitCancelled = await history('wait-cancel-history', agent);
    const waitNested = waitCancelled.data.filter(row => row.event?.type === 'tool.result' && row.event.payload.call_id === 'curl-wait-cancel-cell/code-1');
    assert.equal(waitNested.length, 1, 'the waited earlier-turn nested call terminates exactly once');
    assert.equal(waitNested[0].event.payload.status, 'failed');
    assert.deepEqual(openToolCalls(waitCancelled), [], 'no call stays running after cancelling the wait');
    assert.ok(Object.values(terminals(waitCancelled)).every(count => count === 1), JSON.stringify(terminals(waitCancelled)));
    const waitNext = (await curl('wait-cancel-next-turn', '/v1/agents/' + agent + '/turns', { method: 'POST', body: { input: 'CURL_WAIT_CANCEL_NEXT: answer.' }, headers: { 'Idempotency-Key': randomUUID() }, expected: 202 })).value;
    assert.equal((await terminal('wait-cancel-next-terminal', agent, waitNext.turn_id)).state, 'completed');
    summary.wait_cancel = { turn: waitTurn.turn_id, result: waitNested[0].event.payload.status, terminals: terminals(await history('wait-cancel-final', agent)) };
    summary.cancel = { turn: cancelRun.turn_id, result: cancelResults[0].event.payload.status, terminals: terminals(settled) };
    summary.unawaited = { agent, turn, late_result_cursor: late.frames.find(frame => frame === lateResult)?.cursor ?? null, terminals: terminals(final) };

    assert.deepEqual(unexpected, []);
  } finally {
    await writeFile(join(output, 'trace.json'), JSON.stringify({ summary, kills, model_calls: modelCalls, effects, unexpected, offline_mcp_discovery: discovery.length }, null, 2));
    await kill('test cleanup');
    for (const socket of sockets) socket.destroy();
    await new Promise(r => control.close(r));
    console.log(JSON.stringify({ evidence: output, kills: kills.length, effects: effects.map(effect => effect.name), model_calls: modelCalls.length }));
  }
});
