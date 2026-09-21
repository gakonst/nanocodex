import test from 'node:test';
import assert from 'node:assert/strict';
import {measure,promptFor,quantile,summarize} from './core.mjs';
const wait=ms=>new Promise(r=>setTimeout(r,ms));
test('first meaningful excludes created, reasoning and empty deltas; parses split UTF8 and CRLF',async()=>{
  const encoder=new TextEncoder();
  const fetchImpl=async()=>new Response(new ReadableStream({async start(controller){
    controller.enqueue(encoder.encode('data: {"type":"response.created","response":{}}\r\n\r\n'));
    await wait(35);
    controller.enqueue(encoder.encode('data: {"type":"response.reasoning_text.delta","delta":"private"}\n\ndata: {"type":"response.output_text.delta","delta":" "}\n\n'));
    await wait(35);
    const bytes=encoder.encode('event: response.output_text.delta\r\ndata: {"type":"response.output_text.delta","delta":"é"}\r\n\r\n');
    for(const byte of bytes)controller.enqueue(Uint8Array.of(byte));
    await wait(20);controller.enqueue(encoder.encode('data: {"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":5,"output_tokens":1}}}\n\n'));controller.close();
  }}),{headers:{'content-type':'text/event-stream'}});
  const r=await measure({url:'https://example.test/responses',key:'test-only',body:{},fetchImpl});
  assert.equal(r.error,null);assert.equal(r.delta_count,1);assert.equal(r.event_count,5);assert.equal(r.first_meaningful_kind,'response.output_text.delta');assert.ok(r.first_meaningful_ms>=60);assert.ok(r.completed_ms>=r.first_meaningful_ms+10);assert.ok(!JSON.stringify(r).includes('private'));
});
test('buffered JSON observes output only after body completes',async()=>{
  const r=await measure({url:'https://example.test',key:'test',body:{},fetchImpl:async()=>Response.json({status:'completed',output_text:'42',usage:{output_tokens:1}},{headers:{'x-nanocodex-inference-buffering':'buffered'}})});
  assert.equal(r.first_meaningful_kind,'buffered_json_output');assert.equal(r.buffering,'buffered');assert.equal(r.delta_count,0);assert.equal(r.terminal,true);
});
test('no text or creation event does not invent meaningful output',async()=>{
  const r=await measure({url:'https://example.test',key:'test',body:{},fetchImpl:async()=>new Response('data: {"type":"response.created"}\n\n',{headers:{'content-type':'text/event-stream'}})});
  assert.equal(r.first_meaningful_ms,null);assert.equal(r.error,'missing_terminal');
});
test('prompt families preserve long prefix and quantiles include sample caution',()=>{
  const a=promptFor('long-prefix','test',0),b=promptFor('long-prefix','test',1);assert.equal(a.slice(0,a.indexOf('\nQuestion')),b.slice(0,b.indexOf('\nQuestion')));assert.notEqual(a,b);assert.ok(a.length>20000);assert.equal(quantile([1,2,3,4],.95),4);assert.equal(quantile([],.5),null);
  const s=summarize([{status:'completed',first_meaningful_ms:12,total_ms:20,error:null}]);assert.equal(s[0].meaningful_p50_ms,12);assert.equal(s[0].p95_caution,'fewer_than_20_samples');
});

import {run} from './run.mjs';
import {compare} from './compare.mjs';
test('dry run enforces budget without credentials and handles decimal reservation',async()=>{
  const p={run_id:'test',models:['auto'],families:['short'],stream_modes:[false,true],arms:[{}],repetitions:3,max_output_tokens:256,max_requests:6,reserve_usd_per_request:.1,max_reserved_usd:.6};
  assert.equal((await run(p)).reserved_usd,.6);
  await assert.rejects(run({...p,max_requests:5}));await assert.rejects(run({...p,max_reserved_usd:undefined}));await assert.rejects(run({...p,stream_modes:['false']}));
});
test('comparison keeps paired deltas and rejects duplicate pairs',()=>{
  const base={runner:'us',model_requested:'candidate',family:'short',arm:'same',stage:'same',status:'completed',error:null};
  const rows=[{...base,pair:0,stream:false,first_meaningful_ms:100},{...base,pair:0,stream:true,first_meaningful_ms:40},{...base,pair:1,stream:false,first_meaningful_ms:200},{...base,pair:1,stream:true,first_meaningful_ms:80}];
  const [r]=compare(rows,'stream','false','true');assert.equal(r.paired_n,2);assert.equal(r.p95_delta_ms,-120);assert.equal(r.paired_delta_p50_ms,-120);assert.throws(()=>compare([...rows,rows[0]],'stream','false','true'));
});

import worker from './worker.mjs';
test('regional runner requires auth/expiry and distinguishes placement from ingress',async()=>{
  assert.equal((await worker.fetch(new Request('https://example.test/evidence'),{})).status,401);
  const request=new Request('https://example.test/evidence',{headers:{authorization:'Bearer synthetic','cf-placement':'remote-FRA'}});
  Object.defineProperty(request,'cf',{value:{colo:'SJC',country:'US'}});
  assert.equal((await worker.fetch(request,{BENCH_TOKEN:'synthetic',EXPIRES_AT:'invalid'})).status,410);
  const r=await worker.fetch(request,{BENCH_TOKEN:'synthetic',EXPIRES_AT:'2099-01-01T00:00:00Z',PLACEMENT_REGION:'aws:eu-central-1'});
  const evidence=await r.json();assert.equal(evidence.cf_placement,'remote-FRA');assert.equal(evidence.ingress_colo,'SJC');assert.equal(evidence.placement_hint,'aws:eu-central-1');
});

test('Worker fetch uses supported manual redirects and never follows an inference redirect',async()=>{
  let options;
  const r=await measure({url:'https://example.test/responses',key:'test',body:{},fetchImpl:async(_url,init)=>{options=init;return new Response('',{status:302,headers:{location:'https://other.test/responses'}});}});
  assert.equal(options.redirect,'manual');assert.equal(r.http_status,302);assert.notEqual(r.error,null);assert.equal(r.first_meaningful_ms,null);
});
