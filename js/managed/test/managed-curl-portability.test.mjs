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

// Public HTTP portability journey: every API call is the curl executable
// against the normal account ingress, Managed API and Egress workers on
// persisted workerd SQLite/R2 (one process, the same fixture as
// managed-curl-recovery). Only the model provider is stubbed. A Codex root
// completes a turn that spawns and waits on a real Code Mode child, so its
// durable task-tree journal ("<stateId>:subagents") is non-empty; the managed
// archive is exported, rejected in representative tampered forms, imported
// into a new agent, and the destination serves a further turn whose provider
// request carries the preserved history.
// It also imports a keyless (UUIDv7 state ID) source beside its still-resident
// fenced source object, and shows the export gate refusing a Codex root with a
// cross-harness Claude child (409 subagent_routes_not_portable) without fencing it.
const root = fileURLToPath(new URL('../../..', import.meta.url));
const owner = '11111111-1111-4111-8111-111111111166';
const settings = { model: 'gpt-6.1-sol', thinking: 'low', reasoning_mode: 'standard', fast_mode: false };
const flags = ['nodejs_compat', 'durable_object_io_tasks_prevent_eviction', 'enable_request_signal'];

// Synthetic identity bootstrap only (as managed-curl-recovery); behavior under
// test is reached exclusively through the public /v1 routes. The second key
// is the same account grant without agents:portability; the third also lacks
// memory:read/memory:write, so a child it delegates to is publicly observable.
const bootstrap = [
  "import managed from './src/index.ts';export * from './src/index.ts';",
  "import {ensureAccount,createApiKey} from './src/account-auth.ts';",
  "const key=async(env,select,label)=>{await ensureAccount(env,'" + owner + "',true);",
  " const auth=await(await env.NANOCODEX_USERS.getByName('" + owner + "').fetch('https://user.internal/authorization')).json();",
  " return createApiKey(env,{kind:'api_key',userId:'" + owner + "',...auth.grant,capabilities:select(auth.grant.capabilities),subjectId:'fixture:" + owner + "',credentialId:'fixture'},label);};",
  "export default {async fetch(request,env,ctx){const path=new URL(request.url).pathname;",
  "if(path==='/__fixture/identity')return Response.json(await key(env,c=>c,'synthetic-curl-portability'));",
  "if(path==='/__fixture/identity-without-portability')return Response.json(await key(env,c=>c.filter(x=>x!=='agents:portability'),'synthetic-curl-no-portability'));",
  "if(path==='/__fixture/identity-without-memory')return Response.json(await key(env,c=>c.filter(x=>!['agents:portability','memory:read','memory:write'].includes(x)),'synthetic-curl-no-memory'));",
  "if(path==='/__fixture/chatgpt'){const expires_at=(Math.ceil(Date.now()/1000)+3600)*1000;",
  " const claims={exp:Math.ceil(expires_at/1000),'https://api.openai.com/auth':{chatgpt_account_id:'synthetic-account',chatgpt_account_is_fedramp:false}};",
  " const jwt=btoa(JSON.stringify({alg:'none'})).replaceAll('=','')+'.'+btoa(JSON.stringify(claims)).replaceAll('=','')+'.fixture';",
  " return env.NANOCODEX.fetch('https://broker.internal/users/" + owner + "/credentials/chatgpt',{method:'PUT',headers:{'content-type':'application/json'},",
  "  body:JSON.stringify({access_token:jwt,refresh_token:'synthetic-refresh',account_id:'synthetic-account',expires_at,fedramp:false})});}",
  "return managed.fetch(request,env,ctx);}};",
].join('\n');

// Production WebSocket transport. Socket-local history follows
// previous_response_id; CONTROL (the parent test) owns every decision.
async function providerFetch(request, env) {
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
}
const providerSource = 'export default { fetch: ' + providerFetch.toString() + ' };';

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
  const prelude = requires.map((n, i) => 'import * as builtin' + i + ' from ' + JSON.stringify(n) + ';').join('\n')
    + '\nconst requireMap={' + requires.map((n, i) => JSON.stringify(n) + ':builtin' + i).join(',') + "};const require=name=>{if(!requireMap[name])throw new Error('Unexpected require '+name);return requireMap[name];};\n";
  const path = join(output, name + '.mjs'); await writeFile(path, prelude + code);
  const lazy = [...code.matchAll(/import\("([^"]*just-bash-lazy\.mjs)"\)/g)].map(m => m[1]);
  return [{ type: 'ESModule', path }, ...[...new Set(lazy)].map(path => ({ type: 'ESModule', path })), ...[...wasm].map(path => ({ type: 'CompiledWasm', path }))];
}

test('curl exports a managed agent with its completed child and imports it into a new agent that keeps serving the preserved history', { timeout: 360_000 }, async () => {
  const output = join(root, 'output/managed-curl-portability', new Date().toISOString().replaceAll(':', '-') + '-' + randomUUID().slice(0, 8));
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
    { name: 'egress', modulesRoot: '/', modules: await bundle(output, 'egress', "export * from './src/egress.ts';export {default} from './src/egress.ts';", join(root, 'js/egress')),
      compatibilityDate: '2026-07-29', compatibilityFlags: ['nodejs_compat', 'enable_request_signal'],
      bindings: { ENVIRONMENT: 'test', CREDENTIAL_ENCRYPTION_KEY: 'MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY' }, serviceBindings: { MANAGED_AGENT_OWNERSHIP: { name: 'managed', entrypoint: 'ManagedAgentOwnership' } },
      durableObjects: Object.fromEntries([['USER_CREDENTIALS', 'UserCredentialBroker'], ['AGENT_SUBJECTS', 'AgentSubjectDirectory'], ['USER_CONNECTORS', 'UserConnectorBroker'],
        ['MCP_CONNECTIONS', 'McpConnectionDirectory'], ['SPOTIFY_RATE_LIMITS', 'SpotifyRateLimit'], ['GMAIL_PUSH_MAILBOXES', 'GmailPushMailbox']].map(([binding, className]) => [binding, { className, useSQLite: true }])),
      outboundService: 'provider' },
    { name: 'provider', modules: true, script: providerSource, compatibilityDate: '2026-07-29' }];
  await writeFile(join(output, 'workers.json'), JSON.stringify(workers));

  // ---- External stub boundary (the parent owns all provider decisions) ----
  const modelCalls = [], unexpected = [], discovery = [];
  const respond = (items, end) => { const id = 'resp_' + randomUUID();
    return [{ type: 'response.created', response: { id, status: 'in_progress' } },
      ...items.filter(item => item.type === 'message').map(item => ({ type: 'response.output_text.delta', output_index: 0, delta: item.content[0].text })),
      { type: 'response.completed', response: { id, status: 'completed', ...(end ? { end_turn: true } : {}), output: items, usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } }]; };
  const say = text => respond([{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] }], true);
  const exec = (callId, source) => respond([{ type: 'custom_tool_call', name: 'exec', call_id: callId, input: source }], false);
  // Most specific first: the destination turn's history still holds the root marker.
  const markers = ['PORT_KEYLESS_DEST', 'PORT_KEYLESS_ROOT', 'PORT_CLAUDE_AFTER', 'PORT_CLAUDE_ROOT', 'PORT_DEST_WIDEN', 'PORT_CHILD_WIDEN', 'PORT_DEST_FOLLOWUP', 'PORT_CHILD_FOLLOWUP', 'PORT_CHILD_TASK', 'PORT_ROOT'];
  // Memory tools authorize with the calling session's bound authority.
  const memoryProbe = callId => exec(callId, 'text(JSON.stringify(await (async () => tools.memories__status({}))().then(() => ({ memory: "allowed" }), error => ({ memory: "denied", error: String(error?.message ?? error) }))));');
  const decide = ({ history }) => {
    const user = JSON.stringify(history.filter(item => item.role === 'user'));
    const scenario = markers.find(marker => user.includes(marker));
    const since = history.findLastIndex(item => item.role === 'user' && JSON.stringify(item.content).includes(scenario));
    const fresh = history.slice(since + 1).filter(item => /_call_output$/.test(item.type ?? ''));
    modelCalls.push({ scenario, fresh_outputs: fresh.length, at: new Date().toISOString(), history,
      items: history.map(item => item.type === 'message' || !item.type ? 'message:' + item.role : item.type + (item.call_id ? ':' + item.call_id : '')) });
    switch (scenario) {
      case 'PORT_ROOT': return [
        () => exec('port-root-spawn', 'text(await tools.spawn_agent(' + JSON.stringify({ role: 'Portability child', task: 'PORT_CHILD_TASK: submit the synthetic result.', model: 'sol', thinking: 'low', output_contract: { kind: 'string' } }) + '));'),
        () => exec('port-root-wait', 'text(await tools.wait_agent({agent_ids:[1],timeout_ms:20000}));'),
      ][fresh.length]?.() ?? say('PORT_ROOT_DONE');
      case 'PORT_CHILD_TASK': return fresh.length === 0 ? exec('port-child-submit', 'text(await tools.submit_result({output:"PORT_CHILD_OK"}));') : say('PORT_CHILD_OK');
      // At the destination: delegate again to the imported child 1, whose
      // conversation is restored from the nested journal and its R2 records.
      case 'PORT_DEST_FOLLOWUP': return [
        () => exec('port-dest-send', 'text(await tools.send_agent_message({agent_id:1,purpose:"delegate",message:"PORT_CHILD_FOLLOWUP: submit the follow-up result."}));'),
        () => exec('port-dest-wait', 'text(await tools.wait_agent({agent_ids:[1],timeout_ms:20000}));'),
      ][fresh.length]?.() ?? say('PORT_DEST_OK');
      case 'PORT_CHILD_FOLLOWUP': return [
        () => memoryProbe('port-child-memory'),
        () => exec('port-child-followup-submit', 'text(await tools.submit_result({output:"PORT_CHILD_FOLLOWUP_OK"}));'),
      ][fresh.length]?.() ?? say('PORT_CHILD_FOLLOWUP_OK');
      // A later destination turn under the full key delegates again.
      case 'PORT_DEST_WIDEN': return [
        () => memoryProbe('port-dest-widen-memory'),
        () => exec('port-dest-widen-send', 'text(await tools.send_agent_message({agent_id:1,purpose:"delegate",message:"PORT_CHILD_WIDEN: probe memory, then submit."}));'),
        () => exec('port-dest-widen-wait', 'text(await tools.wait_agent({agent_ids:[1],timeout_ms:20000}));'),
      ][fresh.length]?.() ?? say('PORT_DEST_WIDEN_OK');
      case 'PORT_CHILD_WIDEN': return [
        () => memoryProbe('port-child-widen-memory'),
        () => exec('port-child-widen-submit', 'text(await tools.submit_result({output:"PORT_CHILD_WIDEN_OK"}));'),
      ][fresh.length]?.() ?? say('PORT_CHILD_WIDEN_OK');
      // Keyless (UUIDv7) source and its destination: plain answers.
      case 'PORT_KEYLESS_ROOT': return say('PORT_KEYLESS_ROOT_DONE');
      case 'PORT_KEYLESS_DEST': return say('PORT_KEYLESS_DEST_OK');
      // A Codex root delegating to a cross-harness Claude child.
      case 'PORT_CLAUDE_ROOT': return [
        () => exec('port-claude-spawn', 'text(await tools.spawn_agent(' + JSON.stringify({ role: 'Claude child', task: 'PORT_CLAUDE_CHILD: submit the synthetic result.', harness: 'claude', model: claudeModel, output_contract: { kind: 'string' } }) + '));'),
        () => exec('port-claude-wait', 'text(await tools.wait_agent({agent_ids:[1],timeout_ms:20000}));'),
      ][fresh.length]?.() ?? say('PORT_CLAUDE_ROOT_DONE');
      case 'PORT_CLAUDE_AFTER': return say('PORT_CLAUDE_AFTER_OK');
      default: unexpected.push({ kind: 'model', user: user.slice(0, 2000) }); return say('UNEXPECTED');
    }
  };
  // Anthropic Messages stub for the cross-harness Claude child.
  const claudeModel = 'claude-sonnet-4-6', claudeCalls = [];
  const claudeSse = blocks => [
    { type: 'message_start', message: { id: 'msg_' + randomUUID(), type: 'message', role: 'assistant', model: claudeModel, content: [], usage: { input_tokens: 10, output_tokens: 0 } } },
    ...blocks.flatMap((block, index) => block.type === 'tool_use'
      ? [{ type: 'content_block_start', index, content_block: { ...block, input: {} } }, { type: 'content_block_delta', index, delta: { type: 'input_json_delta', partial_json: JSON.stringify(block.input) } }, { type: 'content_block_stop', index }]
      : [{ type: 'content_block_start', index, content_block: { type: 'text', text: '' } }, { type: 'content_block_delta', index, delta: { type: 'text_delta', text: block.text } }, { type: 'content_block_stop', index }]),
    { type: 'message_delta', delta: { stop_reason: blocks.some(block => block.type === 'tool_use') ? 'tool_use' : 'end_turn', stop_sequence: null }, usage: { output_tokens: 1 } },
    { type: 'message_stop' },
  ].map(event => 'event: ' + event.type + '\ndata: ' + JSON.stringify(event) + '\n\n').join('');
  const decideClaudeChild = body => {
    const results = body.messages.flatMap(message => Array.isArray(message.content) ? message.content : []).filter(block => block.type === 'tool_result');
    claudeCalls.push({ model: body.model, tool_results: results.length, at: new Date().toISOString() });
    return results.length === 0
      ? claudeSse([{ type: 'tool_use', id: 'toolu_port_claude_submit', name: 'exec', input: { code: 'text(await tools.submit_result({output:"PORT_CLAUDE_CHILD_OK"}));' } }])
      : claudeSse([{ type: 'text', text: 'PORT_CLAUDE_CHILD_OK' }]);
  };
  const sockets = new Set();
  const control = createServer(async (req, res) => {
    let raw = ''; for await (const chunk of req) raw += chunk;
    const call = JSON.parse(raw), url = new URL(call.url);
    const send = (status, body, type = 'application/json') => { res.writeHead(status, { 'content-type': type }); res.end(typeof body === 'string' ? body : JSON.stringify(body)); };
    if (url.href === 'https://control.internal/model') return send(200, decide(JSON.parse(call.body)));
    if (url.origin === 'https://chatgpt.com' && call.body?.includes('"gpt-6-luna"')) {
      return send(200, { id: 'title', output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'Synthetic portability' }] }], usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } });
    }
    if (url.origin === 'https://api.anthropic.com' && url.pathname === '/v1/models') return send(200, { data: [{ id: claudeModel, display_name: 'Synthetic Claude Sonnet' }], has_more: false });
    if (url.origin === 'https://api.anthropic.com' && url.pathname === '/v1/messages') {
      const body = JSON.parse(call.body);
      if (body.stream === true && JSON.stringify(body.messages).includes('PORT_CLAUDE_CHILD')) return send(200, decideClaudeChild(body), 'text/event-stream');
      unexpected.push({ kind: 'claude', model: body.model, stream: body.stream ?? null }); return send(400, { type: 'error', error: { type: 'invalid_request_error', message: 'unexpected synthetic Claude request' } });
    }
    if (/^https:\/\/(platform\.claude\.com|claude\.ai|api\.anthropic\.com)\//.test(call.url)) {
      const response = await claudeProvider(new Request(call.url, { method: call.method, headers: call.headers, body: call.body ?? undefined }));
      if (response) { res.writeHead(response.status, Object.fromEntries(response.headers)); return void res.end(Buffer.from(await response.arrayBuffer())); }
    }
    // Default hosted MCP catalog discovery is optional background work; it
    // stays offline here and is recorded, never answered with fabricated tools.
    if (call.method === 'POST' && /\/mcp\/?$|^https:\/\/mcp\./.test(call.url)) { discovery.push(call.url); return send(503, { error: 'synthetic offline' }); }
    unexpected.push({ kind: 'http', method: call.method, url: call.url });
    send(404, { error: 'unexpected synthetic external request' });
  });
  control.on('connection', socket => { sockets.add(socket); socket.on('close', () => sockets.delete(socket)); });
  await new Promise(resolvePromise => control.listen(0, '127.0.0.1', resolvePromise));
  const controlBase = 'http://127.0.0.1:' + control.address().port;

  let fixture, base, token, limitedToken, memorylessToken;
  const start = () => new Promise((resolvePromise, reject) => {
    const child = fork(fileURLToPath(new URL('./fixtures/managed-curl-recovery-process.mjs', import.meta.url)), [output, controlBase], { detached: true, stdio: ['ignore', 'pipe', 'pipe', 'ipc'] });
    for (const stream of ['stdout', 'stderr']) child[stream].on('data', chunk => void appendFile(join(output, 'workerd.log'), chunk));
    child.once('error', reject); child.once('exit', code => reject(new Error('fixture exited before ready: ' + code)));
    child.once('message', message => { child.removeAllListeners('exit'); fixture = { child, exited: new Promise(r => child.once('exit', r)) }; base = message.ready; resolvePromise(); });
  });
  async function stop() {
    const current = fixture; if (!current) return; fixture = undefined;
    process.kill(-current.child.pid, 'SIGKILL'); await current.exited;
  }
  const waitFor = async (label, predicate, timeout = 30_000, interval = 200) => {
    const deadline = Date.now() + timeout;
    for (;;) { const value = await predicate(); if (value) return value; if (Date.now() > deadline) throw new Error('timed out waiting for ' + label); await delay(interval); }
  };

  // ---- curl: bearer travels on stdin config, never argv or saved evidence ----
  let sequence = 0;
  const assertions = [];
  const check = (label, fn) => { fn(); assertions.push(label); };
  async function curl(label, path, { method = 'GET', body, headers = {}, expected, bearer = token, maxTime = 30 } = {}) {
    const name = String(++sequence).padStart(3, '0') + '-' + label, file = join(output, 'curl', name);
    if (body !== undefined) await writeFile(file + '.request.json', JSON.stringify(body, null, 2));
    const args = ['--silent', '--show-error', '--max-time', String(maxTime), '--request', method, '--dump-header', file + '.headers', '--config', '-',
      '-H', 'Content-Type: application/json', ...Object.entries(headers).flatMap(([key, value]) => ['-H', key + ': ' + value]),
      ...(body === undefined ? [] : ['--data-binary', '@' + file + '.request.json']), new URL(path, base).href];
    const started = Date.now(), child = spawn('curl', args, { stdio: ['pipe', 'pipe', 'pipe'] });
    child.stdin.end('header = ' + JSON.stringify('Authorization: Bearer ' + bearer) + '\n');
    let stdout = '', stderr = '';
    child.stdout.on('data', chunk => { stdout += chunk; });
    child.stderr.on('data', chunk => { stderr += chunk; });
    const exit = await new Promise(r => child.on('close', (code, signal) => r({ code, signal })));
    const head = await readFile(file + '.headers', 'utf8').catch(() => '');
    const status = Number([...head.matchAll(/^HTTP\/\S+ (\d+)/gm)].at(-1)?.[1] ?? 0);
    await writeFile(file + '.response', stdout);
    let value; try { value = JSON.parse(stdout); } catch { value = stdout; }
    await appendFile(join(output, 'transcript.jsonl'), JSON.stringify({ name, command: ['curl', ...args],
      stdin: 'header = "Authorization: Bearer <redacted synthetic API key' + (bearer === limitedToken ? ' without agents:portability' : bearer === memorylessToken ? ' without agents:portability or memory' : '') + '>"',
      status, exit, stderr, duration_ms: Date.now() - started, expected: expected ?? null, request: body ?? null, response: value }) + '\n');
    if (expected !== undefined) assert.equal(status, expected, name + ': ' + stderr + ' ' + stdout.slice(0, 2000));
    return { status, value };
  }
  const turnState = async (label, agent, turn) => (await curl(label, '/v1/agents/' + agent + '/turns/' + turn, { expected: 200 })).value;
  const terminal = async (label, agent, turn) => waitFor(label + ' terminal', async () => {
    const value = await turnState(label, agent, turn);
    return ['completed', 'failed', 'cancelled'].includes(value.state) ? value : undefined;
  });
  // A client follows history pages; an adopted archive may end a page early.
  const history = async (label, agent) => {
    const data = [], pages = [];
    for (let after = '0'; ;) {
      const page = (await curl(label + '-page-' + (pages.length + 1), '/v1/agents/' + agent + '/events/history?after=' + after + '&limit=256', { expected: 200 })).value;
      pages.push({ rows: page.data.length, has_more: page.has_more });
      data.push(...page.data);
      if (!page.has_more) return { data, pages };
      assert.ok(page.data.length > 0 && pages.length < 20, 'history paging progresses');
      after = page.data.at(-1).cursor;
    }
  };
  // Visible transcript identity of a history row, independent of the agent id.
  const visible = row => JSON.stringify(row);
  const summary = {};

  try {
    await start();
    token = (await (await fetch(new URL('/__fixture/identity', base), { method: 'POST' })).json()).token;
    limitedToken = (await (await fetch(new URL('/__fixture/identity-without-portability', base), { method: 'POST' })).json()).token;
    memorylessToken = (await (await fetch(new URL('/__fixture/identity-without-memory', base), { method: 'POST' })).json()).token;
    assert.ok(token && limitedToken && memorylessToken, 'synthetic API keys');
    assert.equal((await fetch(new URL('/__fixture/chatgpt', base), { method: 'POST' })).status, 204);

    // 1. A real root turn that spawns, waits on and completes a Code Mode child.
    // Keyed creation (the public default for retried clients): the agent ID
    // is a UUIDv8 derived from the Idempotency-Key, never a Rust session ID.
    const run = (await curl('source-run-admit', '/v1/agent-runs', { method: 'POST', body: { input: 'PORT_ROOT: delegate one synthetic result to a child, then answer.', settings },
      headers: { 'Idempotency-Key': randomUUID() }, expected: 201 })).value;
    const source = run.agent_id;
    check('keyed source agent ID is a UUIDv8', () => assert.match(source, /^[0-9a-f]{8}-[0-9a-f]{4}-8[0-9a-f]{3}-/));
    const done = await terminal('source-run-terminal', source, run.turn_id);
    check('source turn completed', () => assert.equal(done.state, 'completed', JSON.stringify(done)));
    check('child completed through the provider', () => assert.ok(modelCalls.some(call => call.scenario === 'PORT_CHILD_TASK' && call.fresh_outputs === 1)));
    const sourceHistory = await history('source-history', source);
    check('source history shows the root answer and the child result', () => {
      assert.match(JSON.stringify(sourceHistory), /PORT_ROOT_DONE/);
      assert.match(JSON.stringify(sourceHistory), /PORT_CHILD_OK/);
    });

    // 2. Export requires agents:portability; the forbidden attempt fences nothing.
    const forbiddenExport = await curl('export-without-portability', '/v1/agents/' + source + '/durability', { method: 'POST', bearer: limitedToken });
    check('export without agents:portability is 403', () => assert.equal(forbiddenExport.status, 403, JSON.stringify(forbiddenExport.value)));
    let exported;
    for (let attempt = 1; attempt <= 30; attempt++) {
      exported = await curl('export-' + attempt, '/v1/agents/' + source + '/durability', { method: 'POST' });
      if (exported.status !== 202) break;
      await delay(500);
    }
    check('export completes with 200', () => assert.equal(exported.status, 200, JSON.stringify(exported.value).slice(0, 2000)));
    const archive = exported.value;
    await writeFile(join(output, 'archive.json'), JSON.stringify(archive, null, 2));
    const journal = archive.durability.subagents;
    summary.archive = { format: archive.format, source_agent_id: archive.source_agent_id, state_id: archive.durability.stateId, revision: archive.durability.revision,
      inline_records: archive.durability.records.length, subagents: journal && { state_id: journal.stateId, revision: journal.revision, inline_records: journal.records.length, payload_bytes: journal.payload.length },
      managed_durability_records: archive.managed_durability_records };
    check('archive carries the nested task-tree journal; records travel through R2, not inline', () => {
      assert.equal(archive.format, 'nanocodex-managed-durability-state-v2');
      assert.equal(archive.source_agent_id, source);
      assert.deepEqual(archive.durability.records, []);
      assert.ok(journal, 'durability.subagents is present after a completed child');
      assert.equal(journal.stateId, archive.durability.stateId + ':subagents');
      assert.equal(journal.format, 'nanocodex-durability-state-v2');
      assert.match(journal.revision, /^[1-9][0-9]*$/);
      assert.deepEqual(journal.records, []);
    });
    const sourceAfter = await curl('source-turn-after-export', '/v1/agents/' + source + '/turns', { method: 'POST', body: { input: 'PORT_ROOT: must not run.' }, headers: { 'Idempotency-Key': randomUUID() } });
    check('exported source is fenced (409)', () => assert.equal(sourceAfter.status, 409, JSON.stringify(sourceAfter.value)));

    // 3. Representative import rejections, none of which creates an agent.
    const callsBefore = modelCalls.length;
    const importAs = (label, durability, extra = {}, bearer = token) => curl(label, '/v1/agents', { method: 'POST', body: { durability, ...extra }, headers: { 'Idempotency-Key': randomUUID() }, bearer });
    const forbiddenImport = await importAs('import-without-portability', archive, {}, limitedToken);
    check('import without agents:portability is 403', () => assert.equal(forbiddenImport.status, 403, JSON.stringify(forbiddenImport.value)));
    const wrongState = await importAs('import-tampered-subagents-state-id', { ...archive, durability: { ...archive.durability, subagents: { ...journal, stateId: archive.durability.stateId + ':other' } } });
    check('tampered nested subagents stateId is 400', () => { assert.equal(wrongState.status, 400); assert.equal(wrongState.value.error, 'invalid_durability_import'); });
    const inlineRecords = await importAs('import-nested-inline-records', { ...archive, durability: { ...archive.durability, subagents: { ...journal, records: [{ key: 'c:forged', value: 'forged' }] } } });
    check('nested inline journal records are 400', () => { assert.equal(inlineRecords.status, 400); assert.equal(inlineRecords.value.error, 'invalid_durability_import'); });
    const mismatch = await importAs('import-settings-mismatch', archive, { settings: { ...settings, thinking: 'high' } });
    check('settings mismatch is 400', () => { assert.equal(mismatch.status, 400); assert.match(JSON.stringify(mismatch.value), /settings must match/); });
    check('rejected imports never reach the provider', () => assert.equal(modelCalls.length, callsBefore));

    // 4. Import into a new agent; a keyed replay converges on the same agent.
    const importKey = randomUUID();
    let imported;
    for (let attempt = 1; attempt <= 20; attempt++) {
      imported = await curl('import-' + attempt, '/v1/agents', { method: 'POST', body: { durability: archive }, headers: { 'Idempotency-Key': importKey }, maxTime: 60 });
      if (imported.status !== 503) break;
      await delay(1000);
    }
    check('import creates a new agent (201)', () => assert.equal(imported.status, 201, JSON.stringify(imported.value)));
    const destination = imported.value.agent_id;
    check('destination is a new agent carrying the source durability identity', () => {
      assert.notEqual(destination, source);
      assert.equal(imported.value.durability_id, archive.durability.stateId);
    });
    const replay = await curl('import-idempotent-replay', '/v1/agents', { method: 'POST', body: { durability: archive }, headers: { 'Idempotency-Key': importKey } });
    check('keyed import replay returns the same agent', () => { assert.ok(replay.status === 200 || replay.status === 201, String(replay.status)); assert.equal(replay.value.agent_id, destination); });

    // 5. Visible continuity: the source transcript and turn receipt are served by the destination.
    const destinationHistory = await history('destination-history', destination);
    summary.history_pages = { source: sourceHistory.pages, destination: destinationHistory.pages };
    check('destination history reproduces the source transcript, including the child rows', () => {
      assert.deepEqual(destinationHistory.data.slice(0, sourceHistory.data.length).map(visible), sourceHistory.data.map(visible));
    });
    const receipt = await turnState('destination-source-turn-receipt', destination, run.turn_id);
    check('source turn receipt is served by the destination', () => assert.equal(receipt.state, 'completed', JSON.stringify(receipt)));

    // 6. A further turn at the destination: the provider receives the preserved history.
    const next = (await curl('destination-turn-admit', '/v1/agents/' + destination + '/turns', { method: 'POST', body: { input: 'PORT_DEST_FOLLOWUP: delegate again to the same child, then answer.' },
      headers: { 'Idempotency-Key': randomUUID() }, expected: 202, bearer: memorylessToken })).value;
    // The narrower key (no memory capabilities) admitted the first destination
    // turn that delegates, so it binds the imported child.
    const nextDone = await terminal('destination-turn-terminal', destination, next.turn_id);
    check('destination turn completed', () => assert.equal(nextDone.state, 'completed', JSON.stringify(nextDone)));
    const destCalls = modelCalls.filter(call => call.scenario === 'PORT_DEST_FOLLOWUP');
    const first = JSON.stringify(destCalls[0]?.history ?? null);
    check('destination provider request carries the preserved source conversation', () => {
      assert.match(first, /PORT_ROOT: delegate one synthetic result/);
      assert.match(first, /PORT_ROOT_DONE/);
      assert.match(first, /port-root-spawn/);
      assert.match(first, /PORT_CHILD_OK/);
    });
    const childFollowup = modelCalls.filter(call => call.scenario === 'PORT_CHILD_FOLLOWUP');
    const childFirst = JSON.stringify(childFollowup[0]?.history ?? null);
    summary.destination_child_followup = childFollowup.map(call => ({ fresh_outputs: call.fresh_outputs, items: call.items }));
    check('imported child 1 is restored: its provider request carries its preserved conversation', () => {
      assert.ok(childFollowup.length >= 1, 'the delegated child reached the provider at the destination');
      assert.match(childFirst, /PORT_CHILD_TASK: submit the synthetic result/);
      assert.match(childFirst, /port-child-submit/);
      assert.match(childFirst, /PORT_CHILD_FOLLOWUP: submit the follow-up result/);
    });
    const waited = JSON.stringify(destCalls.at(-1)?.history.filter(item => item.call_id === 'port-dest-wait' && /_call_output$/.test(item.type ?? '')) ?? null);
    summary.destination_wait = waited.slice(0, 3000);
    check('destination root observes the restored child result', () => assert.match(waited, /PORT_CHILD_FOLLOWUP_OK/));
    const finalHistory = JSON.stringify(await history('destination-history-after-turn', destination));
    check('destination transcript shows the new answer', () => assert.match(finalHistory, /PORT_DEST_OK/));
    // 7. Least privilege, publicly: the imported child holds exactly the
    // narrower authority of the destination turn that bound it, never the
    // source's; a later broader turn reuses that binding and cannot widen it.
    const outputOf = callId => JSON.stringify(modelCalls.flatMap(call => call.history)
      .filter(item => item.call_id === callId && /_call_output$/.test(item.type ?? '')).at(-1) ?? null);
    check('child bound by the narrower destination turn is denied memory', () => {
      assert.match(outputOf('port-child-memory'), /memory capability is required/);
      assert.doesNotMatch(outputOf('port-child-memory'), /allowed/);
    });
    const widen = (await curl('destination-widen-admit', '/v1/agents/' + destination + '/turns', { method: 'POST', body: { input: 'PORT_DEST_WIDEN: probe memory, delegate again, then answer.' },
      headers: { 'Idempotency-Key': randomUUID() }, expected: 202 })).value;
    const widenDone = await terminal('destination-widen-terminal', destination, widen.turn_id);
    summary.memory_probes = Object.fromEntries(['port-child-memory', 'port-dest-widen-memory', 'port-child-widen-memory'].map(id => [id, outputOf(id).slice(0, 1000)]));
    check('full-key destination turn completed', () => assert.equal(widenDone.state, 'completed', JSON.stringify(widenDone)));
    check('the full-key destination root may read memory', () => assert.match(outputOf('port-dest-widen-memory'), /allowed/));
    check('re-delegation from a broader turn never widens the imported child', () => {
      assert.match(outputOf('port-child-widen-memory'), /memory capability is required/);
      assert.doesNotMatch(outputOf('port-child-widen-memory'), /allowed/);
    });
    check('the imported child completes the broader turn delegation', () => assert.match(outputOf('port-dest-widen-wait'), /PORT_CHILD_WIDEN_OK/));
    // 8. Keyless source (no Idempotency-Key): its agent ID and durability
    // state ID are UUIDv7, and the destination adopts that state ID as its
    // runtime session ID while the exported source object is still live in
    // the same workerd isolate.
    // Combined /v1/agent-runs requires a key, so a keyless client creates then admits.
    const keylessAgent = (await curl('keyless-agent-create', '/v1/agents', { method: 'POST', body: { settings }, expected: 201 })).value.agent_id;
    const keyless = { agent_id: keylessAgent, ...(await curl('keyless-turn-admit', '/v1/agents/' + keylessAgent + '/turns', { method: 'POST', body: { input: 'PORT_KEYLESS_ROOT: answer.' },
      headers: { 'Idempotency-Key': randomUUID() }, expected: 202 })).value };
    check('keyless source agent ID is a UUIDv7', () => assert.match(keyless.agent_id, /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-/));
    const keylessDone = await terminal('keyless-run-terminal', keyless.agent_id, keyless.turn_id);
    check('keyless source turn completed', () => assert.equal(keylessDone.state, 'completed', JSON.stringify(keylessDone)));
    let keylessExport;
    for (let attempt = 1; attempt <= 30; attempt++) {
      keylessExport = await curl('keyless-export-' + attempt, '/v1/agents/' + keyless.agent_id + '/durability', { method: 'POST' });
      if (keylessExport.status !== 202) break;
      await delay(500);
    }
    check('keyless export completes with a UUIDv7 state ID', () => {
      assert.equal(keylessExport.status, 200, JSON.stringify(keylessExport.value).slice(0, 2000));
      assert.match(keylessExport.value.durability.stateId, /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-/);
    });
    let keylessImport;
    for (let attempt = 1; attempt <= 20; attempt++) {
      keylessImport = await curl('keyless-import-' + attempt, '/v1/agents', { method: 'POST', body: { durability: keylessExport.value }, headers: { 'Idempotency-Key': randomUUID() }, maxTime: 60 });
      if (keylessImport.status !== 503) break;
      await delay(1000);
    }
    check('keyless import creates a destination holding the source state ID', () => {
      assert.equal(keylessImport.status, 201, JSON.stringify(keylessImport.value));
      assert.equal(keylessImport.value.durability_id, keylessExport.value.durability.stateId);
    });
    // Touch the fenced source so its object is resident while the destination runs.
    const keylessSourceFenced = await curl('keyless-source-fenced', '/v1/agents/' + keyless.agent_id + '/turns', { method: 'POST', body: { input: 'PORT_KEYLESS_ROOT: must not run.' }, headers: { 'Idempotency-Key': randomUUID() } });
    check('keyless exported source is fenced (409)', () => assert.equal(keylessSourceFenced.status, 409, JSON.stringify(keylessSourceFenced.value)));
    const keylessNext = (await curl('keyless-destination-admit', '/v1/agents/' + keylessImport.value.agent_id + '/turns', { method: 'POST', body: { input: 'PORT_KEYLESS_DEST: answer.' },
      headers: { 'Idempotency-Key': randomUUID() }, expected: 202 })).value;
    const keylessNextDone = await terminal('keyless-destination-terminal', keylessImport.value.agent_id, keylessNext.turn_id);
    summary.keyless = { source: keyless.agent_id, state_id: keylessExport.value.durability.stateId, destination: keylessImport.value.agent_id, destination_turn: keylessNextDone };
    check('keyless destination turn completes beside the live source (no session ID collision)', () => assert.equal(keylessNextDone.state, 'completed', JSON.stringify(keylessNextDone)));
    check('keyless destination provider request carries the source answer', () =>
      assert.match(JSON.stringify(modelCalls.filter(call => call.scenario === 'PORT_KEYLESS_DEST')[0]?.history ?? null), /PORT_KEYLESS_ROOT_DONE/));

    // 9. Export gate: a Codex root whose retained child is a cross-harness
    // Claude child is refused before anything is fenced or exported.
    const login = await curl('claude-login', '/v1/credentials/claude/login', { method: 'POST' });
    assert.ok(login.status >= 200 && login.status < 300 && login.value.authorization_url, 'Claude login starts: ' + login.status);
    const loginState = new URL(login.value.authorization_url).searchParams.get('state');
    const connected = await curl('claude-login-complete', '/v1/credentials/claude/login/complete', { method: 'POST', body: { code: 'curl-portability#' + loginState } });
    assert.ok(connected.status >= 200 && connected.status < 300, 'Claude login completes: ' + connected.status + ' ' + JSON.stringify(connected.value));
    const mixed = (await curl('claude-child-run-admit', '/v1/agent-runs', { method: 'POST', body: { input: 'PORT_CLAUDE_ROOT: delegate one synthetic result to a Claude child, then answer.', settings },
      headers: { 'Idempotency-Key': randomUUID() }, expected: 201 })).value;
    const mixedDone = await terminal('claude-child-run-terminal', mixed.agent_id, mixed.turn_id);
    const claudeWait = JSON.stringify(modelCalls.flatMap(call => call.history).filter(item => item.call_id === 'port-claude-wait' && /_call_output$/.test(item.type ?? '')).at(-1) ?? null);
    summary.claude_child = { agent: mixed.agent_id, terminal: mixedDone, claude_calls: claudeCalls, wait: claudeWait.slice(0, 2000) };
    check('Codex root with a Claude child completed', () => assert.equal(mixedDone.state, 'completed', JSON.stringify(mixedDone)));
    check('the Claude child ran through the Claude Messages provider and its result reached the root', () => {
      assert.ok(claudeCalls.length >= 1 && claudeCalls.every(call => call.model === claudeModel), JSON.stringify(claudeCalls));
      assert.match(claudeWait, /PORT_CLAUDE_CHILD_OK/);
    });
    const gated = await curl('claude-child-export', '/v1/agents/' + mixed.agent_id + '/durability', { method: 'POST' });
    check('export with a cross-harness child is 409 subagent_routes_not_portable with an actionable message', () => {
      assert.equal(gated.status, 409, JSON.stringify(gated.value));
      assert.equal(gated.value.error, 'subagent_routes_not_portable');
      assert.match(gated.value.message, /cross-harness subagents are not yet portable.*Close them before exporting/);
      assert.equal(gated.value.durability, undefined);
    });
    const after = (await curl('claude-child-source-turn-after-gate', '/v1/agents/' + mixed.agent_id + '/turns', { method: 'POST', body: { input: 'PORT_CLAUDE_AFTER: answer.' },
      headers: { 'Idempotency-Key': randomUUID() }, expected: 202 })).value;
    const afterDone = await terminal('claude-child-source-after-terminal', mixed.agent_id, after.turn_id);
    check('refused export fenced nothing: the source still serves a new turn', () => {
      assert.equal(afterDone.state, 'completed', JSON.stringify(afterDone));
      assert.ok(modelCalls.some(call => call.scenario === 'PORT_CLAUDE_AFTER'));
    });
    const gatedAgain = await curl('claude-child-export-again', '/v1/agents/' + mixed.agent_id + '/durability', { method: 'POST' });
    check('the gate is stable: a repeated export is refused the same way', () => { assert.equal(gatedAgain.status, 409); assert.equal(gatedAgain.value.error, 'subagent_routes_not_portable'); });
    check('no unexpected external requests', () => assert.deepEqual(unexpected, []));
  } finally {
    await writeFile(join(output, 'trace.json'), JSON.stringify({ summary, assertions, model_calls: modelCalls, unexpected, offline_mcp_discovery: discovery.length }, null, 2));
    await stop();
    for (const socket of sockets) socket.destroy();
    await new Promise(r => control.close(r));
    console.log(JSON.stringify({ evidence: output, assertions: assertions.length, model_calls: modelCalls.length }));
  }
});
