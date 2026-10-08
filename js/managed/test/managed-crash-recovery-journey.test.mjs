import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fork } from 'node:child_process';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// Actual HTTP, managed admission/cancel-only recovery, Rust WASM, Worker SQLite
// and process loss. The provider is the only simulated external dependency.
// Kill the fixture process group, not dispose()/session.shutdown(): graceful
// socket-close callbacks would measure caught retries instead of isolate loss.
const source = `
import { DurableObject } from 'cloudflare:workers';
import { DurableAgentSession } from './src/index.ts';
import { createBrainBucket } from './src/brain-bucket.ts';
import { createBrainWorkspace } from './src/brain-workspace.ts';
export class FixtureSession extends DurableAgentSession {
  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === '/__seed') {
      // Enter the production HTTP lifecycle before fault-injecting retained work.
      // Fresh sessions intentionally defer schema setup until their first request.
      await super.fetch(new Request('https://fixture.internal/__initialize'));
      const { id } = await request.json(); const now = Date.now();
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO session_state (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES (1,'00000000-0000-7000-8000-000000000001','fixture-owner','fixture-org','fixture-team',1,'https://nanocodex.example/','managed',?)", now);
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO managed_configuration VALUES (1, ?)",JSON.stringify({environment:{files:[],skills:[],setup_commands:[],network:{access:'enabled'}}}));
      this.ctx.storage.sql.exec("UPDATE managed_agent_settings SET model='gpt-6.1-sol',thinking='low'");
      this.ctx.storage.sql.exec("INSERT INTO managed_turns (id,request_hash,input_json,authorization_json,state,accepted_cursor,dispatch_input_chunks,may_have_inner_operation,created_at,accepted_at,updated_at) VALUES (?,'fixture-hash',?,?,'accepted',0,1,0,?,?,?)",id,JSON.stringify('fixture'),JSON.stringify({capabilities:['agents:read','agents:write','tools:use']}),now,now,now);
      this.ctx.storage.sql.exec("INSERT INTO managed_turn_dispatch_chunks VALUES (?,0,?)",id,JSON.stringify('fixture'));
      await this.ctx.storage.sync(); return new Response(null,{status:204});
    }
    if(path === '/__resume') { await this.alarm(); return new Response(null,{status:204}); }
    if(path === '/__functions') {
      this.ctx.storage.sql.exec('UPDATE managed_configuration SET body=? WHERE singleton=1',JSON.stringify({tools:['exec_command'],environment:{files:[],skills:[],setup_commands:[],network:{access:'disabled'}}}));
      await this.ctx.storage.sync(); return new Response(null,{status:204});
    }
    if(path === '/__child-proof') { const workspace=createBrainWorkspace(createBrainBucket(this.ctx.storage,this.env.NANOCODEX_WORKSPACES,'00000000-0000-7000-8000-000000000001'),'00000000-0000-7000-8000-000000000001');return Response.json({text:new TextDecoder().decode(await workspace.readFile('/brain/child-proof.txt'))}); }
    if(path === '/__direct-proof') { const workspace=createBrainWorkspace(createBrainBucket(this.ctx.storage,this.env.NANOCODEX_WORKSPACES,'00000000-0000-7000-8000-000000000001'),'00000000-0000-7000-8000-000000000001');return Response.json({text:new TextDecoder().decode(await workspace.readFile('/brain/direct-proof.txt'))}); }
    if(path === '/__forget-code-journal') {
      // Simulate the pre-journal upgrade boundary AFTER real nested effects and
      // a retained original Rust outer intent, not by fabricating that head.
      for(const table of ['managed_code_effects','managed_code_effect_receipt_chunks','managed_code_effect_legacy_parents','managed_code_effect_legacy_sessions','managed_code_effect_migration']) this.ctx.storage.sql.exec('DELETE FROM '+table);
      await this.ctx.storage.sync(); return new Response(null,{status:204});
    }
    if(path === '/__proof') { const workspace=createBrainWorkspace(createBrainBucket(this.ctx.storage,this.env.NANOCODEX_WORKSPACES,'00000000-0000-7000-8000-000000000001'),'00000000-0000-7000-8000-000000000001');return Response.json({text:new TextDecoder().decode(await workspace.readFile('/brain/progress-proof.txt'))}); }
    if(path === '/__inspect') { const turns=this.ctx.storage.sql.exec("SELECT id,state,error,attempt_count,may_have_inner_operation FROM managed_turns ORDER BY id").toArray();const safety=this.ctx.storage.sql.exec("SELECT * FROM managed_recovery_safety ORDER BY turn_id").toArray();const effects=this.ctx.storage.sql.exec("SELECT name,state,parent_call_id,call_id,turn_id,session_id,operation_id,model_call_index FROM managed_code_effects ORDER BY created_at DESC LIMIT 10").toArray();const results=this.ctx.storage.sql.exec("SELECT substr(message_json,1,1800) AS message FROM managed_events WHERE json_extract(message_json, '$.event.type')='tool.result' ORDER BY cursor DESC LIMIT 6").toArray();const objects=this.ctx.storage.sql.exec("SELECT key FROM nanocodex_brain_objects LIMIT 20").toArray();return Response.json({turns,safety,effects,results,objects}); }
    return super.fetch(request);
  }
}
const codeCall = (name, callId, args) => ({type:'custom_tool_call',name:'exec',call_id:callId,input:'text(await tools.'+name+'('+JSON.stringify(args)+'));'});
export class FixtureModel extends DurableObject {
  constructor(ctx,env){super(ctx,env);ctx.storage.sql.exec('CREATE TABLE IF NOT EXISTS model_fixture (singleton INTEGER PRIMARY KEY,requests INTEGER NOT NULL,complete INTEGER NOT NULL,stage INTEGER NOT NULL,emitted INTEGER NOT NULL,child_emitted INTEGER NOT NULL);INSERT OR IGNORE INTO model_fixture VALUES(1,0,0,0,0,0)');}
  async fetch(request){
    const path=new URL(request.url).pathname;
    if(path.startsWith('/__stage/')){this.ctx.storage.sql.exec('UPDATE model_fixture SET complete=0,stage=?,emitted=0,child_emitted=0',Number(path.split('/').pop()));await this.ctx.storage.sync();return new Response(null,{status:204});}
    if(path==='/__complete'){this.ctx.storage.sql.exec('UPDATE model_fixture SET complete=1');await this.ctx.storage.sync();return new Response(null,{status:204});}
    if(path==='/__inspect')return Response.json(this.ctx.storage.sql.exec('SELECT requests,complete FROM model_fixture').one());
    if(request.headers.get('upgrade')!=='websocket')return Response.json({tools:[],machines:[],connections:[]});
    const pair=new WebSocketPair();const client=pair[0],server=pair[1];server.accept();let childSocket=false;
    server.addEventListener('message',event=>{this.ctx.storage.sql.exec('UPDATE model_fixture SET requests=requests+1');const row=this.ctx.storage.sql.exec('SELECT requests,complete,stage,emitted,child_emitted FROM model_fixture').one();if(row.stage===10){
      const body=JSON.parse(event.data);childSocket ||= JSON.stringify((body.input??[]).filter(item=>item.role==='user')).includes('JOURNAL_CHILD_FIXTURE');const child=childSocket;
      let output;
      if(child&&!row.child_emitted){this.ctx.storage.sql.exec('UPDATE model_fixture SET child_emitted=1');output=[codeCall('exec_command','fixture-child-command',{cmd:'printf JOURNAL_CHILD_FIXTURE > /brain/child-proof.txt',workdir:'/brain'})];}
      else if(!child&&!row.emitted){this.ctx.storage.sql.exec('UPDATE model_fixture SET emitted=1');output=[codeCall('spawn_agent','fixture-child-spawn',{role:'Fixture child',task:'JOURNAL_CHILD_FIXTURE: write the synthetic proof once using exec_command, then finish.',model:'sol',thinking:'low',output_contract:{kind:'string'}})];}
      else if(!child&&row.emitted===1){this.ctx.storage.sql.exec('UPDATE model_fixture SET emitted=2');output=[codeCall('wait_agent','fixture-child-wait',{agent_ids:[1],timeout_ms:300000})];}
      else if(child&&row.child_emitted===1){this.ctx.storage.sql.exec('UPDATE model_fixture SET child_emitted=2');output=[codeCall('submit_result','fixture-child-submit',{output:'CHILD_OK'})];}
      else output=[{type:'message',role:'assistant',content:[{type:'output_text',text:child?'CHILD_OK':'ROOT_CHILD_SPAWNED'}]}];
      server.send(JSON.stringify({type:'response.completed',response:{id:'fixture-child-'+row.requests,status:'completed',output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));return;
    }
    if(!row.complete){if(row.stage&&!row.emitted){this.ctx.storage.sql.exec('UPDATE model_fixture SET emitted=1');server.send(JSON.stringify({type:'response.completed',response:{id:'fixture-progress-'+row.stage,status:'completed',output:row.stage===9?[codeCall('exec_command','fixture-restricted',{cmd:"printf 'restricted-effect\\n' >> /brain/direct-proof.txt && sleep 3600"})]:[{type:'custom_tool_call',name:'exec',call_id:'fixture-stage-'+row.stage,input:'text(await tools.exec_command({cmd:'+JSON.stringify("printf 'forward-"+row.stage+"\\n' >> /brain/progress-proof.txt")+'}));'+(row.stage===8?' await tools.exec_command({cmd:"sleep 3600"});':'')}],usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));}return;}
      server.send(JSON.stringify({type:'response.completed',response:{id:'fixture-response-'+row.requests,status:'completed',end_turn:true,output:[{type:'message',role:'assistant',content:[{type:'output_text',text:'NEXT_TURN_OK'}]}],usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));});
    return new Response(null,{status:101,webSocket:client});
  }
}
export default{async fetch(request,env){const path=new URL(request.url).pathname;if(path.startsWith('/model/'))return env.MODEL.getByName('fixture-provider').fetch(new Request('https://fixture.internal/'+path.slice(7),request));return env.NANOCODEX_SESSIONS.getByName('fixture-session').fetch(request);}};
`;

test('abrupt managed Worker loss stops at its durable budget and admits the next turn', {timeout:180_000}, async()=>{
  const root=fileURLToPath(new URL('..',import.meta.url));
  const output=fileURLToPath(new URL('../../../output/exec-stall-20261001/managed-process-journey/',import.meta.url))+crypto.randomUUID();
  await mkdir(output,{recursive:true});
  const wasm=[];let index=0;
  const bundle=await build({stdin:{contents:source,resolveDir:root},bundle:true,write:false,format:'esm',target:'es2022',platform:'node',conditions:['workerd'],banner:{js:'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");'},external:['cloudflare:*','node:*'],alias:{'node-rsa':root+'/node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs'},plugins:[{name:'compiled-wasm',setup(build){build.onResolve({filter:/\.wasm$/},async args=>{const path=fileURLToPath(new URL(args.path,'file://'+args.resolveDir+'/'));const name='./fixture-'+index+++'.wasm';wasm.push({name,contents:await readFile(path)});return {path:name,external:true};});}}]});
  await writeFile(output+'/worker.mjs',bundle.outputFiles[0].text);
  for(const asset of wasm)await writeFile(output+'/'+asset.name,asset.contents);
  await writeFile(output+'/assets.json',JSON.stringify(wasm.map(asset=>asset.name)));
  const trace=[];let child;
  const start=()=>new Promise((resolve,reject)=>{
    child=fork(fileURLToPath(new URL('./fixtures/managed-crash-process.mjs',import.meta.url)),[output],{detached:true,stdio:['ignore','pipe','pipe','ipc']});
    child.stdout.on('data',chunk=>writeFile(output+'/child-stdout.log',chunk,{flag:'a'}));child.stderr.on('data',chunk=>writeFile(output+'/child-stderr.log',chunk,{flag:'a'}));
    child.once('error',reject);child.once('message',message=>message.ready?resolve():reject(Error('fixture not ready')));
  });
  let sequence=0,lastInspection;
  const call=(action,id)=>new Promise((resolve,reject)=>{const request=++sequence;const onMessage=message=>{if(message.request!==request)return;child.off('message',onMessage);if(action==='inspect')lastInspection=message.value;message.error?reject(Error(message.error)):resolve(message.value);};child.on('message',onMessage);child.send({request,action,id});});
  const poll=async fn=>{const deadline=Date.now()+15_000;for(;;){const value=await fn();if(value)return value;if(Date.now()>deadline)throw Error('fixture did not reach expected durable state: '+JSON.stringify(lastInspection));await new Promise(resolve=>setTimeout(resolve,25));}};
  const kill=()=>new Promise((resolve,reject)=>{const current=child;current.once('exit',()=>{child=undefined;resolve();});try{process.kill(-current.pid,'SIGKILL');}catch(error){reject(error);}});
  try{
    for(let attempt=1;attempt<=3;attempt++){
      await start();if(attempt===1)await call('seed','poison');await call('resume');
      const observed=await poll(async()=>{const value=await call('inspect');return value.model.requests===attempt&&value.session.safety[0]?.abrupt_attempts===attempt?value:undefined;});
      assert.equal(observed.session.turns[0].state,'accepted');assert.equal(observed.session.safety[0].armed,1);assert.equal(observed.session.safety[0].stopped,0);trace.push({attempt,observed});await kill();
    }
    await start();await call('resume');
    const stopped=await poll(async()=>{const value=await call('inspect');return value.session.turns.find(row=>row.id==='poison')?.state==='failed'?value:undefined;});
    assert.equal(stopped.model.requests,3);assert.match(stopped.session.turns[0].error,/MANAGED_RECOVERY_EXHAUSTED/);trace.push({bounded_terminal:stopped});
    const receipt=await call('receipt','poison');assert.equal(receipt.state,'failed');assert.match(receipt.terminal.error,/outcome unknown/);trace.push({public_receipt:receipt});
    await call('complete');await call('seed','next');await call('resume');
    const next=await poll(async()=>{const value=await call('inspect');return value.session.turns.find(row=>row.id==='next')?.state==='completed'?value:undefined;});
    assert.equal(next.model.requests,4);trace.push({next});
    await call('seed','progress');
    for(let stage=1;stage<=7;stage++){
      if(stage>1)await start();await call('stage',stage);await call('resume');
      const observed=await poll(async()=>{const value=await call('inspect');return value.model.requests===4+stage*2&&value.session.safety.find(row=>row.turn_id==='progress')?.abrupt_attempts===0?value:undefined;});
      assert.equal(observed.session.turns.find(row=>row.id==='progress').state,'accepted');trace.push({forward_stage:stage,observed});await kill();
    }
    await start();await call('complete');await call('resume');
    const progressed=await poll(async()=>{const value=await call('inspect');return value.session.turns.find(row=>row.id==='progress')?.state==='completed'?value:undefined;});
    assert.equal(progressed.model.requests,19);const proof=await call('proof');assert.equal(proof.text,Array.from({length:7},(_,i)=>'forward-'+(i+1)+'\n').join(''));trace.push({progressed,proof});
    // Actual upgrade safety: a completed file append followed by an unfinished
    // nested sleep leaves the original Rust outer Code Mode step EffectPending.
    // Remove only host journal metadata, then physically kill the old owner.
    await call('seed','legacy');await call('stage',8);await call('resume');
    const legacyPending=await poll(async()=>{const value=await call('inspect');return value.session.effects.some(row=>row.parent_call_id==='fixture-stage-8'&&row.call_id.endsWith('/code-2')&&row.state==='pending')?value:undefined;});
    assert.equal((await call('proof')).text,Array.from({length:8},(_,i)=>'forward-'+(i+1)+'\n').join(''));trace.push({legacy_pending:legacyPending});
    await call('forget');await kill();
    await start();await call('complete');await call('resume');
    const upgraded=await poll(async()=>{const value=await call('inspect');return value.session.turns.find(row=>row.id==='legacy')?.state==='completed'?value:undefined;});
    assert.ok(upgraded.session.results.some(row=>row.message.includes('outcome unknown')),'recovered legacy cell must report unknown instead of dispatching');
    const upgradedProof=await call('proof');assert.equal(upgradedProof.text,Array.from({length:8},(_,i)=>'forward-'+(i+1)+'\n').join(''));trace.push({legacy_upgrade:upgraded,proof:upgradedProof});
    await call('seed','after-legacy');await call('resume');
    const afterLegacy=await poll(async()=>{const value=await call('inspect');return value.session.turns.find(row=>row.id==='after-legacy')?.state==='completed'?value:undefined;});trace.push({after_legacy:afterLegacy});
    // A tools:['exec_command'] configuration still runs through Code Mode.
    // The command writes successfully, then stalls before its nested receipt.
    // This replaces the obsolete newly-emitted direct function-mode scenario;
    // the pre-journal upgrade boundary remains exercised above.
    await call('functions');await kill();await start();await call('seed','restricted');await call('stage',9);await call('resume');
    const restrictedPending=await poll(async()=>{const value=await call('inspect');return value.session.effects.some(row=>row.parent_call_id==='fixture-restricted'&&row.state==='pending')?value:undefined;});
    await poll(async()=>{try{return (await call('directProof')).text==='restricted-effect\n';}catch{return false;}});trace.push({restricted_pending:restrictedPending});
    await kill();await start();await call('complete');await call('resume');
    const restrictedRecovered=await poll(async()=>{const value=await call('inspect');return value.session.turns.find(row=>row.id==='restricted')?.state==='completed'?value:undefined;});
    assert.ok(restrictedRecovered.session.results.some(row=>row.message.includes('outcome unknown')),'restricted nested unfinished tool must report unknown');
    const restrictedProof=await call('directProof');assert.equal(restrictedProof.text,'restricted-effect\n');trace.push({restricted_recovered:restrictedRecovered,proof:restrictedProof});
    await call('seed','after-restricted');await call('resume');
    const afterRestricted=await poll(async()=>{const value=await call('inspect');return value.session.turns.find(row=>row.id==='after-restricted')?.state==='completed'?value:undefined;});trace.push({after_restricted:afterRestricted});
    await call('seed','child-stage');await call('stage',10);await call('resume');
    const childProof=await poll(async()=>{try{const value=await call('childProof');return value.text==='JOURNAL_CHILD_FIXTURE'?value:undefined;}catch{lastInspection=await call('inspect');return undefined;}});
    const childCompleted=await poll(async()=>{
      const value=await call('inspect');
      const effect=value.session.effects.find(row=>row.call_id==='fixture-child-command/code-1');
      const result=value.session.results.map(row=>JSON.parse(row.message).event.payload).find(row=>row.call_id==='fixture-child-wait/code-1');
      const agent=result?.structured_result?.agents?.[0];
      return value.session.turns.find(row=>row.id==='child-stage')?.state==='completed' && effect?.state==='completed' && agent?.status?.state==='completed' ? value : undefined;
    });
    const childEffect=childCompleted.session.effects.find(row=>row.call_id==='fixture-child-command/code-1');
    assert.match(childEffect.operation_id,/^non-durable:/);assert.equal(childEffect.model_call_index,1);
    trace.push({child_tool_proof:childProof,child_completed:childCompleted});
    await writeFile(output+'/trace.json',JSON.stringify(trace,null,2)+'\n');console.log(JSON.stringify({evidence:output,attempts:3,terminal:'failed/outcome unknown',next:'completed',unique_forward_effects:7,legacy_upgrade_effects:1,legacy_duplicate_effects:0,restricted_code_effects:1,restricted_duplicate_effects:0,child_effects:1,child_status:'completed/CHILD_OK',model_requests:childCompleted.model.requests}));
  }finally{await writeFile(output+'/trace.json',JSON.stringify({trace,lastInspection},null,2)+'\n');if(child)await kill();}
});
