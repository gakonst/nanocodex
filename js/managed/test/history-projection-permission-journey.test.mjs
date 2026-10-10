// Run: node --test js/managed/test/history-projection-permission-journey.test.mjs
// Real workerd, SQLite, membership RPC, completion/outbox transaction and alarms.
// Fixtures seed already-completed turns and inject the otherwise unavailable RPC
// outage. Queue/alarm inspection has no public API, so it stays in this fixture.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { setTimeout as delay } from 'node:timers/promises';
import { build } from 'esbuild';
import { Miniflare } from 'miniflare';

const team = '11111111-1111-4111-8111-111111111111';
const owner = '11111111-1111-4111-8111-111111111112';
const writer = '11111111-1111-4111-8111-111111111113';
const source = `
import { DurableAgentSession, commitManagedTransition } from './src/index.ts';
import { Organization } from './src/account-auth.ts';
import { DurableEventLog } from './src/durable-events.ts';
import { storeTurnInput } from './src/managed-turn-input.ts';
import { MemoryScope } from './src/memory-scope.ts';
const warn = console.warn.bind(console);
console.warn = record => warn(JSON.stringify(record));
export class FixtureMemory extends MemoryScope {
  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === '/fixture/outage') {
      await this.ctx.storage.put('fixture-outage', await request.json());
      return new Response(null, {status:204});
    }
    if (path === '/project' && await this.ctx.storage.get('fixture-outage')) return new Response(null, {status:503});
    return super.fetch(request);
  }
}
export class FixtureOrganization extends Organization {
  async resolveCompanyMembership(user) {
    if (await this.ctx.storage.get('fixture-outage')) throw new Error('Synthetic membership RPC unavailable');
    return super.resolveCompanyMembership(user);
  }
  async fetch(request) {
    if (new URL(request.url).pathname === '/fixture/outage') {
      await this.ctx.storage.put('fixture-outage', await request.json());
      return new Response(null, {status:204});
    }
    return super.fetch(request);
  }
}
export class FixtureSession extends DurableAgentSession {
  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === '/fixture/seed') {
      await (await super.fetch(new Request('https://fixture.internal/missing'))).body?.cancel();
      const now = Date.now(), sql = this.ctx.storage.sql;
      const uuid = crypto.randomUUID().split('-'); uuid[2] = '7' + uuid[2].slice(1);
      sql.exec("INSERT INTO session_state (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES (1,?,?,?,?,1,'https://fixture.internal','managed',?)", uuid.join('-'), '${writer}', '${team}', '${team}', now);
      this.ctx.storage.kv.put('context_team_id', '${team}');
      const log = new DurableEventLog(this.ctx.storage);
      for (let i = 0; i < 19; i++) {
        const id = 'turn-' + i, input = i === 0 ? 'x'.repeat(300000) : 'Synthetic team turn';
        sql.exec("INSERT INTO managed_turns (id,request_hash,input_json,authorization_json,state,accepted_cursor,may_have_inner_operation,created_at,accepted_at,updated_at) VALUES (?, 'fixture', ?, '{}', 'accepted', 0, 0, ?, ?, ?)", id, storeTurnInput(this.ctx.storage, id, JSON.stringify(input)), now, now, now);
        commitManagedTransition(this.ctx.storage, log, id, {type:'turn_completed',id,final_message:'Saved team result',usage:null,citations:[]});
      }
      sql.exec("UPDATE history_projection_outbox SET attempt_count = 7 WHERE turn_id = 'turn-17'");
      sql.exec("UPDATE history_projection_outbox SET retry_at = ?, attempt_count = 4 WHERE turn_id = 'turn-18'", now + 300000);
      // Independent maintenance must keep its deadline after projection retirement.
      sql.exec("INSERT INTO managed_webhook VALUES (1,'https://webhook.invalid/receipt','synthetic-secret')");
      sql.exec("INSERT INTO managed_webhook_deliveries VALUES ('maintenance','{}',0,?,'pending')", now + 60000);
      return new Response(null, {status:204});
    }
    if (path === '/fixture/alarm') {
      await this.alarm();
      return new Response(null, {status:204});
    }
    if (path === '/fixture/due') {
      this.ctx.storage.sql.exec('UPDATE history_projection_outbox SET retry_at = 0');
      return new Response(null, {status:204});
    }
    if (path === '/fixture/state') return Response.json({
      session_id:this.ctx.storage.sql.exec('SELECT session_id FROM session_state').one().session_id,
      rows:this.ctx.storage.sql.exec('SELECT turn_id,attempt_count,retry_at FROM history_projection_outbox ORDER BY rowid').toArray(),
      chunks:this.ctx.storage.sql.exec('SELECT COUNT(*) AS n FROM managed_history_projection_chunks').one().n,
      alarm:await this.ctx.storage.getAlarm(),
      maintenance:this.ctx.storage.sql.exec("SELECT retry_at FROM managed_webhook_deliveries WHERE id = 'maintenance'").one().retry_at,
    });
    return super.fetch(request);
  }
}
export default {fetch(request, env) {
  const url = new URL(request.url), name = url.searchParams.get('session');
  if (url.pathname.startsWith('/memory/')) {
    url.pathname = url.pathname.slice('/memory'.length);
    const forwarded = new Request(url, request);
    forwarded.headers.set('x-nanocodex-organization-id', '${team}');
    forwarded.headers.set('x-nanocodex-team-id', '${team}');
    forwarded.headers.set('x-nanocodex-memory-initialize', '1');
    return env.NANOCODEX_MEMORY.getByName('${team}').fetch(forwarded);
  }
  return name ? env.NANOCODEX_SESSIONS.getByName(name).fetch(request)
    : env.NANOCODEX_ORGANIZATIONS.getByName('${team}').fetch(request);
}};
`;

test('projection permission denial retires work; membership outages back off and recover', {timeout:90_000}, async () => {
  const root = fileURLToPath(new URL('..', import.meta.url));
  const output = join(root, '../../output/history-projection-permission', `${Date.now()}-${process.pid}`);
  await mkdir(output, {recursive:true});
  const assets = [], logs = [], trace = [];
  let assetNext = 0;
  const bundled = await build({stdin:{contents:source,resolveDir:root},bundle:true,write:false,format:'esm',target:'es2022',platform:'node',conditions:['workerd'],
    banner:{js:'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");'},external:['cloudflare:*','node:*'],
    alias:{'node-rsa':join(root,'../nanocodex/tools/browser/unsupportedNodeRsa.mjs')},plugins:[{name:'wasm',setup(b){
      b.onResolve({filter:/\.wasm$/},async args=>{const path='fixture-'+(assetNext++)+'.wasm';assets.push({type:'CompiledWasm',path,contents:await readFile(join(args.resolveDir,args.path))});return {path:'./'+path,external:true};});
    }}]});
  const mf = new Miniflare({port:0,modules:[{type:'ESModule',path:'worker.mjs',contents:bundled.outputFiles[0].text},...assets],
    compatibilityDate:'2026-07-30',compatibilityFlags:['nodejs_compat','enable_request_signal'],
    durableObjects:{NANOCODEX_SESSIONS:{className:'FixtureSession',useSQLite:true},NANOCODEX_ORGANIZATIONS:{className:'FixtureOrganization',useSQLite:true},NANOCODEX_MEMORY:{className:'FixtureMemory',useSQLite:true}},
    r2Buckets:['NANOCODEX_HISTORY','NANOCODEX_WORKSPACES'],handleRuntimeStdio(stdout,stderr){for(const stream of [stdout,stderr])createInterface({input:stream}).on('line',line=>logs.push(line));}});
  const curl = promisify(execFile);
  try {
    const base = await mf.ready;
    const call = async (path, body, status = body === undefined ? 200 : 204) => {
      const prefix = join(output, String(trace.length));
      const args = ['--silent','--show-error','--max-time','10','-D',prefix+'.headers','-o',prefix+'.body','-w','%{http_code}'];
      if (body !== undefined) {await writeFile(prefix+'.request.json', JSON.stringify(body));args.push('-H','content-type: application/json','--data-binary','@'+prefix+'.request.json');}
      args.push(new URL(path, base).href);
      const {stdout} = await curl('curl', args);
      trace.push({command:['curl',...args],status:Number(stdout)});
      assert.equal(Number(stdout), status, path + ': ' + await readFile(prefix+'.body','utf8'));
      const raw = await readFile(prefix+'.body','utf8');return raw ? JSON.parse(raw) : undefined;
    };
    const poll = async predicate => {for(let i=0;i<100;i++){const value=await predicate();if(value)return value;await delay(20);}throw Error('Projection did not settle');};
    await call('/company/create', {actor:owner,id:team,name:'Synthetic Company'}, 201);
    const joinWriter = async () => {const invite=await call('/company/invite',{actor:owner,user_id:writer,role:'writer'},201);await call('/company/accept',{actor:writer,token:invite.token},200);};
    await joinWriter();
    for (const [name, change] of [['downgraded',{role:'reader'}],['revoked',{remove:true}]]) {
      if (name === 'revoked') await call('/company/member',{actor:owner,user_id:writer,role:'writer'},200);
      await call('/fixture/seed?session='+name, {});
      const before = await call('/fixture/state?session='+name);
      assert.equal(before.rows.length,19);assert.ok(before.chunks > 0);
      await call('/company/member',{actor:owner,user_id:writer,...change},200);
      await call('/fixture/alarm?session='+name, {});
      const after = await poll(async()=>{const s=await call('/fixture/state?session='+name);return s.rows.length===0 && s.alarm===s.maintenance && s;});
      assert.equal(after.chunks,0);assert.ok(after.alarm > Date.now()+10000);
      const receipt=await call('/turns/turn-0?session='+name);assert.equal(receipt.state,'completed');assert.equal(receipt.input,'x'.repeat(300000));
      assert.deepEqual((await call('/memory/read',{session_id:before.session_id},200)).turns,[]);
      await call('/fixture/alarm?session='+name, {});
      assert.equal((await call('/fixture/state?session='+name)).alarm, after.maintenance);
    }
    await joinWriter();
    const name = 'temporary';
    await call('/fixture/seed?session='+name, {});
    const before=await call('/fixture/state?session='+name);
    await call('/fixture/outage',true);
    await call('/fixture/alarm?session='+name, {});
    const after=await poll(async()=>{const s=await call('/fixture/state?session='+name);return s.rows[0]?.attempt_count===1 && s.alarm===Math.min(...s.rows.map(r=>r.retry_at),s.maintenance) && s;});
    assert.equal(after.rows.length,19);assert.equal(after.chunks,before.chunks);
    for(let i=0;i<18;i++){assert.equal(after.rows[i].attempt_count,before.rows[i].attempt_count+1);assert.ok(after.rows[i].retry_at > Date.now());}
    assert.deepEqual(after.rows[18],before.rows[18]);
    assert.ok(after.rows[17].retry_at-after.rows[0].retry_at >= 59000,'existing capped exponential backoff');
    await call('/fixture/outage',false);
    await call('/fixture/due?session='+name, {});
    await call('/fixture/alarm?session='+name, {});
    await poll(async()=>{const s=await call('/fixture/state?session='+name);return s.rows.length===0 && s.alarm===s.maintenance && s.chunks===0;});
    assert.equal((await call('/turns/turn-0?session='+name)).state,'completed');
    const projected=await call('/memory/read',{session_id:before.session_id},200);
    assert.equal(projected.turns.length,19);assert.equal(projected.turns.find(t=>t.turn_id==='turn-0').user,'x'.repeat(300000));
    await call('/fixture/seed?session=delivery', {});
    const deliveryBefore=await call('/fixture/state?session=delivery');
    await call('/memory/fixture/outage',true);
    await call('/fixture/alarm?session=delivery', {});
    const deliveryAfter=await poll(async()=>{const s=await call('/fixture/state?session=delivery');return s.rows.slice(0,18).every(r=>r.attempt_count>0 && r.retry_at>Date.now()) && s;});
    assert.equal(deliveryAfter.rows.length,19);assert.equal(deliveryAfter.chunks,deliveryBefore.chunks);
    assert.deepEqual(deliveryAfter.rows[18],deliveryBefore.rows[18]);
    await call('/memory/fixture/outage',false);
    await call('/fixture/due?session=delivery', {});
    await call('/fixture/alarm?session=delivery', {});
    await poll(async()=>{const s=await call('/fixture/state?session=delivery');return s.rows.length===0 && s.alarm===s.maintenance;});
    assert.equal((await call('/memory/read',{session_id:deliveryBefore.session_id},200)).turns.length,19);
    assert.ok(logs.some(line=>line.includes('managed.history_projection_failed')));
    console.log(JSON.stringify({evidence:output,downgrade:'retired',revocation:'retired',lookup_outage:'backoff then recovered',delivery_outage:'backoff then recovered',maintenance:'scheduled',original_history:'retained'}));
  } finally {
    await mf.dispose();
    await writeFile(join(output,'trace.json'),JSON.stringify(trace,null,2));
    await writeFile(join(output,'runtime.log'),logs.join('\n'));
  }
});
