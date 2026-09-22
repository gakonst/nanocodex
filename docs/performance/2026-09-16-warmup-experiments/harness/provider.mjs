import{readFile,writeFile}from'node:fs/promises';import{randomUUID,createHash}from'node:crypto';import{createRequire}from'node:module';import{loadavg}from'node:os';
const require=createRequire('/Users/georgios/github/gakonst/nanocodex-master/js/managed/package.json'),WS=require('ws');
const auth=JSON.parse(await readFile('/Users/georgios/.codex/auth.json','utf8')).tokens;
if(!auth?.access_token||!auth.account_id)throw Error('Existing subscription credentials unavailable');
const target='wss://chatgpt.com/backend-api/codex/responses',out=new URL((process.env.OUTPUT??'provider')+'.json',import.meta.url),delay=ms=>new Promise(r=>setTimeout(r,ms));
const report={started_at:new Date().toISOString(),model:'gpt-6-astra',effort:'low',tier:'default',endpoint:target,lead_ms:Number(process.env.LEAD_MS??5000),rows:[],credential_source:'existing Codex subscription; credentials omitted'};
const save=()=>writeFile(out,JSON.stringify(report,(k,v)=>['access_token','refresh_token','authorization','obfuscation','encrypted_content'].includes(k)?'[omitted]':typeof v==='string'?v.replaceAll(auth.access_token,'[omitted]'):v,2)+'\n');
function numericTiming(v){if(typeof v==='number'||typeof v==='boolean')return v;if(Array.isArray(v))return v.map(numericTiming);if(v&&typeof v==='object')return Object.fromEntries(Object.entries(v).map(([k,x])=>[k,numericTiming(x)]).filter(([,x])=>x!==undefined));return undefined;}
function socket(row){const start=performance.now(),id=randomUUID(),frames=[];const ws=new WS(target,{headers:{authorization:'Bearer '+auth.access_token,'ChatGPT-Account-ID':auth.account_id,'OpenAI-Beta':'responses_websockets=2026-02-06','x-openai-internal-codex-responses-lite':'true','session-id':id,'thread-id':id,'x-client-request-id':id,'x-responsesapi-include-timing-metrics':'true','user-agent':'nanocodex/0.6.1'},handshakeTimeout:30000});
 ws.on('message',raw=>{try{const e=JSON.parse(raw);frames.push({event:e,at:performance.now()});}catch{}});ws.on('error',()=>{});
 const ready=new Promise((resolve,reject)=>{ws.once('open',()=>{row.socket_ms=performance.now()-start;resolve();});ws.once('error',e=>reject(Error('WebSocket '+e.message)));ws.once('unexpected-response',(_req,r)=>{r.resume();reject(Error('WebSocket HTTP '+r.statusCode));});});
 ws.on('upgrade',r=>{row.upgrade_request_id=r.headers['x-request-id'];row.server_model=r.headers['openai-model'];});return{ws,frames,ready};}
async function response(peer,body,row,kind){const start=performance.now(),after=peer.frames.length;peer.ws.send(JSON.stringify({type:'response.create',...body}));let first,done,text='';const seen=new Set();
 const deadline=start+45000;while(!done){for(let i=after;i<peer.frames.length;i++){if(seen.has(i))continue;seen.add(i);const {event:e,at}=peer.frames[i];
 if(e.type==='error')throw Error('Provider error '+String(e.error?.code??e.code??'unknown')+': '+String(e.error?.message??e.message??'').slice(0,250));
 if(e.type==='response.failed')throw Error('Response failed '+(e.response?.error?.code??'unknown'));
 if(e.type==='response.output_text.delta'&&e.delta){first??=at;text+=e.delta;}
 if(e.type==='response.completed'||e.type==='response.done'){done={e,at};break;}}
 if(done)break;if(peer.ws.readyState!==WS.OPEN)throw Error('Provider socket closed');if(performance.now()>deadline)throw Error('Response timeout');await delay(5);}
 const result={kind,request_ms:done.at-start,first_text_ms:first===undefined?null:first-start,text,response_id:done.e.response?.id,status:done.e.response?.status,usage:done.e.response?.usage,service_tier:done.e.response?.service_tier,provider_timing_events:peer.frames.slice(after).filter(({event:e})=>e.type==='responsesapi.websocket_timing').map(({event:e})=>numericTiming(e)),events:peer.frames.slice(after).map(({event:e,at})=>({type:e.type,at_ms:at-start}))};
 row[kind]=result;if(kind==='warmup'&&text)throw Error('Non-generating warmup unexpectedly produced text');return{...result,firstAt:first,completedAt:done.at};}
const sizes=(process.env.SIZES??'small,large').split(','),arms=(process.env.ARMS??'cold,socket,request').split(','),reps=Number(process.env.REPS??3);
for(const size of sizes){for(let rep=1;rep<=reps;rep++){
 const blockId=randomUUID();const prefix='Fixture '+blockId+'. You are a concise assistant. Follow the user request exactly. Do not call tools.\n'+(size==='large'?'Reference data follows; ignore it unless requested.\n'+Array.from({length:1500},(_,i)=>`Item ${i}: label cedar, value ${10000+i}, group amber.`).join('\n'):'');
 const base={model:report.model,instructions:prefix,tools:[],tool_choice:'auto',parallel_tool_calls:false,stream:true,include:['reasoning.encrypted_content'],store:false,service_tier:'default',reasoning:{effort:'low',context:'all_turns'},text:{verbosity:'low'},prompt_cache_key:'nc-warm-exp-'+blockId};
 for(let n=0;n<arms.length;n++){const arm=arms[(n+rep-1)%arms.length],row={arm,size,rep,block_id:blockId,prefix_bytes:Buffer.byteLength(prefix),prefix_sha256:createHash('sha256').update(prefix).digest('hex'),started_at:new Date().toISOString(),loadavg:loadavg()};report.rows.push(row);await save();let peer;
 const began=performance.now();let prepared,previous;
 const prepare=async()=>{peer=socket(row);await peer.ready;if(arm==='request'){const warm=await response(peer,{...base,input:[],generate:false},row,'warmup');previous=warm.response_id;if(!previous)throw Error('Warmup missing response ID');}row.preparation_ms=performance.now()-began;};
 try{if(arm!=='cold'){prepared=prepare();prepared.catch(()=>{});}await delay(Math.max(0,report.lead_ms-(performance.now()-began)));const submit=performance.now();row.submit_at_ms=submit-began;row.prepared_before_submit=row.preparation_ms!==undefined;if(!prepared)prepared=prepare();await prepared;
 const result=await response(peer,{...base,...(previous?{previous_response_id:previous}:{}),input:[{role:'user',content:[{type:'input_text',text:'Return exactly 42.'}]}]},row,'generation');
 row.submit_to_text_ms=result.firstAt===undefined?null:result.firstAt-submit;row.open_to_text_ms=result.firstAt===undefined?null:result.firstAt-began;row.submit_to_complete_ms=result.completedAt-submit;row.correct=result.text.trim()==='42';if(!row.correct)row.error='Unexpected output';
 }catch(e){row.error=String(e);process.exitCode=1;}finally{if(peer?.ws.readyState===WS.CONNECTING)peer.ws.terminate();else peer?.ws.close();await save();console.log(JSON.stringify({arm,size,rep,prepare_ms:row.preparation_ms,submit_to_text_ms:row.submit_to_text_ms,open_to_text_ms:row.open_to_text_ms,warmup_ms:row.warmup?.request_ms,correct:row.correct,error:row.error}));}
 if(row.error&&(row.error.includes('HTTP 401')||row.error.includes('HTTP 403')))throw Error('Subscription authorization failed; stopping cohort');
 }} }
report.finished_at=new Date().toISOString();await save();
