import { test } from 'node:test';
import assert from 'node:assert/strict';
import { summarizeProviderObservations, providerObservationKey } from '../src/provider-telemetry.ts';
import { runProviderProbes } from '../src/provider-probes.ts';
const sample = (timestamp, fullResponseMs, outcome = 'success') => ({ timestamp, source:'live',workerColo:'LHR',clientIngressColo:'ATH',backend:'openrouter',model:'test/model',effort:null,outcome,status:200,headersMs:1,fullResponseMs,generationTtftMs:null,clientDeliveryMs:null,elapsedMs:fullResponseMs??50 });
test('censors failures, excludes stale/future data and requires sufficient successes', () => {
 const result=summarizeProviderObservations([sample(1,999),sample(99,10),sample(100,30),sample(100,null,'timeout'),sample(200,1)],100,{windowMs:10,minimumSamples:3,alpha:.5});
 assert.equal(result.sampleCount,3); assert.equal(result.censoredCount,1); assert.equal(result.fullResponseP50Ms,20); assert.equal(result.fullResponseEwmaMs,20); assert.equal(result.usable,false);
 assert.notEqual(providerObservationKey(sample(100,10)),providerObservationKey({...sample(100,10),workerColo:'SFO'}));
});
test('disabled probes neither fetch nor reserve',async()=>{
 assert.equal(await runProviderProbes({enabled:false,targets:[],dailyRequestLimit:1,workerColo:null,store:{reserveProbe(){throw Error();},append(){throw Error();}},fetch(){throw Error();}}),0);
});
test('daily reservation precedes fetch and observations do not include content or credentials',async()=>{
 let clock=1000,reservations=0,calls=0; const observations=[];
 const count=await runProviderProbes({enabled:true,dailyRequestLimit:1,workerColo:'LHR',now:()=>clock,targets:[{backend:'openrouter',model:'test/model',key:'secret'},{backend:'vercel',model:'test/model',key:'secret'}],store:{reserveProbe(){return ++reservations<=1;},append(x){observations.push(x);}},fetch:async(url,init)=>{calls++;assert.equal(reservations,1);assert.equal(url,'https://openrouter.ai/api/v1/chat/completions');assert.equal(init.redirect,'error');clock+=5;return new Response(new ReadableStream({pull(c){clock+=15;c.enqueue(new TextEncoder().encode('OK'));c.close();}}));}});
 assert.equal(count,1);assert.equal(calls,1);assert.equal(observations[0].generationTtftMs,null);assert.equal(observations[0].clientDeliveryMs,null);assert.equal(observations[0].clientIngressColo,null);assert.equal(observations[0].outcome,'success');assert.ok(observations[0].fullResponseMs>=observations[0].headersMs);assert.doesNotMatch(JSON.stringify(observations),/secret|Reply OK/);
});
test('HTTP errors remain censored, timeout and unknown geography explicit',async()=>{
 for(const timeout of [false,true]){
 const observations=[];
 await runProviderProbes({enabled:true,dailyRequestLimit:1,workerColo:null,timeoutMs:1,now:()=>1000,targets:[{backend:'vercel',model:'test/model',key:'secret'}],store:{reserveProbe:()=>true,append:x=>{observations.push(x);}},fetch:async(_,init)=>timeout?await new Promise((_,reject)=>init.signal.addEventListener('abort',()=>reject(Error('private')))):new Response('private error',{status:429})});
 assert.equal(observations[0].outcome,timeout?'timeout':'http_error');assert.equal(observations[0].fullResponseMs,null);assert.equal(observations[0].workerColo,null);assert.doesNotMatch(JSON.stringify(observations),/private/);
 }
});
test('arbitrary backends and invalid limits cannot issue requests',async()=>{
 for(const dailyRequestLimit of [0,101,NaN,1]){
 assert.equal(await runProviderProbes({enabled:true,dailyRequestLimit,workerColo:null,targets:[{backend:'https://evil.example',model:'test/model',key:'secret'}],store:{reserveProbe(){throw Error();},append(){throw Error();}},fetch(){throw Error();}}),0);
 }
});
test('SQLite store atomically caps durable budget, bounds retained data and projects fields',async()=>{
 const { DatabaseSync }=await import('node:sqlite');
 const { SqliteProviderTelemetryStore }=await import('../src/provider-telemetry.ts');
 const db=new DatabaseSync(':memory:');
 const sql={exec(query,...bindings){const stmt=db.prepare(query);return stmt.columns().length?stmt.all(...bindings):(stmt.run(...bindings),[]);}};
 const store=new SqliteProviderTelemetryStore(sql);
 assert.equal(store.reserveProbe('2026-09-20',2),true);assert.equal(store.reserveProbe('2026-09-20',2),true);assert.equal(new SqliteProviderTelemetryStore(sql).reserveProbe('2026-09-20',2),false);
 assert.equal(store.reserveProbe('2026-09-21',2),true);
 for(let i=0;i<515;i++)store.append({...sample(i,10),prompt:'private',key:'secret'});
 const rows=store.read();assert.equal(rows.length,512);assert.equal(rows[0].timestamp,3);assert.doesNotMatch(JSON.stringify(rows),/private|secret/);db.close();
});
test('live observer measures headers/body separately using monotonic time and persists once',async()=>{
 const {beginLiveProviderObservation}=await import('../src/provider-telemetry.ts');
 let mono=10;const samples=[];
 const observer=beginLiveProviderObservation(sample(1,2),{append:x=>{samples.push(x);}},{wallNow:()=>1000,monotonicNow:()=>mono});
 mono=20;observer.headers(200);mono=40;assert.equal(await observer.finish('success'),true);
 assert.equal(await observer.finish('success'),false);
 assert.equal(samples.length,1);assert.equal(samples[0].headersMs,10);assert.equal(samples[0].fullResponseMs,30);assert.equal(samples[0].timestamp,1000);assert.equal(samples[0].generationTtftMs,null);assert.equal(samples[0].clientDeliveryMs,null);
 const failed=beginLiveProviderObservation(sample(1,2),{append(){throw Error('storage unavailable');}});
 failed.headers(200);assert.equal(await failed.finish('success'),false);
});
