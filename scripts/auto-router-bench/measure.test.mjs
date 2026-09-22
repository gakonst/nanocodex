import {test} from 'node:test';import assert from 'node:assert/strict';import {measure} from './measure.mjs';
const encoder=new TextEncoder(),frame=x=>'data: '+JSON.stringify(x)+'\n\n';
test('reasoning delta is observed in an earlier read than terminal across partial UTF8/SSE frames',async()=>{
 const reasoning=encoder.encode(frame({type:'response.reasoning_text.delta',delta:'π thought'}));
 const fetchImpl=async()=>new Response(new ReadableStream({start(c){c.enqueue(reasoning.slice(0,65));setTimeout(()=>{c.enqueue(reasoning.slice(65));setTimeout(()=>{c.enqueue(encoder.encode(frame({type:'response.output_text.delta',delta:'42'})+frame({type:'response.completed',response:{status:'completed'}})));c.close();},15);},15);}}),{headers:{'content-type':'text/event-stream'}});
 const r=await measure({url:'https://example.test',key:'synthetic',body:{},fetchImpl});assert.equal(r.error,null);assert.equal(r.delta_count,2);assert.equal(r.deltas_in_earlier_chunk_than_terminal,1);assert(r.first_reasoning_ms<r.first_text_ms);assert(r.first_delta_to_terminal_ms>=10);assert(!JSON.stringify(r).includes('π thought'));
});
test('same-chunk synthetic deltas do not prove incremental transport',async()=>{
 const fetchImpl=async()=>new Response(frame({type:'response.output_text.delta',delta:'42'})+frame({type:'response.completed',response:{status:'completed'}}),{headers:{'content-type':'text/event-stream'}});
 const r=await measure({url:'https://example.test',key:'synthetic',body:{},fetchImpl});assert.equal(r.deltas_before_terminal,1);assert.equal(r.deltas_in_earlier_chunk_than_terminal,0);
});
test('incomplete stream and HTTP failures stay failures',async()=>{
 const r=await measure({url:'https://example.test',key:'synthetic',body:{},fetchImpl:async()=>new Response(frame({type:'response.created'}),{headers:{'content-type':'text/event-stream'}})});assert.equal(r.error,'missing_terminal');assert.equal(r.first_delta_ms,null);
 const failure=await measure({url:'https://example.test',key:'synthetic',body:{},fetchImpl:async()=>Response.json({error:{message:'Do not retain error body'}},{status:502})});assert.equal(failure.error,'http_502');assert(!JSON.stringify(failure).includes('Do not retain'));
});
