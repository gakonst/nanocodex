// Measures storage writes per managed Claude turn (rows written, transactions,
// write epochs ~ output-gate batches, bytes by table). Production Worker, DO and
// Rust/WASM runtime; only provider HTTP is a fixture.
// Reproduce: node --test test/turn-write-amplification-journey.test.mjs
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdir, writeFile, rm, readdir, stat } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { resolve, join } from 'node:path';
import { build } from 'esbuild';
import { builtinModules } from 'node:module';
import { Miniflare } from 'miniflare';
import { claudeProvider } from '../../egress/test/claude-provider.fixture.mjs';
const repo = fileURLToPath(new URL('../../../', import.meta.url));
const evidence = resolve(repo, process.env.NANOCODEX_WA_EVIDENCE_DIR ?? 'output/write-amplification');
const identity = '11111111-1111-4111-8111-111111111144';
const bootstrap = `
import { DurableObject } from 'cloudflare:workers';
export class FixtureSandbox extends DurableObject { async clearRemoteDesktop() {} async destroy() {} }
import managed from './src/index.ts';
import { DurableAgentSession } from './src/index.ts';
export * from './src/index.ts';
import { ensureAccount, createApiKey } from './src/account-auth.ts';
const metrics = new Map();
const WRITE = /^\\s*(?:WITH[\\s\\S]*?\\)\\s*)?(INSERT(?:\\s+OR\\s+\\w+)?\\s+INTO|UPDATE|DELETE\\s+FROM|REPLACE\\s+INTO)\\s+([A-Za-z_][A-Za-z0-9_]*)/i;
function instrument(ctx) {
  const m = { exec:0, writeStatements:0, transactions:0, epochs:0, cursors:[] };
  metrics.set(ctx.id.toString(), m);
  const sql = ctx.storage.sql; const exec = sql.exec.bind(sql);
  let open = false;
  const epoch = () => { if (open) return; open = true; m.epochs++; setTimeout(() => { open = false; }, 0); };
  Object.defineProperty(sql, 'exec', { configurable:true, value:(query, ...args) => {
    const cursor = exec(query, ...args); m.exec++;
    const match = WRITE.exec(query);
    if (match) { m.writeStatements++; m.cursors.push([match[2], cursor]); epoch(); }
    return cursor;
  } });
  const tx = ctx.storage.transactionSync.bind(ctx.storage);
  Object.defineProperty(ctx.storage, 'transactionSync', { configurable:true, value:(fn) => { m.transactions++; return tx(fn); } });
}
function report() {
  return Object.fromEntries([...metrics].map(([id, m]) => {
    const byTable = {}; let rows = 0;
    for (const [table, cursor] of m.cursors) { let n = 0; try { n = cursor.rowsWritten; } catch {} rows += n; byTable[table] = (byTable[table] ?? 0) + n; }
    return [id, { exec:m.exec, writeStatements:m.writeStatements, transactions:m.transactions, epochs:m.epochs, rowsWritten:rows, byTable }];
  }));
}
export class MeasuredAgentSession extends DurableAgentSession { constructor(ctx, env) { instrument(ctx); super(ctx, env); } }
export default { async fetch(request, env, ctx) {
  const path = new URL(request.url).pathname;
  if (path === '/__fixture/metrics') return Response.json(report());
  if (path === '/__fixture/metrics/reset') { for (const m of metrics.values()) Object.assign(m, { exec:0, writeStatements:0, transactions:0, epochs:0, cursors:[] }); return Response.json({}); }
  if (path === '/__fixture') {
    const { user } = await request.json();
    await ensureAccount(env, user, true);
    const auth = await (await env.NANOCODEX_USERS.getByName(user).fetch('https://user.internal/authorization')).json();
    return Response.json(await createApiKey(env, { kind:'api_key',userId:user,...auth.grant,subjectId:'fixture:'+user,credentialId:'fixture' }, 'synthetic-write-amplification'));
  }
  return managed.fetch(request, env, ctx);
} };
`;
async function bundle(source, cwd, name) {
  const wasm = new Set();
  const output = await build({ stdin: { contents:source, resolveDir:cwd }, bundle:true, write:false,
    format:'esm', platform:'browser', target:'es2022', external:['cloudflare:*','node:*'],
    alias:{'node-rsa':resolve(repo,'js/nanocodex/tools/browser/unsupportedNodeRsa.mjs')},
    plugins:[{ name:'actual-wasm', setup(b) {
      b.onResolve({filter:/^[a-z][a-z_]*(?:\/[a-z_]+)?$/}, args => builtinModules.includes(args.path) ? {path:'node:'+args.path,external:true} : undefined);
      b.onResolve({filter:/\.wasm$|^nanocodex\/wasm$/}, args => {
        const path = args.path === 'nanocodex/wasm' ? resolve(repo,'js/nanocodex/pkg-web/nanocodex_bg.wasm') : resolve(args.resolveDir,args.path);
        wasm.add(path); return {path,external:true};
      });
    } }],
  });
  const path = resolve(evidence,`${name}.mjs`);
  const code = output.outputFiles[0].text;
  const requires = [...new Set([...code.matchAll(/__require\("(node:[^"]+)"\)/g)].map(match=>match[1]))];
  const prelude = requires.map((name,index)=>`import * as builtin${index} from ${JSON.stringify(name)};`).join('\n')
    + `\nconst requireMap={${requires.map((name,index)=>`${JSON.stringify(name)}:builtin${index}`).join(',')}}; const require=name=>{if(!requireMap[name])throw new Error('Unexpected require '+name);return requireMap[name];};\n`;
  await writeFile(path,prelude+code);
  return [{type:'ESModule',path},...Array.from(wasm,path=>({type:'CompiledWasm',path}))];
}
const DELTAS = Number(process.env.NANOCODEX_WA_DELTAS ?? 24);
const ROUNDS = Number(process.env.NANOCODEX_WA_ROUNDS ?? 6);
function sse(id, text, tool) {
  const words = Array.from({length:DELTAS}, (_, i) => `w${i} `);
  const events = [{type:'message_start',message:{id,role:'assistant',model:'claude-sonnet-4-6',content:[],usage:{input_tokens:10,output_tokens:0}}},
    {type:'content_block_start',index:0,content_block:{type:'text',text:''}},
    ...words.map(w => ({type:'content_block_delta',index:0,delta:{type:'text_delta',text:w}})),
    {type:'content_block_delta',index:0,delta:{type:'text_delta',text}},
    {type:'content_block_stop',index:0}];
  if (tool) events.push({type:'content_block_start',index:1,content_block:{type:'tool_use',id:tool.id,name:'_'+tool.name,input:{}}},
    {type:'content_block_delta',index:1,delta:{type:'input_json_delta',partial_json:JSON.stringify(tool.input)}},
    {type:'content_block_stop',index:1});
  events.push({type:'message_delta',delta:{stop_reason:tool?'tool_use':'end_turn',stop_sequence:null},usage:{output_tokens:2}},{type:'message_stop'});
  return new Response(events.map(e=>`event: ${e.type}\ndata: ${JSON.stringify(e)}\n\n`).join(''),{headers:{'content-type':'text/event-stream'}});
}
let modelCalls = 0;
const toolFor = round => round === 1 ? {name:'Write',input:{file_path:'/brain/wa.txt',content:'WA_PROOF '.repeat(64)}}
  : round === 2 ? {name:'Bash',input:{command:'sleep 2; echo slept',workdir:'/brain'}}
  : {name:'Bash',input:{command:`cat /brain/wa.txt; echo round-${round}`,workdir:'/brain'}};
async function provider(request) {
  const url = new URL(request.url);
  if (url.origin === 'https://api.anthropic.com' && url.pathname === '/v1/models')
    return Response.json({data:[{id:'claude-sonnet-4-6',display_name:'Claude Sonnet 4.6'}],has_more:false});
  if (url.origin === 'https://api.anthropic.com' && url.pathname === '/v1/messages') {
    const body = JSON.parse(await request.text()); modelCalls++;
    const start = body.messages.findLastIndex(m => m.role === "user" && (typeof m.content === "string" || m.content.some(b => b.type === "text" && b.text.includes("WA_TURN"))));
    const round = body.messages.slice(start).flatMap(m => Array.isArray(m.content) ? m.content.filter(b => b.type === "tool_result") : []).length + 1;
    const turnKey = JSON.stringify(body.messages).includes('WA_TURN_2') ? 't2' : 't1';
    if (round <= ROUNDS) { const tool = toolFor(round); return sse(`msg-${modelCalls}`, `Running ${tool.name}.`, {...tool, id:`toolu_wa_${turnKey}_${round}`}); }
    return sse(`msg-${modelCalls}`, 'CLAUDE_TOOL_DONE_WA');
  }
  const fixture = await claudeProvider(request); if (fixture) return fixture;
  return new Response('Unexpected fixture request '+url.href,{status:502});
}
async function sqliteFiles(dir) {
  const out = [];
  for (const entry of await readdir(dir, {withFileTypes:true})) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...await sqliteFiles(path)); else if (entry.name.endsWith('.sqlite')) out.push(path);
  }
  return out;
}
async function tableStats(persistence) {
  const { DatabaseSync } = await import('node:sqlite');
  for (const file of await sqliteFiles(persistence)) {
    const db = new DatabaseSync(file, {readOnly:true});
    try {
      const tables = db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all().map(r => r.name);
      if (!tables.includes('managed_events')) continue;
      const stats = {};
      let pages; try { pages = db.prepare('SELECT name, SUM(pgsize) AS bytes FROM dbstat GROUP BY name').all(); } catch {}
      for (const name of tables) {
        if (name.startsWith('sqlite_') || name.startsWith('_cf')) continue;
        const columns = db.prepare(`PRAGMA table_info("${name}")`).all().map(c => `COALESCE(LENGTH(CAST("${c.name}" AS BLOB)),0)`);
        const row = db.prepare(`SELECT COUNT(*) AS rows, COALESCE(SUM(${columns.join('+') || 0}),0) AS bytes FROM "${name}"`).get();
        if (row.rows) stats[name] = {rows:row.rows, bytes:row.bytes};
      }
      const size = (await stat(file)).size;
      return {file:file.slice(persistence.length), file_bytes:size, tables:stats, ...(pages ? {pages:Object.fromEntries(pages.map(p => [p.name, p.bytes]))} : {})};
    } finally { db.close(); }
  }
}
test('managed Claude turn storage write amplification', {timeout:300_000}, async () => {
  await mkdir(evidence,{recursive:true});
  const managedModules = await bundle(bootstrap, resolve(repo,'js/managed'), 'wa-managed');
  const egressModules = await bundle(`export * from './src/egress.ts'; export { default } from './src/egress.ts';`, resolve(repo,'js/egress'), 'wa-egress');
  const persistence = resolve(evidence,'sqlite-'+crypto.randomUUID());
  const options = {durableObjectsPersist:persistence,r2Persist:resolve(persistence,'r2'),workers:[
    {name:'managed',modulesRoot:'/',modules:managedModules,compatibilityDate:'2026-07-29',compatibilityFlags:['nodejs_compat','enable_request_signal'],
      bindings:{MANAGED_AGENT_DIRECT_CREDENTIALS:'true'},
      serviceBindings:{NANOCODEX:'egress',NANOCODEX_SESSION_MODEL_EGRESS:{name:'egress',entrypoint:'SessionModelEgress'}},
      durableObjects:Object.fromEntries([['NANOCODEX_AUTH','NonceStorage'],['NANOCODEX_USERS','UserAccount'],['NANOCODEX_ORGANIZATIONS','Organization'],['NANOCODEX_API_KEYS','ApiKeyRecord'],['NANOCODEX_SESSIONS','MeasuredAgentSession'],['NANOCODEX_ACCOUNT_TOOLS','AccountHostedTools'],['NANOCODEX_VM_HOST_POOLS','VmHostPool'],['NANOCODEX_MEMORY','MemoryScope'],['NANOCODEX_SANDBOXES','FixtureSandbox']].map(([binding,className])=>[binding,{className,useSQLite:true}])),
      r2Buckets:['NANOCODEX_HISTORY','NANOCODEX_WORKSPACES'],outboundService:provider},
    {name:'egress',modulesRoot:'/',modules:egressModules,compatibilityDate:'2026-07-29',compatibilityFlags:['nodejs_compat','enable_request_signal'],
      bindings:{ENVIRONMENT:'test',CREDENTIAL_ENCRYPTION_KEY:'MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY'},
      serviceBindings:{MANAGED_AGENT_OWNERSHIP:{name:'managed',entrypoint:'ManagedAgentOwnership'}},
      durableObjects:Object.fromEntries([['USER_CREDENTIALS','UserCredentialBroker'],['AGENT_SUBJECTS','AgentSubjectDirectory'],['USER_CONNECTORS','UserConnectorBroker'],['MCP_CONNECTIONS','McpConnectionDirectory'],['SPOTIFY_RATE_LIMITS','SpotifyRateLimit'],['GMAIL_PUSH_MAILBOXES','GmailPushMailbox']].map(([binding,className])=>[binding,{className,useSQLite:true}])),outboundService:provider},
  ]};
  let mf, token;
  const call = async (path, method='GET', body, status=200) => {
    const response = await mf.dispatchFetch('https://nanocodex.example'+path,{method,headers:{...(token?{authorization:'Bearer '+token}:{}),'content-type':'application/json',origin:'https://nanocodex.example'},...(body===undefined?{}:{body:JSON.stringify(body)})});
    const text = await response.text(); let value; try { value = JSON.parse(text); } catch { value = text; }
    assert.equal(response.status, status, path+' '+text.slice(0,400)); return value;
  };
  const turn = async (agent, input, id) => {
    const receipt = await call(`/v1/agents/${agent}/turns`,'POST',{input,id},202);
    let status;
    for (let n=0; n<1500; n++) { status = await call(`/v1/agents/${agent}/turns/${receipt.turn_id??id}`); if (['completed','failed','cancelled'].includes(status.state)) break; await new Promise(r=>setTimeout(r,40)); }
    assert.equal(status.state,'completed',JSON.stringify(status));
  };
  const results = {deltas_per_text:DELTAS, tool_rounds:ROUNDS};
  try {
    mf = new Miniflare(options);
    token = (await call('/__fixture','POST',{user:identity})).token;
    const login = await call('/v1/credentials/claude/login','POST');
    await call('/v1/credentials/claude/login/complete','POST',{code:`managed-runtime#${new URL(login.authorization_url).searchParams.get('state')}`});
    const agent = (await call('/v1/agents','POST',{settings:{model:'claude-sonnet-4-6',thinking:'low',reasoning_mode:'standard',fast_mode:false}},201)).agent_id;
    await turn(agent,'WA_TURN_1 warm the session','wa-turn-1');
    const historyAfterFirst = await call(`/v1/agents/${agent}/events/history?after=0&limit=256`);
    const firstCursor = historyAfterFirst.latest_cursor;
    await call('/__fixture/metrics/reset','POST');
    const started = performance.now();
    await turn(agent,'WA_TURN_2 measured turn','wa-turn-2');
    results.turn_ms = Math.round(performance.now() - started);
    const metrics = await call('/__fixture/metrics');
    results.session_writes = Object.values(metrics).sort((a,b)=>b.rowsWritten-a.rowsWritten)[0];
    const events = []; let after = firstCursor;
    for (;;) { const page = await call(`/v1/agents/${agent}/events/history?after=${after}&limit=256`); events.push(...page.data); if (!page.has_more || !page.data.length) break; after = page.data.at(-1).cursor; }
    const byType = {};
    for (const row of events) { const type = row.type === 'event' ? row.event.type : row.type; const bytes = JSON.stringify(row).length;
      byType[type] ??= {rows:0, bytes:0}; byType[type].rows++; byType[type].bytes += bytes; }
    results.turn_events = {rows:events.length, bytes:events.reduce((n,row)=>n+JSON.stringify(row).length,0), by_type:byType};
    assert.ok(events.some(row => row.event?.type === 'assistant.message' && JSON.stringify(row).includes('CLAUDE_TOOL_DONE_WA')), 'final answer recorded');
    await mf.dispose(); mf = undefined;
    results.database = await tableStats(persistence);
    console.info('WRITE_AMPLIFICATION', JSON.stringify(results));
    await writeFile(resolve(evidence, `write-amplification-${process.env.NANOCODEX_WA_LABEL ?? 'run'}.json`), JSON.stringify(results, null, 2));
  } finally {
    await mf?.dispose();
    await rm(persistence,{recursive:true,force:true});
  }
});
