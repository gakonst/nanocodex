import test from 'node:test';
import assert from 'node:assert/strict';
import {makeCalibration,assertFresh,CANDIDATES,REGIONS,POLICY,parseCase} from './jev-core.mjs';
import {resolveThreadRoute,routingPolicySchema,taskFamily} from '../../js/managed/src/thread-model-routing.ts';
import worker,{JevBudget} from './jev-worker.mjs';
const now=Date.now(),runners=Object.entries(REGIONS).map(([region],i)=>({region,id:`runner${i}`}));
const rows=runners.flatMap((r,ri)=>CANDIDATES.flatMap((model,mi)=>Array.from({length:3},(_,pair)=>({runner:r.id,pair,model_requested:model,route:{backend:model.split(':')[0],model:'gpt-5.6-luna',thinking:'low'},family:pair===1?'long-prefix':'short',headers:{'cf-ray':`abc-${REGIONS[r.region]}`},stage:'streaming',split:'calibration',buffering:'streaming',status:'completed',http_status:200,timestamp:new Date(now-1000).toISOString(),first_meaningful_ms:100+ri*30+mi*4+pair,total_ms:200+ri*30+mi*4+pair}))));
const calibration=makeCalibration(rows,runners,now);
const response=(confidence=.9)=>({answers:{candidate:{choice:CANDIDATES[0],confidence,probabilities:Object.fromEntries(CANDIDATES.map((c,i)=>[c,i===0?.8:.1]))},family:{choice:'mathematics',confidence:.9,probabilities:Object.fromEntries(taskFamily.options.map(f=>[f,f==='mathematics'?1:0]))}}});
test('calibration uses only pairs0-2, same frozen data, three regional and nine global samples',()=>{
 assert.deepEqual(makeCalibration([...rows,{pair:3,secret:'not_calibration'},{pair:4}],runners,now),calibration);
 assert.equal(calibration.metrics.length,12);assert.equal(calibration.metrics.filter(m=>m.scope==='deployment_global').length,3);
 for(const m of calibration.metrics)assert.equal(m.generationTtftSampleCount,m.scope==='deployment_global'?9:3);
 assertFresh(calibration,now);assert.throws(()=>assertFresh(calibration,now+3*3600e3));
 assert.throws(()=>makeCalibration([...rows.slice(1),rows[1]],runners,now));
 assert.throws(()=>makeCalibration(rows.map((r,i)=>i? r:{...r,headers:{'cf-ray':'abc-SJC'}}),runners,now));
});
test('actual resolver accepts same dataset and selects origin-matched or global only',async()=>{
 const saved=JSON.stringify(calibration),states=[];
 for(const origin of [null,'IAD']){
  const route=await resolveThreadRoute({run:async(model,payload)=>{assert.equal(model,'typesafe/jev');states.push(JSON.parse(payload.state));return response();}},'Solve a short probability problem.',routingPolicySchema.parse(POLICY),{openrouter:true,vercel:true,cloudflare:true,workerColo:null,clientIngressColo:origin,provider_performance:calibration.metrics});
  assert.equal(route.audit.provider_telemetry.provider_performance.length,3);
  for(const m of route.audit.provider_telemetry.provider_performance){assert.equal(m.scope,origin?'client_ingress':'deployment_global');assert.equal(m.ttftUsable,true);assert.equal(m.generationTtftSampleCount,origin?3:9);}
 }
 assert.equal(JSON.stringify(calibration),saved);assert.deepEqual(states[0].preferences,states[1].preferences);assert.deepEqual(states[0].policy,states[1].policy);
});
test('caller cannot inject origin, prompt, candidate or calibration',()=>{
 for(const extra of ['origin','prompt','model','provider_performance'])assert.throws(()=>parseCase({arm:'regional',prompt_index:0,[extra]:'x'}));
 assert.throws(()=>parseCase({arm:'regional',prompt_index:3}));
});
test('durable reservation survives recreation and caps each fixed case once',async()=>{
 const data=new Map(),storage={transaction:async fn=>fn({get:async k=>data.get(k),put:async(k,v)=>data.set(k,v)})};
 const req=()=>new Request('https://budget/',{method:'POST',body:JSON.stringify({id:'0:regional'})});
 assert.equal((await new JevBudget({storage}).fetch(req())).status,200);
 assert.equal((await new JevBudget({storage}).fetch(req())).status,409);
});
test('Worker calls actual resolver and timing, retains low-confidence fallback, rejects spoof and duplicate',async()=>{
 const original=globalThis.fetch;let calls=0;const reserved=new Set();
 const env={BENCH_TOKEN:'synthetic-test-token',INFERENCE_KEY:'synthetic-key',EXPIRES_AT:new Date(now+1e6).toISOString(),RUNNER_ID:'test',PLACEMENT_REGION:'aws:us-east-1',EXPECTED_API_INGRESS:'IAD',CALIBRATION_SHA256:'test-hash',CALIBRATION_JSON:JSON.stringify(calibration),TARGET_URL:'https://example.test/v1/responses',AI:{run:async()=>{calls++;return response(.6);}},JEV_BUDGET:{idFromName:()=>'',get:()=>({fetch:async(_url,init)=>{const b=JSON.parse(init.body);if(b.receipt)return new Response('{}');if(reserved.has(b.id))return new Response('{}',{status:409});reserved.add(b.id);return new Response('{}');}})}};
 globalThis.fetch=async(_url,init)=>{
  if(init.method==='HEAD')return new Response(null,{status:405,headers:{'cf-ray':'abc-IAD'}});
  const body=JSON.parse(init.body);assert.equal(body.model,CANDIDATES[0]);assert.equal(body.store,false);
  return new Response('data: '+JSON.stringify({type:'response.output_text.delta',delta:'answer'})+'\n\ndata: '+JSON.stringify({type:'response.completed',response:{status:'completed',model:'gpt-5.6-luna',route:{backend:'cloudflare',model:'gpt-5.6-luna',thinking:'low'}}})+'\n\n',{headers:{'content-type':'text/event-stream','cf-ray':'abc-IAD','x-nanocodex-ingress-colo':'IAD','x-nanocodex-inference-buffering':'streaming'}});
 };
 const request=b=>new Request('https://runner/run',{method:'POST',headers:{authorization:'Bearer synthetic-test-token'},body:JSON.stringify(b)});
 try{
  const r=await worker.fetch(request({arm:'regional',prompt_index:0}),env);assert.equal(r.status,200);const body=await r.json();
  assert.equal(body.classification.attempts,1);assert.equal(body.route.selection,'fallback');assert.equal(body.diagnostics.confidence_status,'low');assert.deepEqual(body.diagnostics.candidate_probabilities,response().answers.candidate.probabilities);assert.equal(body.origin_validated,true);assert.ok(body.experiment_first_meaningful_ms>=body.downstream.first_meaningful_ms);assert.ok(body.preflight_ms>=0);
  assert.equal((await worker.fetch(request({arm:'regional',prompt_index:0}),env)).status,409);assert.equal(calls,1);
  assert.equal((await worker.fetch(request({arm:'global',prompt_index:0,origin:'NRT'}),env)).status,400);
 }finally{globalThis.fetch=original;}
});
