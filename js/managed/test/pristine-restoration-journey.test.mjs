import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { setTimeout as delay } from 'node:timers/promises';
import { build } from 'esbuild';
import { Miniflare } from 'miniflare';

// Real managed HTTP lifecycle and SQLite across workerd restart. Fixture-only
// writes represent interrupted initialization and a corrupt retained turn;
// recovery, failure receipts and history-outbox delivery use production code.
const source = `
import { DurableObject } from 'cloudflare:workers';
import { DurableAgentSession } from './src/index.ts';
const info=console.info.bind(console);
console.info=(record,...rest)=>info(typeof record==='object'?JSON.stringify(record):record,...rest);
export class FixtureSession extends DurableAgentSession {
  constructor(ctx,env) {
    if(ctx.id.name==='kv-pending') ctx.storage.kv.put('nanocodex:durability-import-state','pending');
    if(ctx.id.name==='kv-unknown') ctx.storage.kv.put('fixture-retained','present');
    super(ctx,env);
  }
  async fetch(request) {
    if(new URL(request.url).pathname==='/seed-retained') {
      const initialized=await super.fetch(new Request('https://fixture.internal/missing'));
      await initialized.body?.cancel();
      const now=Date.now();
      this.ctx.storage.sql.exec("INSERT INTO session_state (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES (1,'00000000-0000-7000-8000-000000000001','fixture-owner','fixture-org','fixture-team',1,'https://nanocodex.example/','managed',?)",now);
      this.ctx.storage.sql.exec("INSERT INTO managed_configuration VALUES (1,'{}')");
      this.ctx.storage.sql.exec("INSERT INTO managed_turns (id,request_hash,input_json,authorization_json,state,accepted_cursor,may_have_inner_operation,created_at,accepted_at,updated_at) VALUES ('retained','fixture','null','{\\"capabilities\\":[]}','accepted',0,0,?,?,?)",now,now,now);
      this.ctx.storage.sql.exec("INSERT INTO history_projection_outbox (turn_id,payload_json,attempt_count,retry_at,source_cursor) VALUES ('projection','{\\"proof\\":\\"retained-history\\"}',0,0,'1')");
      await this.ctx.storage.sync();
      return new Response(null,{status:204});
    }
    return super.fetch(request);
  }
}
export class Memory extends DurableObject {
  async fetch(request) {
    if(new URL(request.url).pathname==='/project') {
      await this.ctx.storage.put('projection',await request.json());
      return new Response(null,{status:204});
    }
    return Response.json(await this.ctx.storage.get('projection')??null);
  }
}
export default { fetch(request,env) {
  const url=new URL(request.url);
  if(url.pathname==='/memory') return env.NANOCODEX_MEMORY.getByName('fixture-org').fetch(request);
  const name=url.searchParams.get('name')??'fresh';
  url.search='';
  return env.NANOCODEX_SESSIONS.getByName(name).fetch(new Request(url,request));
}};
`;

test('pristine creation skips restoration while KV-only imports and retained work remain recoverable', { timeout: 60_000 }, async () => {
  const root=fileURLToPath(new URL('..',import.meta.url));
  const output=join(root,'../../output/pristine-restoration-journey',`${Date.now()}-${process.pid}`);
  await mkdir(output,{recursive:true});
  const assets=[],logs=[],trace=[]; let assetNext=0;
  const bundle=await build({stdin:{contents:source,resolveDir:root},bundle:true,write:false,format:'esm',target:'es2022',platform:'node',conditions:['workerd'],banner:{js:'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");'},external:['cloudflare:*','node:*'],alias:{'node-rsa':join(root,'../nanocodex/tools/browser/unsupportedNodeRsa.mjs')},plugins:[{name:'wasm',setup(builder){builder.onResolve({filter:/\.wasm$/},async args=>{const name=`fixture-${assetNext++}.wasm`;assets.push({type:'CompiledWasm',path:name,contents:await readFile(join(args.resolveDir,args.path))});return {path:`./${name}`,external:true};});}}]});
  const modules=[{type:'ESModule',path:'worker.mjs',contents:bundle.outputFiles[0].text},...assets];
  const start=()=>new Miniflare({modules,compatibilityDate:'2026-07-30',compatibilityFlags:['nodejs_compat','enable_request_signal'],durableObjectsPersist:join(output,'sqlite'),r2Persist:join(output,'r2'),durableObjects:{NANOCODEX_SESSIONS:{className:'FixtureSession',useSQLite:true},NANOCODEX_MEMORY:{className:'Memory',useSQLite:true}},r2Buckets:['NANOCODEX_HISTORY','NANOCODEX_WORKSPACES'],handleRuntimeStdio(stdout,stderr){for(const stream of [stdout,stderr])createInterface({input:stream}).on('line',line=>logs.push(line));}});
  let mf=start();
  const call=async path=>{const response=await mf.dispatchFetch('https://fixture.internal'+path);const body=await response.text();trace.push({path,status:response.status,body});return {status:response.status,body};};
  const poll=async fn=>{for(let i=0;i<200;i++){const result=await fn();if(result)return result;await delay(25);}throw Error('retained work did not recover');};
  const observations=()=>logs.flatMap(line=>{const start=line.indexOf('{"type":');if(start<0)return [];try{return [JSON.parse(line.slice(start))];}catch{return [];}}).filter(row=>row.type==='managed.session.storage_initialized');
  try {
    assert.equal((await call('/missing')).status,404);
    await poll(()=>observations().length===1);
    assert.equal(observations()[0].restore_skipped,true,'empty SQL and KV skips restore work');
    assert.equal((await call('/missing?name=kv-pending')).status,409,'KV-only import fence remains authoritative');
    await poll(()=>observations().length===2);
    assert.equal(observations()[1].restore_skipped,false);
    assert.equal((await call('/missing?name=kv-unknown')).status,404);
    await poll(()=>observations().length===3);
    assert.equal(observations()[2].restore_skipped,false,'even unknown retained KV prevents pristine classification');
    assert.equal((await call('/seed-retained?name=retained')).status,204);
    await poll(()=>observations().length===4);
    assert.equal(observations()[3].restore_skipped,true);
    await mf.dispose();mf=start();
    // This ordinary read activates the retained constructor. No fixture resume
    // or alarm call is made: recovery must be scheduled by restoration itself.
    const receipt=await poll(async()=>{const response=await call('/turns/retained?name=retained');return response.status===200&&JSON.parse(response.body).state==='failed'?JSON.parse(response.body):undefined;});
    assert.equal(receipt.state,'failed');
    await poll(async()=>JSON.parse((await call('/memory')).body)?.proof==='retained-history');
    await poll(()=>observations().length>=5);
    assert.equal(observations()[4].restore_skipped,false,'retained schema is restored after workerd restart');
    assert.equal((await call('/missing?name=kv-pending')).status,409);
    console.log(JSON.stringify({evidence:output,fresh_restore_skipped:true,kv_only_import_status:409,retained_turn:receipt.state,history_projection:'retained-history'}));
  } finally {
    await mf.dispose();
    await writeFile(join(output,'trace.json'),JSON.stringify({trace,observations:observations()},null,2)+'\n');
    await writeFile(join(output,'runtime.log'),logs.join('\n')+'\n');
  }
});
