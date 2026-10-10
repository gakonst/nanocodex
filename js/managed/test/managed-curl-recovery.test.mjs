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

test('curl recovers managed work across workerd SIGKILL without duplicate effects and stops at the durable budget', { timeout: 240_000 }, async () => {
  const output = join(root, 'output/managed-curl-recovery', new Date().toISOString().replaceAll(':', '-') + '-' + randomUUID().slice(0, 8));
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
  const markers = ['CURL_DTASK_NEW', 'CURL_DTASK_ORIG', 'CURL_ROOT_DTASK', 'CURL_CODEX_QUEUED', 'CURL_CODEX_HOLD', 'CURL_CLAUDE_CHILD', 'CURL_COMMITTED_TASK', 'CURL_ROOT_COMMITTED', 'CURL_ROOT_CLAUDE_COMMITTED', 'CURL_YIELD_TASK', 'CURL_ROOT_YIELD', 'CURL_WIDE_TASK', 'CURL_ROOT_WIDE', 'CURL_FOLLOWUP_TASK', 'CURL_CHILD_FOLLOWUP', 'CURL_CHILD_TASK', 'CURL_LOOP_TASK', 'CURL_BUDGET_NEXT', 'CURL_LOOP_NEXT', 'CURL_BUDGET', 'CURL_EFFECTS', 'CURL_ROOT_SPAWN', 'CURL_ROOT_LOOP'];
  const baselines = {};
  // Delegated-task journey state (3g): owner losses before and after the
  // child's result for its newest delegation is accepted.
  const dtask = { kills: 0, submits: 0, calls: [] };
  const delegate = (task, marker) => [
    () => exec(marker + '-spawn', 'text(await tools.spawn_agent(' + JSON.stringify({ role: 'Curl child', task, model: 'sol', thinking: 'low', output_contract: { kind: 'string' } }) + '));'),
    () => exec(marker + '-wait', 'text(await tools.wait_agent({agent_ids:[1],timeout_ms:20000}));'),
  ];
  const decide = ({ history }) => {
    const user = JSON.stringify(history.filter(item => item.role === 'user'));
    const scenario = markers.find(marker => user.includes(marker));
    const outputs = history.filter(item => /_call_output$/.test(item.type ?? ''));
    const users = history.filter(item => item.role === 'user' || item.role === 'developer');
    // Concrete identity of the child's interrupted call (id + effect target),
    // not notice wording: only the runtime can supply it after restoration.
    const namesLostCall = user.includes('curl-child-command') && user.includes('effects.example/effect/C');
    const namesWideCall = user.includes('curl-wide-cell/code-10') && user.includes('effects.example/effect/E');
    const namesCommittedEffect = user.includes('curl-committed-effect') && user.includes('effects.example/effect/F');
    // Resume evidence lines name calls as "- tool (call_id ID; state): arguments".
    const lastText = [users.at(-1)?.content].flat().map(part => typeof part === 'string' ? part : part?.text ?? '').join('\n');
    const evidenceLines = lastText.split('\n').filter(line => /^- .*\(call_id [^;]+; /.test(line));
    modelCalls.push({ process: processNumber, scenario, tool_outputs: outputs.length, names_lost_call: namesLostCall, names_wide_call: namesWideCall,
      listed_wide_calls: new Set(user.match(/curl-wide-cell(?:\/code-\d+)?(?=;)/g) ?? []).size,
      names_committed_effect: namesCommittedEffect, listed_committed_calls: new Set(user.match(/curl-committed-batch(?:\/code-\d+)?(?=;)/g) ?? []).size,
      listed_effect_calls: new Set(user.match(/curl-committed-effect(?:\/code-\d+)?(?=;)/g) ?? []).size, omitted_line: user.match(/\d+ additional observed call/)?.[0] ?? null,
      evidence_lines: evidenceLines, restored_outputs: outputs.map(item => item.call_id), at: new Date().toISOString(), last_output: outputs.at(-1),
      items: history.map(item => item.type === 'message' || !item.type ? 'message:' + item.role : item.type + (item.call_id ? ':' + item.call_id : '')), instruction_messages: users.length, last_instruction: JSON.stringify(users.at(-1)?.content ?? null).slice(0, 1200) });
    // A restored child sees one more runtime instruction than its first
    // request. Like a real model, this stub only avoids repeating an effect
    // when its input concretely names the interrupted call; a generic
    // "something may have happened" notice is not enough to act on.
    const resumed = users.length > (baselines[scenario] ??= users.length);
    const since = history.findLastIndex(item => (item.role === 'user' || item.role === 'developer') && JSON.stringify(item.content).includes(scenario));
    const fresh = history.slice(since + 1).filter(item => /_call_output$/.test(item.type ?? '')).length;
    switch (scenario) {
      case 'CURL_CODEX_HOLD': return outputs.length === 0 ? exec('curl-codex-hold',
        '// @exec: {"yield_time_ms": 60000}\ntext(await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/HOLD"}));') : say('CODEX_HOLD_DONE');
      case 'CURL_CODEX_QUEUED': return say('CODEX_QUEUED_RAN');
      case 'CURL_BUDGET': return 'kill'; // the model call is in flight; no durable progress
      case 'CURL_BUDGET_NEXT': return say('BUDGET_NEXT_OK');
      case 'CURL_EFFECTS': return outputs.length === 0 ? exec('curl-effects-cell',
        'const a = await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/A"});\n'
        + 'const b = await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/B"});\ntext(JSON.stringify({a, b}));') : say('EFFECTS_DONE');
      case 'CURL_ROOT_SPAWN': return delegate('CURL_CHILD_TASK: run the synthetic effect once using exec_command, then submit_result.', 'curl-root')[outputs.length]?.() ?? say('ROOT_DONE');
      case 'CURL_CHILD_TASK': return resumed && namesLostCall ? outputs.length === 0 ? exec('curl-child-resumed', 'text(await tools.submit_result({output:"CHILD_RESUMED_WITHOUT_REPEAT"}));') : say('CHILD_RESUMED')
        : outputs.length === 0 ? exec('curl-child-command', 'text(await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/C"}));')
        : outputs.length === 1 ? exec('curl-child-submit', 'text(await tools.submit_result({output:"CHILD_OK"}));') : say('CHILD_OK');
      case 'CURL_ROOT_LOOP': return delegate('CURL_LOOP_TASK: answer once.', 'curl-loop')[outputs.length]?.() ?? say('LOOP_ROOT_DONE');
      case 'CURL_LOOP_TASK': return 'kill'; // every child inference dies with its owner
      case 'CURL_LOOP_NEXT': return say('LOOP_NEXT_OK');
      case 'CURL_CLAUDE_CHILD': return outputs.length === 0 ? exec('curl-claude-child-submit', 'text(await tools.submit_result({output:"CLAUDE_CHILD_OK"}));') : say('CLAUDE_CHILD_OK');
      // More observed calls than the retention bound, then two owner losses.
      case 'CURL_ROOT_WIDE': return delegate('CURL_WIDE_TASK: run the synthetic batch, then apply effect E once.', 'curl-wide')[outputs.length]?.() ?? say('WIDE_ROOT_DONE');
      case 'CURL_WIDE_TASK': {
        if (!resumed) return outputs.length === 0 ? exec('curl-wide-cell', 'for (let i = 1; i <= 9; i++) await tools.exec_command({cmd:"printf " + i});\n'
          + 'text(await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/E"}));') : say('WIDE_UNEXPECTED');
        const resumedCalls = modelCalls.filter(call => call.scenario === 'CURL_WIDE_TASK' && call.tool_outputs === 0).length - 1;
        if (resumedCalls === 1) return 'kill'; // second loss before the resumed child does anything
        return !namesWideCall ? exec('curl-wide-repeat', 'text(await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/E"}));')
          : outputs.length === 0 ? exec('curl-wide-resumed', 'text(await tools.submit_result({output:"WIDE_RESUMED_WITHOUT_REPEAT"}));') : say('WIDE_RESUMED');
      }
      case 'CURL_ROOT_DTASK': {
        const steps = [
          () => exec('dtask-spawn', 'text(await tools.spawn_agent(' + JSON.stringify({ role: 'Curl delegated child', task: 'CURL_DTASK_ORIG: submit ORIG_OK.', model: 'sol', thinking: 'low', output_contract: { kind: 'string' } }) + '));'),
          () => exec('dtask-wait-orig', 'text(await tools.wait_agent({agent_ids:[1],timeout_ms:20000}));'),
          () => exec('dtask-delegate', 'text(await tools.send_agent_message({agent_id:1,purpose:"delegate",message:"CURL_DTASK_NEW: submit NEW_OK."}));'),
        ];
        if (outputs.length < steps.length) return steps[outputs.length]();
        const seen = JSON.stringify(outputs.slice(steps.length));
        if (/NEW_OK|without a valid submit_result/.test(seen) || outputs.length >= steps.length + 8) return say('DTASK_ROOT_DONE');
        return exec('dtask-wait-' + outputs.length, 'text(await tools.wait_agent({agent_ids:[1],timeout_ms:20000}));');
      }
      case 'CURL_DTASK_ORIG': return outputs.length === 0 ? exec('dtask-orig-submit', 'text(await tools.submit_result({output:"ORIG_OK"}));') : say('ORIG_DONE');
      case 'CURL_DTASK_NEW': {
        const last = [users.at(-1)?.content].flat().map(part => typeof part === 'string' ? part : part?.text ?? '').join('\n');
        const submitted = outputs.some(item => item.call_id === 'dtask-new-submit');
        dtask.calls.push({ process: processNumber, resumed: last.includes('runtime restarted while your previous turn was running'),
          restated: last.match(/Delegated task:\n(CURL_DTASK_[A-Z]+)/)?.[1] ?? null, submitted });
        // Loss 1: the newest delegation has not accepted a result yet.
        if (!submitted && dtask.kills === 0) { dtask.kills++; return 'kill'; }
        if (!submitted) { dtask.submits++; return exec('dtask-new-submit', 'text(await tools.submit_result({output:"NEW_OK"}));'); }
        // Loss 2: the result was accepted; the final provider call is in flight.
        if (dtask.kills === 1) { dtask.kills++; return 'kill'; }
        // A model shown its accepted receipt just finishes (it may not resubmit).
        return say('DTASK_NEW_DONE');
      }
      // Completed rounds commit before the owner loss; only effect F is unknown.
      case 'CURL_ROOT_COMMITTED': return delegate('CURL_COMMITTED_TASK: run the synthetic batch, then apply effect F once.', 'curl-committed')[outputs.length]?.() ?? say('COMMITTED_ROOT_DONE');
      case 'CURL_COMMITTED_TASK': {
        if (!resumed) return outputs.length === 0 ? exec('curl-committed-batch', 'for (let i = 1; i <= 9; i++) await tools.exec_command({cmd:"printf " + i});\ntext("BATCH_DONE");')
          : outputs.length === 1 ? exec('curl-committed-effect', 'text(await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/F"}));') : say('COMMITTED_UNEXPECTED');
        if (outputs.at(-1)?.call_id === 'curl-committed-resumed') return say('COMMITTED_RESUMED');
        return namesCommittedEffect ? exec('curl-committed-resumed', 'text(await tools.submit_result({output:"COMMITTED_RESUMED_WITHOUT_REPEAT"}));')
          : exec('curl-committed-repeat', 'text(await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/F"}));');
      }
      case 'CURL_ROOT_CLAUDE_COMMITTED': return [
        () => exec('curl-claude-committed-spawn', 'text(await tools.spawn_agent(' + JSON.stringify({ role: 'Curl Claude child', task: 'CURL_CLAUDE_COMMITTED_TASK: run the synthetic batch, then apply effect G once.', harness: 'claude', model: claudeSettings.model, thinking: 'low', output_contract: { kind: 'string' } }) + '));'),
        () => exec('curl-claude-committed-wait', 'text(await tools.wait_agent({agent_ids:[1],timeout_ms:20000}));'),
      ][outputs.length]?.() ?? say('CLAUDE_COMMITTED_ROOT_DONE');
      // A yielded cell finished effect H1 and is still running when its owner
      // dies during H2, reached through a later wait.
      case 'CURL_ROOT_YIELD': return delegate('CURL_YIELD_TASK: run the yielded synthetic cell, then wait for it.', 'curl-yield')[outputs.length]?.() ?? say('YIELD_ROOT_DONE');
      case 'CURL_YIELD_TASK': {
        const effect = name => 'text(await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/' + name + '"}));';
        if (!resumed) {
          if (outputs.length === 0) return exec('curl-yield-cell', '// @exec: {"yield_time_ms": 1500}\n' + effect('H1')
            + '\nawait tools.exec_command({cmd:"sleep 4"});\n' + effect('H2'));
          const cellId = JSON.stringify(outputs.at(-1)).match(/Script running with cell ID ([0-9a-f-]+:[0-9]+)/)?.[1];
          if (outputs.length === 1 && cellId) return respond([{ type: 'function_call', name: 'wait', call_id: 'curl-yield-wait', arguments: JSON.stringify({ cell_id: cellId, yield_time_ms: 20000 }) }], false);
          return say('YIELD_UNEXPECTED');
        }
        const named = effectName => evidenceLines.find(line => /call_id curl-yield-cell\/code-\d+;/.test(line) && line.includes('effect/' + effectName));
        const done = outputs.map(item => item.call_id);
        if (done.includes('curl-yield-resumed')) return say('YIELD_RESUMED');
        // Like a real model, repeat an effect unless the runtime names it.
        if (!named('H1') && !done.includes('curl-yield-repeat-h1')) return exec('curl-yield-repeat-h1', effect('H1'));
        if (!named('H2') && !done.includes('curl-yield-repeat-h2')) return exec('curl-yield-repeat-h2', effect('H2'));
        return exec('curl-yield-resumed', 'text(await tools.submit_result({output:"YIELD_RESUMED_WITHOUT_REPEAT"}));');
      }
      // Explicit delegation to the same, already completed child after restart.
      case 'CURL_CHILD_FOLLOWUP': return [
        () => exec('curl-followup-send', 'text(await tools.send_agent_message({agent_id:1,purpose:"delegate",message:"CURL_FOLLOWUP_TASK: apply effect D once with exec_command, then submit_result."}));'),
        () => exec('curl-followup-wait', 'text(await tools.wait_agent({agent_ids:[1],timeout_ms:20000}));'),
      ][fresh]?.() ?? say('FOLLOWUP_ROOT_DONE');
      case 'CURL_FOLLOWUP_TASK': return [
        () => exec('curl-followup-command', 'text(await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/D"}));'),
        () => exec('curl-followup-submit', 'text(await tools.submit_result({output:"FOLLOWUP_OK"}));'),
      ][fresh]?.() ?? say('FOLLOWUP_OK');
      default: unexpected.push({ kind: 'model', user: user.slice(0, 2000) }); return say('UNEXPECTED');
    }
  };
  // Anthropic Messages stub for a Claude Sonnet root that delegates to a
  // Codex child. Like the live model, it labels values as text("label:", v)
  // and acts only on what the previous tool_result actually showed it.
  const claudeCalls = [];
  const claudeSse = blocks => [
    { type: 'message_start', message: { id: 'msg_' + randomUUID(), type: 'message', role: 'assistant', model: claudeSettings.model, content: [], usage: { input_tokens: 10, output_tokens: 0 } } },
    ...blocks.flatMap((block, index) => block.type === 'tool_use'
      ? [{ type: 'content_block_start', index, content_block: { ...block, input: {} } }, { type: 'content_block_delta', index, delta: { type: 'input_json_delta', partial_json: JSON.stringify(block.input) } }, { type: 'content_block_stop', index }]
      : [{ type: 'content_block_start', index, content_block: { type: 'text', text: '' } }, { type: 'content_block_delta', index, delta: { type: 'text_delta', text: block.text } }, { type: 'content_block_stop', index }]),
    { type: 'message_delta', delta: { stop_reason: blocks.some(block => block.type === 'tool_use') ? 'tool_use' : 'end_turn', stop_sequence: null }, usage: { output_tokens: 1 } },
    { type: 'message_stop' },
  ].map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join('');
  const claudeExec = (id, code) => claudeSse([{ type: 'tool_use', id, name: 'exec', input: { code } }]);
  // Held Claude turn: one Code Mode cell whose effect response the parent holds.
  const decideClaudeHold = body => {
    const results = body.messages.flatMap(message => Array.isArray(message.content) ? message.content : []).filter(block => block.type === 'tool_result');
    const queued = JSON.stringify(body.messages).includes('CURL_CLAUDE_QUEUED');
    claudeCalls.push({ process: processNumber, scenario: queued ? 'CURL_CLAUDE_QUEUED' : 'CURL_CLAUDE_HOLD', tool_results: results.length });
    if (queued) return claudeSse([{ type: 'text', text: 'QUEUED_TURN_RAN' }]);
    return results.length === 0
      ? claudeExec('toolu_curl_claude_hold', '// @exec: {"yield_time_ms": 60000}\ntext(await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/HOLD"}));')
      : claudeSse([{ type: 'text', text: 'HOLD_DONE' }]);
  };
  // Claude child: one finished Code Mode round, then effect G dies with its owner.
  const claudeCommittedCalls = [];
  const decideClaudeCommitted = body => {
    const results = body.messages.flatMap(message => Array.isArray(message.content) ? message.content : []).filter(block => block.type === 'tool_result');
    const ids = results.map(block => block.tool_use_id);
    // Only runtime-authored user text, never the restored assistant tool_use blocks.
    const userText = JSON.stringify(body.messages.filter(message => message.role === 'user')
      .flatMap(message => typeof message.content === 'string' ? [message.content] : message.content.filter(block => block.type === 'text').map(block => block.text)));
    const resumed = userText.includes('runtime restarted while your previous turn was running');
    const lastUser = body.messages.filter(message => message.role === 'user').at(-1);
    const lastText = [lastUser?.content].flat().map(part => typeof part === 'string' ? part : part?.type === 'text' ? part.text : '').join('\n');
    const call = { process: processNumber, tool_results: ids, resumed, restored_outputs: ids,
      evidence_lines: lastText.split('\n').filter(line => /^- .*\(call_id [^;]+; /.test(line)),
      names_effect: userText.includes('toolu_committed_effect') && userText.includes('effects.example/effect/G'),
      listed_committed_calls: new Set(userText.match(/toolu_committed_batch(?:\/code-\d+)?(?=;)/g) ?? []).size,
      listed_effect_calls: new Set(userText.match(/toolu_committed_effect(?:\/code-\d+)?(?=;)/g) ?? []).size,
      omitted_line: userText.match(/\d+ additional observed call/)?.[0] ?? null };
    claudeCommittedCalls.push(call);
    if (!resumed) return ids.length === 0 ? claudeExec('toolu_committed_batch', 'for (let i = 1; i <= 9; i++) await tools.exec_command({cmd:"printf " + i});\ntext("BATCH_DONE");')
      : ids.length === 1 ? claudeExec('toolu_committed_effect', 'text(await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/G"}));')
      : claudeSse([{ type: 'text', text: 'CLAUDE_COMMITTED_UNEXPECTED' }]);
    if (ids.includes('toolu_committed_resumed')) return claudeSse([{ type: 'text', text: 'CLAUDE_COMMITTED_RESUMED' }]);
    return call.names_effect ? claudeExec('toolu_committed_resumed', 'text(await tools.submit_result({output:"CLAUDE_COMMITTED_RESUMED_WITHOUT_REPEAT"}));')
      : claudeExec('toolu_committed_repeat', 'text(await tools.exec_command({cmd:"curl -s -X POST https://effects.example/effect/G"}));');
  };
  let releaseHold, held = new Promise(resolvePromise => { releaseHold = resolvePromise; });
  const heldEffects = [];
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
      const decision = decide(JSON.parse(call.body));
      if (decision === 'kill') return void kill('model call in flight: ' + modelCalls.at(-1).scenario);
      return send(200, decision);
    }
    if (url.origin === 'https://api.anthropic.com' && url.pathname === '/v1/models') return send(200, { data: [{ id: claudeSettings.model, display_name: 'Synthetic Claude Sonnet' }], has_more: false });
    if (url.origin === 'https://api.anthropic.com' && url.pathname === '/v1/messages') {
      const body = JSON.parse(call.body);
      if (body.stream === true && JSON.stringify(body.messages).includes('CURL_CLAUDE_ROOT')) return send(200, decideClaude(body), 'text/event-stream');
      if (body.stream === true && JSON.stringify(body.messages).includes('CURL_CLAUDE_HOLD')) return send(200, decideClaudeHold(body), 'text/event-stream');
      if (body.stream === true && JSON.stringify(body.messages).includes('CURL_CLAUDE_COMMITTED_TASK')) return send(200, decideClaudeCommitted(body), 'text/event-stream');
      unexpected.push({ kind: 'claude', model: body.model, stream: body.stream ?? null }); return send(400, { type: 'error', error: { type: 'invalid_request_error', message: 'unexpected synthetic Claude request' } });
    }
    if (/^https:\/\/(platform\.claude\.com|claude\.ai|api\.anthropic\.com)\//.test(call.url)) {
      const response = await claudeProvider(new Request(call.url, { method: call.method, headers: call.headers, body: call.body ?? undefined }));
      if (response) { res.writeHead(response.status, Object.fromEntries(response.headers)); return void res.end(Buffer.from(await response.arrayBuffer())); }
    }
    if (url.hostname === 'effects.example') {
      const name = url.pathname.split('/').pop();
      if (name === 'HOLD') {
        heldEffects.push({ process: processNumber, method: call.method, at: new Date().toISOString() });
        await held; return send(200, 'EFFECT_HOLD_APPLIED\n', 'text/plain');
      }
      effects.push({ process: processNumber, name, method: call.method, at: new Date().toISOString() });
      // B and the child's C reach the external system, then the owner dies
      // before any receipt can return: the outcome is genuinely unknown.
      if (name === 'B' || (['C', 'E', 'F', 'G', 'H2'].includes(name) && effects.filter(effect => effect.name === name).length === 1)) return void kill('effect ' + name + ' dispatched, response never returned');
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
      const decision = decide({ history: body.input ?? [] });
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
  // Recovery may replay a call (tool.call and tool.result again), but a call
  // closed as interrupted must never report again.
  const openToolCalls = page => {
    const open = new Map(), interrupted = new Map(), reopened = [];
    for (const row of page.data) {
      const key = (row.agent_id ?? 'root') + ':' + row.event?.payload?.call_id;
      if ((row.event?.type === 'tool.call' || row.event?.type === 'tool.result') && interrupted.has(key))
        reopened.push({ call: key, interrupted: interrupted.get(key), cursor: row.cursor, type: row.event.type });
      if (row.event?.type === 'tool.call') open.set(key, { cursor: row.cursor, tool: row.event.payload.tool });
      if (row.event?.type === 'tool.result') {
        open.delete(key);
        if (row.event.payload.structured_result?.code === 'TOOL_CALL_INTERRUPTED') interrupted.set(key, row.cursor);
      }
    }
    assert.equal(page.has_more, false, 'history fits one page');
    assert.deepEqual(reopened, [], 'a call closed as interrupted never reports again');
    return [...open].map(([call, value]) => ({ call, ...value }));
  };
  // Negative invariant: every call the child observed before the loss is
  // either committed to its restored request (directly, or as nested work of a
  // committed terminal cell) or named in resume evidence / its omitted count.
  const observedCalls = (rows, agentId, prefixes) => [...new Set(rows.data
    .filter(row => row.agent_id === agentId && row.event?.type === 'tool.call')
    .map(row => row.event.payload.call_id).filter(id => prefixes.some(prefix => id === prefix || id.startsWith(prefix + '/code-'))))];
  const assertCovered = (label, observed, call, terminalCells) => {
    const listed = new Set(call.evidence_lines.map(line => line.match(/call_id ([^;]+);/)[1]));
    const committed = new Set(call.restored_outputs);
    const omitted = Number(call.omitted_line?.match(/\d+/)?.[0] ?? 0);
    const missing = observed.filter(id => !listed.has(id) && !committed.has(id)
      && !(id.includes('/code-') && terminalCells.includes(id.split('/code-')[0]) && committed.has(id.split('/code-')[0])));
    assert.ok(observed.length > 0 && missing.length <= omitted,
      label + ': an observed call is neither committed nor named: ' + JSON.stringify({ observed, missing, omitted, listed: [...listed] }));
  };
  const summary = {};

  try {
    await start();
    const identity = await fetch(new URL('/__fixture/identity', base), { method: 'POST' });
    token = (await identity.json()).token; assert.ok(token, 'synthetic API key');
    assert.equal((await fetch(new URL('/__fixture/chatgpt', base), { method: 'POST' })).status, 204);
    const unauthorized = await (async () => { const saved = token; token = 'synthetic-invalid'; try { return await curl('unauthenticated-run-rejected', '/v1/agent-runs', { method: 'POST', body: { input: 'CURL_BUDGET', settings }, headers: { 'Idempotency-Key': randomUUID() } }); } finally { token = saved; } })();
    assert.equal(unauthorized.status, 401); assert.equal(modelCalls.length, 0, 'rejected admission never reaches the provider');

    // 0. Mixed Claude -> Codex delegation (live final-mixed regression): the
    // Claude root's labeled Code Mode text must carry the actual spawn receipt
    // into its next provider request, so it waits on that child instead of
    // probing and spawning replacements.
    const login = await curl('claude-login', '/v1/credentials/claude/login', { method: 'POST' });
    assert.ok(login.status >= 200 && login.status < 300 && login.value.authorization_url, 'Claude login starts: ' + login.status);
    const loginState = new URL(login.value.authorization_url).searchParams.get('state');
    const connected = await curl('claude-login-complete', '/v1/credentials/claude/login/complete', { method: 'POST', body: { code: 'curl-recovery#' + loginState } });
    assert.ok(connected.status >= 200 && connected.status < 300, 'Claude login completes: ' + connected.status + ' ' + JSON.stringify(connected.value));
    const claudeRun = (await curl('claude-mixed-admit', '/v1/agent-runs', { method: 'POST', body: { input: 'CURL_CLAUDE_ROOT: delegate one synthetic result to a Codex child.', settings: claudeSettings }, headers: { 'Idempotency-Key': randomUUID() }, expected: 201 })).value;
    const claudeDone = await terminal('claude-mixed-terminal', claudeRun.agent_id, claudeRun.turn_id);
    const claudeRows = (await history('claude-mixed-history', claudeRun.agent_id)).data;
    const rootSpawns = claudeRows.filter(row => row.agent_id === undefined && row.event?.type === 'tool.result' && row.event.payload.tool === 'spawn_agent');
    summary.claude = { terminal: claudeDone, provider_calls: claudeCalls, root_spawn_results: rootSpawns.map(row => row.event.payload),
      child_calls: modelCalls.filter(call => call.scenario === 'CURL_CLAUDE_CHILD').length };
    assert.equal(claudeDone.state, 'completed', JSON.stringify(claudeDone));
    assert.equal(rootSpawns.length, 1, 'the Claude root makes exactly one actual spawn_agent call');
    const spawned = rootSpawns[0].event.payload.structured_result?.agent_id;
    assert.ok(Number.isInteger(spawned), 'the public history records the admitted child id');
    assert.equal(claudeCalls[1]?.receipt_agent_id, spawned, 'the next Claude provider request shows the labeled spawn receipt: ' + claudeCalls[1]?.shown);
    assert.match(claudeCalls[2]?.shown ?? '', /CURL_CLAUDE_WAIT: .*CLAUDE_CHILD_OK/, 'the labeled wait result shows the Codex child output');
    assert.equal(claudeCalls.length, 3, 'one provider request per observed step');
    assert.match(JSON.stringify(claudeRows), /CLAUDE_MIXED_DONE/);

    // 0b. Follow-ups queued behind a still-running turn (production 01a120ef).
    // Claude rejects their attempt as "blocked by unfinished operation" and the
    // Session parks them on retry backoff. A cancelled follow-up must settle
    // while the earlier turn still runs, without reaching the provider; an
    // uncancelled one must start as soon as the earlier turn settles rather
    // than after its remaining backoff. Codex queues follow-ups in its driver.
    const providerCalls = scenario => [...claudeCalls, ...modelCalls].filter(call => call.scenario === scenario).length;
    const queuedBehindHold = async (label, harnessSettings, hold, queued, cancel) => {
      held = new Promise(resolvePromise => { releaseHold = resolvePromise; });
      const heldBefore = heldEffects.length, queuedBefore = providerCalls(queued);
      const holdRun = (await curl(label + '-hold-admit', '/v1/agent-runs', { method: 'POST', body: { input: hold + ': run the held synthetic effect once.', settings: harnessSettings }, headers: { 'Idempotency-Key': randomUUID() }, expected: 201 })).value;
      await waitFor(label + ' held effect dispatched', () => heldEffects.length === heldBefore + 1);
      const queuedTurn = (await curl(label + '-queued-admit', '/v1/agents/' + holdRun.agent_id + '/turns', { method: 'POST', body: { input: queued + ': follow-up behind the held turn.' }, headers: { 'Idempotency-Key': randomUUID() }, expected: 202 })).value;
      const queuedState = () => turnState(label + '-queued-state', holdRun.agent_id, queuedTurn.turn_id);
      let parked;
      if (harnessSettings === claudeSettings) {
        parked = await waitFor(label + ' queued turn parked', async () => {
          const value = await queuedState();
          return value.state === 'accepted' && /blocked by unfinished operation/.test(value.error ?? '')
            && (cancel || value.retry_at - Date.now() >= 6_000) ? value : undefined;
        }, 25_000, 200);
      } else {
        await delay(1_500);
        parked = await queuedState();
      }
      let cancelled, cancelMs = null;
      if (cancel) {
        const requested = Date.now();
        await curl(label + '-queued-cancel', '/v1/agents/' + holdRun.agent_id + '/turns/' + queuedTurn.turn_id + '/cancel', { method: 'POST', headers: { 'Idempotency-Key': randomUUID() } });
        cancelled = await terminal(label + '-queued-cancel-terminal', holdRun.agent_id, queuedTurn.turn_id);
        cancelMs = Date.now() - requested;
      }
      const holdDuring = await turnState(label + '-hold-during', holdRun.agent_id, holdRun.turn_id);
      releaseHold();
      const holdDone = await terminal(label + '-hold-terminal', holdRun.agent_id, holdRun.turn_id);
      const holdSettledAt = Date.now();
      const queuedDone = cancel ? cancelled : await terminal(label + '-queued-terminal', holdRun.agent_id, queuedTurn.turn_id);
      const result = { parked, cancel_ms: cancelMs, hold_during: holdDuring.state, hold: holdDone.state, queued: queuedDone,
        queued_after_hold_ms: cancel ? null : Date.now() - holdSettledAt,
        backoff_left_at_hold_ms: parked.retry_at ? parked.retry_at - holdSettledAt : null,
        held_effects: heldEffects.length - heldBefore, queued_provider_calls: providerCalls(queued) - queuedBefore };
      summary[label] = result;
      assert.equal(holdDone.state, 'completed', JSON.stringify(holdDone));
      assert.equal(result.held_effects, 1, 'the held effect ran once');
      if (cancel) {
        assert.equal(queuedDone.state, 'cancelled', JSON.stringify(queuedDone));
        assert.ok(!['completed', 'failed', 'cancelled'].includes(holdDuring.state), 'the earlier turn was still running: ' + JSON.stringify(holdDuring));
        assert.equal(result.queued_provider_calls, 0, 'the cancelled follow-up never reached the provider');
      } else {
        assert.equal(queuedDone.state, 'completed', JSON.stringify(queuedDone));
        assert.equal(result.queued_provider_calls, 1);
        assert.ok(result.queued_after_hold_ms < 2_500, 'the follow-up starts once the earlier turn settles: ' + JSON.stringify(result));
      }
    };
    await queuedBehindHold('claude-queued-cancel', claudeSettings, 'CURL_CLAUDE_HOLD', 'CURL_CLAUDE_QUEUED', true);
    await queuedBehindHold('claude-queued-wake', claudeSettings, 'CURL_CLAUDE_HOLD', 'CURL_CLAUDE_QUEUED', false);
    assert.ok(summary['claude-queued-wake'].backoff_left_at_hold_ms > 2_500, 'backoff outlasted the earlier turn: ' + JSON.stringify(summary['claude-queued-wake']));
    await queuedBehindHold('codex-queued-cancel', settings, 'CURL_CODEX_HOLD', 'CURL_CODEX_QUEUED', true);
    await queuedBehindHold('codex-queued-wake', settings, 'CURL_CODEX_HOLD', 'CURL_CODEX_QUEUED', false);

    // A turn cancelled while its Code Mode cell waits on a long effect
    // settles promptly (the 16-minute report); the effect outcome stays unknown.
    const activeCancel = async (label, harnessSettings, hold) => {
      held = new Promise(resolvePromise => { releaseHold = resolvePromise; });
      const heldBefore = heldEffects.length;
      const run = (await curl(label + '-admit', '/v1/agent-runs', { method: 'POST', body: { input: hold + ': run the held synthetic effect once.', settings: harnessSettings }, headers: { 'Idempotency-Key': randomUUID() }, expected: 201 })).value;
      await waitFor(label + ' held effect dispatched', () => heldEffects.length === heldBefore + 1);
      const requested = Date.now();
      await curl(label + '-cancel', '/v1/agents/' + run.agent_id + '/turns/' + run.turn_id + '/cancel', { method: 'POST', headers: { 'Idempotency-Key': randomUUID() } });
      const done = await terminal(label + '-terminal', run.agent_id, run.turn_id);
      const cancelMs = Date.now() - requested;
      releaseHold();
      summary[label] = { terminal: done, cancel_ms: cancelMs };
      assert.equal(done.state, 'cancelled', JSON.stringify(done));
      assert.ok(cancelMs < 5_000, label + ' cancellation settles while the effect is still held: ' + cancelMs);
    };
    await activeCancel('claude-active-cancel', claudeSettings, 'CURL_CLAUDE_HOLD');
    await activeCancel('codex-active-cancel', settings, 'CURL_CODEX_HOLD');

    // 1. Repeated abrupt loss of the same unfinished model call consumes the
    // persisted budget: three provider invocations, then a durable terminal.
    const budgetKey = randomUUID(), budgetBody = { input: 'CURL_BUDGET: synthetic recovery budget probe.', settings };
    const admitted = (await curl('budget-admit', '/v1/agent-runs', { method: 'POST', body: budgetBody, headers: { 'Idempotency-Key': budgetKey }, expected: 201 })).value;
    const budget = { agent: admitted.agent_id, turn: admitted.turn_id };
    for (let attempt = 1; attempt <= 3; attempt++) {
      await waitFor(`budget kill ${attempt}`, () => kills.length === attempt && !fixture);
      assert.equal(modelCalls.filter(call => call.scenario === 'CURL_BUDGET').length, attempt);
      await start();
      // Any public request reconstructs the Session; its constructor resumes
      // retained work (production would also fire the persisted alarm).
      const observed = await turnState(`budget-after-loss-${attempt}`, budget.agent, budget.turn);
      assert.ok(!['completed', 'cancelled'].includes(observed.state), JSON.stringify(observed));
    }
    const stopped = await terminal('budget-terminal', budget.agent, budget.turn);
    assert.equal(stopped.state, 'failed', JSON.stringify(stopped)); assert.match(JSON.stringify(stopped), exhausted);
    assert.equal(modelCalls.filter(call => call.scenario === 'CURL_BUDGET').length, 3, 'exactly three automatic attempts reach the provider');
    const replay = await curl('budget-idempotent-replay', '/v1/agent-runs', { method: 'POST', body: budgetBody, headers: { 'Idempotency-Key': budgetKey } });
    assert.equal(replay.value.turn_id, budget.turn, JSON.stringify(replay.value));
    assert.equal(modelCalls.filter(call => call.scenario === 'CURL_BUDGET').length, 3, 'replaying the original request never re-dispatches');
    const next = (await curl('budget-next-turn', `/v1/agents/${budget.agent}/turns`, { method: 'POST', body: { input: 'CURL_BUDGET_NEXT: the session still serves new work.' }, headers: { 'Idempotency-Key': randomUUID() }, expected: 202 })).value;
    const nextDone = await terminal('budget-next-terminal', budget.agent, next.turn_id);
    assert.equal(nextDone.state, 'completed', JSON.stringify(nextDone));
    summary.budget = { stopped, provider_attempts: 3, next: nextDone.state };

    // 2. A Code Mode cell completes effect A (durable receipt), dispatches B,
    // then the owner dies. Recovery replays A's receipt and reports B unknown.
    const effectsRun = (await curl('effects-admit', '/v1/agent-runs', { method: 'POST', body: { input: 'CURL_EFFECTS: apply A then B exactly once.', settings }, headers: { 'Idempotency-Key': randomUUID() }, expected: 201 })).value;
    await waitFor('effect B owner loss', () => kills.length === 4 && !fixture);
    assert.deepEqual(effects.map(effect => effect.name), ['A', 'B']);
    await start();
    // Reconnect like a client whose stream died with the server.
    const reconnect = await curl('effects-reconnect-sse', `/v1/agents/${effectsRun.agent_id}/events?cursor=${BigInt(effectsRun.accepted_cursor) - 1n}`, { headers: { Accept: 'text/event-stream' }, maxTime: 30,
      sse: value => ['turn_completed', 'turn_failed', 'turn_cancelled'].includes(value.type) && value.turn_id === effectsRun.turn_id });
    assert.equal(reconnect.status, 200);
    const effectsDone = await terminal('effects-terminal', effectsRun.agent_id, effectsRun.turn_id);
    const effectsHistory = await history('effects-history', effectsRun.agent_id);
    assert.deepEqual(effects.map(effect => effect.name), ['A', 'B'], 'no effect is dispatched twice');
    summary.effects = { terminal: effectsDone.state, model_calls: modelCalls.filter(call => call.scenario === 'CURL_EFFECTS') };
    assert.equal(effectsDone.state, 'completed', JSON.stringify(effectsDone));
    assert.match(JSON.stringify(effectsHistory), /outcome unknown/, 'the recovered cell reports the unproved effect as unknown');
    assert.match(JSON.stringify(effectsHistory), /EFFECT_A_APPLIED/, 'the recovered cell replays the durable A receipt');
    assert.deepEqual(openToolCalls(effectsHistory), [], 'every root call interrupted by owner loss has a terminal result');

    // 3. A nested child is active (its effect C is in flight) when the owner
    // dies. Recovery must reach a bounded terminal without re-running C.
    const childRun = (await curl('child-admit', '/v1/agent-runs', { method: 'POST', body: { input: 'CURL_ROOT_SPAWN: delegate one synthetic effect to a child.', settings }, headers: { 'Idempotency-Key': randomUUID() }, expected: 201 })).value;
    await waitFor('child effect C owner loss', () => kills.length === 5 && !fixture);
    await start();
    const childDone = await terminal('child-terminal', childRun.agent_id, childRun.turn_id);
    const childHistory = await history('child-history', childRun.agent_id);
    const childCalls = modelCalls.filter(call => call.scenario === 'CURL_CHILD_TASK');
    summary.child = { terminal: childDone, model_calls: modelCalls.filter(call => ['CURL_ROOT_SPAWN', 'CURL_CHILD_TASK'].includes(call.scenario)) };
    assert.equal(effects.filter(effect => effect.name === 'C').length, 1, 'the restored child does not repeat its interrupted effect C');
    assert.equal(childCalls[0].names_lost_call, false, 'before loss the child input has no interrupted-call evidence');
    assert.ok(childCalls.some(call => call.process === 6 && call.instruction_messages > childCalls[0].instruction_messages && call.names_lost_call),
      'the restored child input names its interrupted call (call id and arguments) as outcome unknown');
    assert.equal(childDone.state, 'completed', JSON.stringify(childDone));
    assert.match(JSON.stringify(childHistory), /CHILD_RESUMED_WITHOUT_REPEAT/, 'the root observes the restored child result');
    assert.deepEqual(openToolCalls(childHistory), [], 'the child call lost with its owner has a terminal result, not a running call');

    // 3b. Restart the idle process, then explicitly delegate new work to the
    // same restored child (mirrors the live 'no Nanocodex host is active' report).
    // Same workerd process: the owner-only public drill ctx.abort()s the
    // Session isolate (as eviction/deploy would) while module globals survive.
    const beforeRestart = await curl('before-public-restart-state', `/v1/agents/${childRun.agent_id}`, { expected: 200 });
    assert.equal(beforeRestart.value.agent_loaded, true, 'the completed root runtime is loaded before the drill');
    const restarted = await curl('child-public-restart', `/v1/agents/${childRun.agent_id}/restart`, { method: 'POST', expected: 202 });
    assert.equal(restarted.value.restarting, true);
    // The acknowledged drill actually discards the loaded runtime.
    await waitFor('restart unloads the runtime', async () =>
      (await curl('after-public-restart-state', `/v1/agents/${childRun.agent_id}`, { expected: 200 })).value.agent_loaded === false);
    // The internal commit phase is not a public route.
    const commitProbe = await curl('restart-commit-not-public', `/v1/agents/${childRun.agent_id}/restart/commit`, { method: 'POST' });
    assert.ok(commitProbe.status >= 400 && commitProbe.status < 500, `public restart commit is rejected: ${commitProbe.status}`);
    const followup = (await curl('followup-turn', `/v1/agents/${childRun.agent_id}/turns`, { method: 'POST', body: { input: 'CURL_CHILD_FOLLOWUP: delegate effect D to the same child.' }, headers: { 'Idempotency-Key': randomUUID() }, expected: 202 })).value;
    const followupDone = await terminal('followup-terminal', childRun.agent_id, followup.turn_id);
    const followupHistory = JSON.stringify(await history('followup-history', childRun.agent_id));
    summary.followup = { terminal: followupDone, effects_d: effects.filter(effect => effect.name === 'D').length, host_inactive: /no Nanocodex host is active/.test(followupHistory),
      model_calls: modelCalls.filter(call => ['CURL_CHILD_FOLLOWUP', 'CURL_FOLLOWUP_TASK'].includes(call.scenario)) };
    assert.doesNotMatch(followupHistory, /no Nanocodex host is active/, 'restored child must have a live host for explicit delegation');
    assert.equal(followupDone.state, 'completed', JSON.stringify(followupDone));
    assert.equal(effects.filter(effect => effect.name === 'D').length, 1, 'delegated child effect runs exactly once');
    assert.match(followupHistory, /FOLLOWUP_OK/);
    const delegated = modelCalls.find(call => call.scenario === 'CURL_FOLLOWUP_TASK');
    assert.doesNotMatch(delegated.last_instruction, /curl-child-command/, 'an explicit new delegation does not carry stale interrupted-call evidence');

    // 3c. A child observes more calls than the retained bound, its effect E
    // is in flight, and its owner is lost twice (once during its resume).
    const wideRun = (await curl('wide-admit', '/v1/agent-runs', { method: 'POST', body: { input: 'CURL_ROOT_WIDE: delegate a wide batch to a child.', settings }, headers: { 'Idempotency-Key': randomUUID() }, expected: 201 })).value;
    const wideBase = kills.length;
    await waitFor('wide effect E owner loss', () => kills.length === wideBase + 1 && !fixture);
    await start(); await turnState('wide-after-loss-1', wideRun.agent_id, wideRun.turn_id);
    await waitFor('wide resumed child owner loss', () => kills.length === wideBase + 2 && !fixture);
    await start(); await turnState('wide-after-loss-2', wideRun.agent_id, wideRun.turn_id);
    const wideDone = await terminal('wide-terminal', wideRun.agent_id, wideRun.turn_id);
    const wideCalls = modelCalls.filter(call => call.scenario === 'CURL_WIDE_TASK');
    summary.wide = { terminal: wideDone.state, effects_e: effects.filter(effect => effect.name === 'E').length,
      child_calls: wideCalls.map(({ process, tool_outputs, names_wide_call, listed_wide_calls, instruction_messages }) => ({ process, tool_outputs, names_wide_call, listed_wide_calls, instruction_messages })) };
    const resumedWide = wideCalls.filter(call => call.tool_outputs === 0).slice(1);
    assert.equal(resumedWide.length, 2, 'the child resumed after each owner loss');
    for (const call of resumedWide) {
      assert.ok(call.names_wide_call, 'every resume names the still-unknown effect call, including after a second loss');
      assert.equal(call.listed_wide_calls, 8, 'evidence stays bounded to eight calls');
    }
    const wideHistory = await history('wide-history', wideRun.agent_id);
    assert.match(JSON.stringify(wideHistory), /\b3 additional observed call/, 'evicted completed calls are counted, never silently dropped');
    assert.deepEqual(openToolCalls(wideHistory), [], 'child calls lost across two owner losses each have a terminal result');
    assert.equal(effects.filter(effect => effect.name === 'E').length, 1, 'effect E is never dispatched again');
    assert.equal(wideDone.state, 'completed', JSON.stringify(wideDone));

    // 3d. A child finishes a ten-call round, then its owner dies during effect F.
    // The restored conversation holds the finished round's receipt, so resume
    // evidence names only the call it cannot show: none of the committed calls.
    const committedRun = (await curl('committed-admit', '/v1/agent-runs', { method: 'POST', body: { input: 'CURL_ROOT_COMMITTED: delegate a committed batch to a child.', settings }, headers: { 'Idempotency-Key': randomUUID() }, expected: 201 })).value;
    const committedBase = kills.length;
    await waitFor('committed effect F owner loss', () => kills.length === committedBase + 1 && !fixture);
    await start(); await turnState('committed-after-loss', committedRun.agent_id, committedRun.turn_id);
    const committedDone = await terminal('committed-terminal', committedRun.agent_id, committedRun.turn_id);
    const committedCalls = modelCalls.filter(call => call.scenario === 'CURL_COMMITTED_TASK');
    const committedResume = committedCalls.find(call => call.process > committedCalls[0].process);
    summary.committed = { terminal: committedDone.state, effects_f: effects.filter(effect => effect.name === 'F').length,
      child_calls: committedCalls.map(({ process, tool_outputs, names_committed_effect, listed_committed_calls, listed_effect_calls, omitted_line, items }) =>
        ({ process, tool_outputs, names_committed_effect, listed_committed_calls, listed_effect_calls, omitted_line, items })) };
    assert.ok(committedResume, 'the child resumed after the owner loss');
    assert.ok(committedResume.items.includes('custom_tool_call_output:curl-committed-batch'), 'the finished round is in the restored conversation: ' + JSON.stringify(committedResume.items));
    assert.ok(committedResume.names_committed_effect, 'the resume names the still-unknown effect call');
    assert.equal(committedResume.listed_committed_calls, 0, 'calls already committed to the restored conversation are not reported as unknown');
    assert.equal(committedResume.omitted_line, null, 'no committed call is counted as omitted unknown evidence');
    assert.equal(committedResume.listed_effect_calls, 2, 'the interrupted cell and its nested effect call are both named');
    const committedHistory = await history('committed-history', committedRun.agent_id);
    assertCovered('codex committed', observedCalls(committedHistory, 1, ['curl-committed-batch', 'curl-committed-effect']), committedResume, ['curl-committed-batch']);
    assert.deepEqual(openToolCalls(committedHistory), [], 'the lost effect call has a terminal result');
    assert.equal(effects.filter(effect => effect.name === 'F').length, 1, 'effect F is never dispatched again');
    assert.equal(committedDone.state, 'completed', JSON.stringify(committedDone));

    // 3e. The same journey for a Claude child, whose checkpoints follow its
    // provider-call completions and tool rounds instead of call starts.
    const claudeCommittedRun = (await curl('claude-committed-admit', '/v1/agent-runs', { method: 'POST', body: { input: 'CURL_ROOT_CLAUDE_COMMITTED: delegate a committed batch to a Claude child.', settings }, headers: { 'Idempotency-Key': randomUUID() }, expected: 201 })).value;
    const claudeCommittedBase = kills.length;
    await waitFor('Claude committed effect G owner loss', () => kills.length === claudeCommittedBase + 1 && !fixture);
    await start(); await turnState('claude-committed-after-loss', claudeCommittedRun.agent_id, claudeCommittedRun.turn_id);
    const claudeCommittedDone = await terminal('claude-committed-terminal', claudeCommittedRun.agent_id, claudeCommittedRun.turn_id);
    const claudeResume = claudeCommittedCalls.find(call => call.resumed);
    summary.claude_committed = { terminal: claudeCommittedDone.state, effects_g: effects.filter(effect => effect.name === 'G').length, child_calls: claudeCommittedCalls };
    assert.ok(claudeResume, 'the Claude child resumed after the owner loss: ' + JSON.stringify(claudeCommittedCalls));
    assert.ok(claudeResume.tool_results.includes('toolu_committed_batch'), 'the finished round is in the restored Claude conversation');
    assert.ok(claudeResume.names_effect, 'the resume names the still-unknown effect call');
    assert.equal(claudeResume.listed_committed_calls, 0, 'calls already committed to the restored Claude conversation are not reported as unknown');
    assert.equal(claudeResume.omitted_line, null, 'no committed Claude call is counted as omitted unknown evidence');
    assert.equal(claudeResume.listed_effect_calls, 2, 'the interrupted Claude cell and its nested effect call are both named');
    const claudeCommittedHistory = await history('claude-committed-history', claudeCommittedRun.agent_id);
    assertCovered('claude committed', observedCalls(claudeCommittedHistory, 1, ['toolu_committed_batch', 'toolu_committed_effect']), claudeResume, ['toolu_committed_batch']);
    assert.deepEqual(openToolCalls(claudeCommittedHistory), [], 'the lost Claude effect call has a terminal result');
    assert.equal(effects.filter(effect => effect.name === 'G').length, 1, 'effect G is never dispatched again');
    assert.equal(claudeCommittedDone.state, 'completed', JSON.stringify(claudeCommittedDone));

    // 3f. A yielded cell's nested results never enter the conversation. Its
    // owner dies mid-cell, so finished H1 stays named as observed, unknown H2
    // as unknown, and the cell as still running; nothing is repeated.
    const yieldRun = (await curl('yield-admit', '/v1/agent-runs', { method: 'POST', body: { input: 'CURL_ROOT_YIELD: delegate a yielded cell to a child.', settings }, headers: { 'Idempotency-Key': randomUUID() }, expected: 201 })).value;
    const yieldBase = kills.length;
    await waitFor('yield effect H2 owner loss', () => kills.length === yieldBase + 1 && !fixture);
    await start(); await turnState('yield-after-loss', yieldRun.agent_id, yieldRun.turn_id);
    const yieldDone = await terminal('yield-terminal', yieldRun.agent_id, yieldRun.turn_id);
    const yieldCalls = modelCalls.filter(call => call.scenario === 'CURL_YIELD_TASK');
    const yieldResume = yieldCalls.find(call => call.process > yieldCalls[0].process);
    summary.yield = { terminal: yieldDone.state, effects: effects.filter(effect => effect.name.startsWith('H')).map(effect => effect.name),
      child_calls: yieldCalls.map(({ process, tool_outputs, evidence_lines, restored_outputs, omitted_line }) => ({ process, tool_outputs, evidence_lines, restored_outputs, omitted_line })) };
    assert.ok(yieldResume, 'the child resumed after the owner loss');
    assert.ok(yieldResume.items.includes('custom_tool_call_output:curl-yield-cell'), 'the yield output is in the restored conversation');
    const yieldLine = pattern => yieldResume.evidence_lines.find(line => pattern.test(line));
    assert.match(yieldLine(/call_id curl-yield-cell\/code-\d+;.*effect\/H1/) ?? '', /a result was observed/, 'finished nested H1 stays named as observed: ' + JSON.stringify(yieldResume.evidence_lines));
    assert.match(yieldLine(/call_id curl-yield-cell\/code-\d+;.*effect\/H2/) ?? '', /no result was observed/, 'H2 stays named as unknown');
    assert.match(yieldLine(/call_id curl-yield-cell;/) ?? '', /yielded; still running/, 'the yielded cell stays listed as still running');
    const yieldHistory = await history('yield-history', yieldRun.agent_id);
    assertCovered('codex yield', observedCalls(yieldHistory, 1, ['curl-yield-cell', 'curl-yield-wait']), yieldResume, []);
    assert.deepEqual(effects.filter(effect => effect.name.startsWith('H')).map(effect => effect.name), ['H1', 'H2'], 'H1 and H2 each reach the external system exactly once');
    assert.deepEqual(openToolCalls(yieldHistory), [], 'every child call has a terminal result');
    assert.equal(yieldDone.state, 'completed', JSON.stringify(yieldDone));

    // 3g. Restart recovery follows the child's newest delegation. Losing the
    // owner before acceptance resumes that delegated task; losing it after
    // acceptance completes the turn with the accepted result, with no
    // provider rerun and one completion visible to the waiting parent.
    const dtaskRun = (await curl('dtask-admit', '/v1/agent-runs', { method: 'POST', body: { input: 'CURL_ROOT_DTASK: spawn a child, then delegate a new task to it.', settings }, headers: { 'Idempotency-Key': randomUUID() }, expected: 201 })).value;
    const dtaskBase = kills.length;
    await waitFor('dtask owner loss before acceptance', () => kills.length === dtaskBase + 1 && !fixture);
    await start(); await turnState('dtask-after-loss-1', dtaskRun.agent_id, dtaskRun.turn_id);
    await waitFor('dtask owner loss after acceptance', () => kills.length === dtaskBase + 2 && !fixture);
    const acceptedLoss = kills.at(-1).process;
    await start(); await turnState('dtask-after-loss-2', dtaskRun.agent_id, dtaskRun.turn_id);
    const dtaskDone = await terminal('dtask-terminal', dtaskRun.agent_id, dtaskRun.turn_id);
    const dtaskHistory = await history('dtask-history', dtaskRun.agent_id);
    const rootCalls = modelCalls.filter(call => call.scenario === 'CURL_ROOT_DTASK');
    const rootSeen = JSON.stringify(rootCalls.at(-1)?.last_output ?? null);
    const wakeTurns = new Set(modelCalls.filter(call => /subagent_completion agent_id/.test(call.last_instruction)).map(call => call.process)).size;
    summary.dtask = { terminal: dtaskDone.state, kills: dtask.kills, submits: dtask.submits, child_calls: dtask.calls, accepted_loss_process: acceptedLoss,
      root_last_output: rootSeen.slice(0, 600), wake_turns: wakeTurns };
    const firstResume = dtask.calls.find(call => call.resumed);
    assert.equal(dtask.kills, 2, 'both owner losses happened');
    assert.ok(firstResume && !firstResume.submitted, 'the unaccepted delegated turn resumed: ' + JSON.stringify(dtask.calls));
    assert.equal(firstResume.restated, 'CURL_DTASK_NEW', 'the resume restates the newest delegation, not the original task');
    assert.deepEqual(dtask.calls.filter(call => call.process > acceptedLoss), [], 'an accepted result is never rerun by the provider after a restart');
    assert.equal(dtask.submits, 1, 'the delegated result is accepted exactly once');
    assert.match(rootSeen, /NEW_OK/, 'the waiting parent receives the accepted result');
    assert.doesNotMatch(rootSeen, /without a valid submit_result/, 'the accepted result is not replaced by a missing-result failure');
    assert.equal(wakeTurns, 0, 'an active waiting parent gets no duplicate idle continuation');
    assert.deepEqual(openToolCalls(dtaskHistory), [], 'every call has a terminal result');
    assert.equal(dtaskDone.state, 'completed', JSON.stringify(dtaskDone));

    // 4. A child whose every inference dies with its owner exhausts bounded
    // automatic recovery; the root reaches a terminal and the agent stays usable.
    const loopRun = (await curl('loop-admit', '/v1/agent-runs', { method: 'POST', body: { input: 'CURL_ROOT_LOOP: delegate to a child that keeps losing its owner.', settings }, headers: { 'Idempotency-Key': randomUUID() }, expected: 201 })).value;
    const loopBase = kills.length;
    for (let loss = 1; ; loss++) {
      const outcome = await waitFor(`loop owner loss ${loss} or terminal`, async () => {
        if (kills.length >= loopBase + loss) return 'killed';
        const value = fixture && (await curl(`loop-poll-${loss}`, `/v1/agents/${loopRun.agent_id}/turns/${loopRun.turn_id}`)).value;
        return ['completed', 'failed', 'cancelled'].includes(value?.state) ? 'terminal' : undefined;
      }, 30_000, 200);
      if (outcome === 'terminal') break;
      assert.ok(loss <= 6, 'automatic recovery must be bounded');
      await start();
    }
    const loopDone = await terminal('loop-terminal', loopRun.agent_id, loopRun.turn_id);
    const loopHistory = await history('loop-history', loopRun.agent_id);
    const loopCalls = modelCalls.filter(call => call.scenario === 'CURL_LOOP_TASK');
    summary.loop = { terminal: loopDone, child_provider_attempts: loopCalls.length, root_calls: modelCalls.filter(call => call.scenario === 'CURL_ROOT_LOOP') };
    assert.ok(loopCalls.length <= 4, 'one initial child inference plus at most three automatic resumes: ' + loopCalls.length);
    assert.ok(['completed', 'failed'].includes(loopDone.state), JSON.stringify(loopDone));
    assert.match(JSON.stringify([loopDone, loopHistory]), /recovery exhausted|outcome unknown/);
    const loopNext = (await curl('loop-next-turn', `/v1/agents/${loopRun.agent_id}/turns`, { method: 'POST', body: { input: 'CURL_LOOP_NEXT: still usable after exhaustion.' }, headers: { 'Idempotency-Key': randomUUID() }, expected: 202 })).value;
    assert.equal((await terminal('loop-next-terminal', loopRun.agent_id, loopNext.turn_id)).state, 'completed');
    assert.deepEqual(unexpected, []);
  } finally {
    await writeFile(join(output, 'trace.json'), JSON.stringify({ summary, kills, model_calls: modelCalls, effects, unexpected, offline_mcp_discovery: discovery.length }, null, 2));
    await kill('test cleanup');
    for (const socket of sockets) socket.destroy();
    await new Promise(r => control.close(r));
    console.log(JSON.stringify({ evidence: output, kills: kills.length, effects: effects.map(effect => effect.name), model_calls: modelCalls.length }));
  }
});
