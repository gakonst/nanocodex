// Safe metadata only: never retain credentials, prompt echoes, or generated text.
import {projection} from '../regional-inference-bench/core.mjs';
export async function measure({url,key,body,fetchImpl=fetch,timeoutMs=90000}) {
 const started=performance.now(),clock=()=>performance.now()-started;
 const r={timestamp:new Date().toISOString(),http_status:null,headers_ms:null,first_body_ms:null,first_text_ms:null,first_reasoning_ms:null,first_delta_ms:null,terminal_ms:null,total_ms:null,terminal:false,error:null,events:[],chunks:[],deltas:[],body_bytes:0};
 let terminal,chunkIndex=-1,terminalChunk=null;
 try {
  const response=await fetchImpl(url,{method:'POST',headers:{authorization:`Bearer ${key}`,'content-type':'application/json'},body:JSON.stringify(body),redirect:'manual',signal:AbortSignal.timeout(timeoutMs)});
  r.http_status=response.status;r.headers_ms=clock();r.headers=Object.fromEntries(['content-type','cf-ray','cf-placement','x-nanocodex-provider','x-nanocodex-model','x-nanocodex-thinking','x-nanocodex-inference-buffering','x-nanocodex-ingress-colo'].map(k=>[k,response.headers.get(k)]));
  const sse=(response.headers.get('content-type')??'').includes('text/event-stream');r.transport=sse?'sse':'json';
  const decoder=new TextDecoder();let pending='';
  const event=frame=>{
   const data=frame.split('\n').filter(x=>x.startsWith('data:')).map(x=>x.slice(5).trimStart()).join('\n');if(!data||data==='[DONE]')return;
   let e;try{e=JSON.parse(data);}catch{r.error??='invalid_sse_json';return;}
   const at_ms=clock(),type=typeof e.type==='string'?e.type:'unknown';r.events.push({type,at_ms,chunk_index:chunkIndex});
   const kind=type==='response.output_text.delta'?'text':/^response\.reasoning(?:_text|_summary_text|\.text)?\.delta$/.test(type)?'reasoning':null;
   if(kind&&typeof e.delta==='string'&&/\S/.test(e.delta)){r[`first_${kind}_ms`]??=at_ms;r.first_delta_ms??=at_ms;r.deltas.push({kind,at_ms,chunk_index:chunkIndex,chars:e.delta.length,before_terminal:!r.terminal});}
   if(['response.completed','response.incomplete','response.failed'].includes(type)){terminal=e.response;r.terminal=true;r.terminal_ms=at_ms;terminalChunk=chunkIndex;}
   if(type==='error'||type==='response.failed')r.error??='provider_error';
  };
  for await(const chunk of response.body){chunkIndex++;r.first_body_ms??=clock();r.body_bytes+=chunk.length;r.chunks.push({at_ms:clock(),bytes:chunk.length});if(r.body_bytes>4*1024*1024)throw Error('body_limit');pending+=decoder.decode(chunk,{stream:true});if(sse){pending=pending.replace(/\r\n/g,'\n');let b;while((b=pending.indexOf('\n\n'))>=0){event(pending.slice(0,b));pending=pending.slice(b+2);}}}
  pending+=decoder.decode();if(sse){if(pending.trim())event(pending);}else{try{terminal=JSON.parse(pending);}catch{r.error??='invalid_json';}r.terminal=['completed','incomplete','failed'].includes(terminal?.status);if(r.terminal)r.terminal_ms=clock();}
  Object.assign(r,projection(terminal));if(terminal?.route?.diagnostics)r.route_diagnostics=terminal.route.diagnostics;if(!response.ok)r.error??=`http_${response.status}`;if(response.ok&&!r.terminal)r.error??='missing_terminal';
 }catch(e){r.error=['TimeoutError','AbortError'].includes(e?.name)?'timeout_outcome_unknown':'transport_or_protocol_error';r.exception_name=e?.name??null;}
 r.total_ms=clock();r.delta_count=r.deltas.length;r.deltas_before_terminal=r.deltas.filter(d=>d.before_terminal).length;r.deltas_in_earlier_chunk_than_terminal=terminalChunk===null?null:r.deltas.filter(d=>d.chunk_index<terminalChunk).length;r.first_delta_to_terminal_ms=r.first_delta_ms===null||r.terminal_ms===null?null:r.terminal_ms-r.first_delta_ms;return r;
}
