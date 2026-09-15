// Real deployed managed Worker versus hosted Agents; credentials stay in environment.
import {writeFileSync,mkdirSync,appendFileSync,existsSync} from 'node:fs';
import {resolve,dirname} from 'node:path';
import {Agent} from '../managed/index.mjs';
import {workloads,instructions,validate} from './agents-workloads.mjs';
const args=Object.fromEntries(process.argv.slice(2).map(a=>a.replace(/^--/,'').split('=')));
const choices=(key,all)=>args[key]?args[key].split(','):all;
const models=choices('models',['gpt-5.6-luna','gpt-5.6-terra','gpt-5.6-sol','gpt-6-astra']);
const efforts=choices('efforts',['low','medium','high']);const tiers=choices('tiers',['default','fast']);
const tasks=workloads.filter(w=>choices('workloads',workloads.map(w=>w.id)).includes(w.id));
const paths=choices('paths',['nanocodex_cloudflare','openai_agents']);
const repeats=Number(args.repetitions??3),turns=Number(args.turns??1),concurrency=Number(args.concurrency??1);
if(!args.output||!process.env.OPENAI_API_KEY||!process.env.NANOCODEX_API_KEY||!process.env.NANOCODEX_MANAGED_URL)throw Error('output and credential environment required');
for(const [selected,allowed] of [[models,['gpt-5.6-luna','gpt-5.6-terra','gpt-5.6-sol','gpt-6-astra']],[efforts,['low','medium','high','xhigh','max']],[tiers,['default','fast']],[paths,['nanocodex_cloudflare','openai_agents','responses_http']]])if(selected.some(x=>!allowed.includes(x))||new Set(selected).size!==selected.length)throw Error('invalid matrix selection');
if(args.delegation!==undefined&&!['default','disabled','enabled'].includes(args.delegation))throw Error('invalid delegation setting');
if(!tasks.length||![repeats,turns,concurrency].every(Number.isInteger)||repeats<1||repeats>10||turns<1||turns>2||concurrency<1||concurrency>4)throw Error('invalid bounded run');
const destination=resolve(args.output);if(existsSync(destination)||existsSync(destination+'.sessions.jsonl'))throw Error('Output already exists; choose a new filename to preserve prior observations');mkdirSync(dirname(destination),{recursive:true});
if(args.resume)throw Error('Resume is not supported: use a new output filename');
const report={version:2,seed:20260911,delegation:args.delegation??'default',started_at:new Date().toISOString(),label:args.label??'baseline',managed_url:process.env.NANOCODEX_MANAGED_URL,managed_version:args.version??null,models,efforts,tiers,repetitions:repeats,turns,concurrency,instructions,workloads:tasks,records:[]};
const secret=[process.env.OPENAI_API_KEY,process.env.NANOCODEX_API_KEY];
const safe=e=>secret.reduce((s,k)=>s.replaceAll(k,'[REDACTED]'),String(e?.message??e)).slice(0,1000);
const save=()=>{const start=performance.now();writeFileSync(destination,JSON.stringify(report,(k,v)=>['encrypted_content','obfuscation'].includes(k)?'[omitted]':v,2)+'\n');report.max_checkpoint_ms=Math.max(report.max_checkpoint_ms??0,performance.now()-start);};
const journal=row=>{const start=performance.now();appendFileSync(destination+'.sessions.jsonl',JSON.stringify({path:row.path,session_id:row.session_id,started_at:row.started_at})+'\n');row.journal_ms=performance.now()-start;};
const pause=ms=>new Promise(r=>setTimeout(r,ms));
const headers={'Authorization':'Bearer '+process.env.OPENAI_API_KEY,'OpenAI-Beta':'agents=v1','Content-Type':'application/json'};
async function oai(path,method='GET',body,signal=AbortSignal.timeout(20000)){
 const response=await fetch('https://api.openai.com/v1/agents/sessions'+path,{method,headers,signal,...(body?{body:JSON.stringify(body)}:{})});
 if(!response.ok){let info;try{info=await response.json();}catch{};throw Error(`OpenAI HTTP ${response.status}: ${info?.error?.message??'request failed'}`);}
 return response;
}
async function cleanup(id){
 for(let attempt=0;attempt<12;attempt++){
 const response=await fetch('https://api.openai.com/v1/agents/sessions/'+id,{method:'DELETE',headers,signal:AbortSignal.timeout(20000)});
 await response.body?.cancel();if([200,204,404].includes(response.status))return response.status;
 if(response.status!==409)throw Error('OpenAI session deletion HTTP '+response.status);
 await oai('/'+id+'/events','POST',{events:[{type:'agent.session.input.cancel'}]}).then(r=>r.body?.cancel());await pause(Math.min(5000,1000*(attempt+1)));
 }throw Error('OpenAI cleanup still busy');
}
async function consume(response,observe){
 const reader=response.body.getReader(),decoder=new TextDecoder();let pending='';
 try{for(;;){const part=await reader.read();if(part.done)throw Error('stream closed before terminal');pending+=decoder.decode(part.value,{stream:true});
 let match;while((match=/\r?\n\r?\n/.exec(pending))){const frame=pending.slice(0,match.index);pending=pending.slice(match.index+match[0].length);const data=frame.split(/\r?\n/).filter(l=>l.startsWith('data:')).map(l=>l.slice(5).trimStart()).join('\n');if(data&&data!=='[DONE]'&&observe(JSON.parse(data)))return;}
 }}finally{await reader.cancel().catch(()=>{});}
}
function textDelta(row,text,elapsed){if(text){row.first_delta_characters??=text.length;row.ttft_ms??=elapsed;row.last_text_ms=elapsed;row.text+=text;row.text_deltas++;}}
function aggregateUsage(calls){
 const unique=[...new Map(calls.filter(c=>c.response_id).map(c=>[c.response_id,c])).values()];
 if(!unique.length||unique.some(c=>!c.usage))return undefined;
 const sum=path=>{const values=unique.map(c=>path.reduce((v,k)=>v?.[k],c.usage));return values.every(Number.isFinite)?values.reduce((a,b)=>a+b,0):undefined;};
 return {input_tokens:sum(['input_tokens']),output_tokens:sum(['output_tokens']),total_tokens:sum(['total_tokens']),input_tokens_details:{cached_tokens:sum(['input_tokens_details','cached_tokens']),cache_write_tokens:sum(['input_tokens_details','cache_write_tokens'])},output_tokens_details:{reasoning_tokens:sum(['output_tokens_details','reasoning_tokens'])}};
}
async function runJob(job){
 let managed,sid,lastCursor='0',currentRow,currentElapsed;const owned=[];
 try{for(let turnIndex=0;turnIndex<turns;turnIndex++){
 const task=tasks.find(w=>w.id===job.workload);const row={...job,turn_index:turnIndex,state:turnIndex?'warm':'fresh',started_at:new Date().toISOString(),text:'',text_deltas:0,ttft_ms:null,events:[],http:[]};report.records.push(row);owned.push(row);save();const start=performance.now();
 const elapsed=()=>performance.now()-start;currentRow=row;currentElapsed=elapsed;
 try{
 if(job.path==='nanocodex_cloudflare'){
 const fetcher=async(input,init)=>{const activeRow=currentRow,clock=currentElapsed;const at=clock();const response=await fetch(input,init);activeRow.http.push({path:new URL(input instanceof Request?input.url:input).pathname,method:init?.method??'GET',start_ms:at,headers_ms:clock()-at,status:response.status,cf_ray:response.headers.get('cf-ray')});return response;};
 if(!managed){managed=await Agent.create({baseUrl:process.env.NANOCODEX_MANAGED_URL,apiKey:process.env.NANOCODEX_API_KEY,fetch:fetcher,settings:{model:job.model,thinking:job.effort,reasoningMode:'standard',fastMode:job.tier==='fast'},...(args.production?{}:{configuration:{instructions,tools:[],...(args.delegation==='disabled'?{multi_agent:{enabled:false}}:args.delegation==='enabled'?{multi_agent:{enabled:true}}:{}),environment:{network:{access:'disabled'}}}})});row.create_ms=elapsed();}
 row.session_id=managed.id;if(turnIndex===0)journal(row);const stop=new AbortController();let turn;
 const watcher=(async()=>{for await(const event of managed.events.watch({cursor:lastCursor,signal:stop.signal})){
  const at=elapsed();row.first_event_ms??=at;const e=event.data.event;row.events.push(e?.type?.endsWith('.delta')?{elapsed_ms:at,type:e.type,cursor:event.cursor,agent_id:event.data.agent_id,characters:e.payload?.text?.length??0}:{elapsed_ms:at,type:event.type,cursor:event.cursor,data:event.data});
  if(event.type==='turn_accepted')row.accepted_ms??=at;
  if(event.data.agent_id===undefined&&e?.type==='run.started')row.run_started_ms??=at;
  if(event.data.agent_id===undefined&&e?.type==='model.call.started')row.model_started_ms??=at;
  if(event.data.agent_id===undefined&&e?.type==='assistant.delta'&&e.payload?.text)textDelta(row,e.payload.text,at);
  if(e?.type==='model.call.completed'){row.model_calls??=[];row.model_calls.push(e.payload);}
  if(event.data.agent_id===undefined&&e?.type==='run.completed')row.run=e.payload;
 }})().catch(error=>{if(!stop.signal.aborted)row.stream_error=safe(error);});
 try{turn=managed.turn.prompt({input:task.prompt,idempotencyKey:crypto.randomUUID()});const result=await turn.result({signal:AbortSignal.timeout(300000)});row.completion_ms=elapsed();row.final_text=result.finalMessage;row.turn_usage=result.usage;row.turn_id=result.turnId;row.terminal='completed';row.last_cursor=result.cursor;lastCursor=result.cursor??row.events.at(-1)?.cursor??lastCursor;
 row.usage=aggregateUsage(row.model_calls??[]);
 }catch(e){await turn?.cancel().catch(()=>{});throw e;}finally{stop.abort();await watcher;}
 try{const calls=[];let after;for(;;){const page=await managed.requests({after});calls.push(...page.data.filter(c=>c.turn_id===row.turn_id).map(c=>({...c.payload,response_id:c.id})));if(!page.has_more)break;after=page.data.at(-1)?.cursor;if(!after)throw Error('Missing usage pagination cursor');}row.model_calls=calls;row.usage=aggregateUsage(calls);}catch(error){row.metadata_error=safe(error);}

 }else if(job.path==='responses_http'){
 const response=await fetch('https://api.openai.com/v1/responses',{method:'POST',headers,signal:AbortSignal.timeout(300000),body:JSON.stringify({model:job.model,instructions,input:task.prompt,reasoning:{effort:job.effort},service_tier:job.tier,store:false,stream:true})});
 if(!response.ok){const error=await response.json();throw Error(`Responses HTTP ${response.status}: ${error?.error?.message??'failed'}`);}
 row.headers_ms=elapsed();row.request_id=response.headers.get('x-request-id');
 await consume(response,e=>{const at=elapsed();row.first_event_ms??=at;row.events.push(e.type?.endsWith('.delta')?{elapsed_ms:at,type:e.type,turn_id:e.turn_id,characters:e.delta?.length??0}:{elapsed_ms:at,data:e});
  if(e.type==='response.output_text.delta')textDelta(row,e.delta,at);
  if(e.response?.service_tier)row.reported_tier=e.response.service_tier;
  if(e.type==='response.completed'){row.completion_ms=at;row.terminal='completed';row.usage=e.response.usage;row.final_text=e.response.output.filter(i=>i.type==='message'&&i.role==='assistant').flatMap(i=>i.content??[]).filter(c=>c.type==='output_text').map(c=>c.text).join('');return true;}
  if(['response.failed','response.incomplete','error'].includes(e.type))throw Error(e.response?.error?.message??e.error?.message??e.type);
  return false;
 });
 }else{
 let response;
 if(!sid)response=await oai('','POST',{agent:{model:job.model,instructions,reasoning:{effort:job.effort},service_tier:job.tier,tools:[]},environment:{type:'none'},input:task.prompt,stream:true},AbortSignal.timeout(300000));
 else{response=await oai('/'+sid+'/events?stream=true','GET',undefined,AbortSignal.timeout(300000));row.stream_connected_ms=elapsed();await oai('/'+sid+'/events','POST',{events:[{type:'agent.session.input.message',input:[{role:'user',content:[{type:'input_text',text:task.prompt}]}]}]}).then(r=>r.body?.cancel());row.submitted_ms=elapsed();}
 row.headers_ms=elapsed();row.request_id=response.headers.get('x-request-id');
 await consume(response,e=>{const at=elapsed();row.first_event_ms??=at;row.events.push(e.type?.endsWith('.delta')?{elapsed_ms:at,type:e.type,turn_id:e.turn_id,characters:e.delta?.length??0}:{elapsed_ms:at,data:e});const id=e.session?.id??e.session_id;if(id&&!sid){sid=id;row.session_id=id;journal(row);}if(sid)row.session_id=sid;
  if(e.type?.endsWith('output_text.delta')&&e.turn_id===row.turn_id)textDelta(row,e.delta,at);
  if(e.turn&&!e.turn.subagent_id){row.turn_id=e.turn.id;if(e.turn.usage)row.usage=e.turn.usage;}
  if(['agent.session.turn.completed','agent.session.turn.failed','agent.session.turn.cancelled'].includes(e.type)&&!e.turn?.subagent_id){row.completion_ms=at;row.terminal=e.type.endsWith('.completed')?'completed':e.type;return true;}
  if(['error','agent.session.failed','agent.session.requires_action'].includes(e.type))throw Error(e.error?.message??e.type);
  return false;
 });
 try{for(const delay of [0,2000,5000]){if(delay)await pause(delay);const data=await(await oai('/'+sid+'/turns?limit=100&order=desc')).json();const turn=data.data?.find(t=>t.id===row.turn_id);if(turn?.usage){row.usage=turn.usage;break;}}
 const items=await(await oai('/'+sid+'/items?limit=100&order=desc&turn_id='+row.turn_id)).json();
 const messages=(items.data??[]).filter(i=>i.type==='message'&&i.role==='assistant');
 const final=messages.find(i=>i.phase==='final_answer')??messages[0];
 row.final_text=final?final.content.filter(c=>['output_text','text'].includes(c.type)).map(c=>c.text).join(''):row.text;
 }catch(error){row.metadata_error=safe(error);row.final_text=row.text;}
 }
 Object.assign(row,validate(task,row.final_text??row.text));row.visible_characters=(row.final_text??row.text).length;row.streaming_ms=row.ttft_ms===null?null:row.last_text_ms-row.ttft_ms;
 }catch(error){row.error=safe(error);row.correct=false;}
 row.wall_ms=elapsed();save();console.log(JSON.stringify({completed:report.records.filter(r=>r.wall_ms).length,model:row.model,effort:row.effort,tier:row.tier,workload:row.workload,path:row.path,state:row.state,ttft_ms:row.ttft_ms,completion_ms:row.completion_ms,correct:row.correct,error:row.error}));
 if(row.error&&/401|quota|authentication|permission/i.test(row.error))throw Error('access failure; stopping matrix');
 if(row.error)break;
 }}finally{
 try{if(managed)await managed.delete();if(sid)await cleanup(sid);for(const row of owned)row.deleted=true;}
 catch(e){for(const row of owned)row.cleanup_error=safe(e);throw e;}finally{save();}
 }
}
let seed=20260911;const random=()=>{seed=(1664525*seed+1013904223)>>>0;return seed/2**32;};const jobs=[];
for(let repetition=1;repetition<=repeats;repetition++){
 const round=models.flatMap(model=>efforts.flatMap(effort=>tiers.flatMap(tier=>tasks.map(w=>({model,effort,tier,workload:w.id,repetition})))));
 for(let i=round.length-1;i>0;i--){const j=Math.floor(random()*(i+1));[round[i],round[j]]=[round[j],round[i]];}
 for(const pair of round)jobs.push(...(random()<.5?paths:[...paths].reverse()).map(path=>({...pair,path})));
}
report.expected_records=jobs.length*turns;save();
try{let next=0,failure;await Promise.all(Array.from({length:concurrency},async()=>{while(!failure&&next<jobs.length){const job=jobs[next++];try{await runJob(job);}catch(error){failure=error;}}}));if(failure)throw failure;report.complete=true;}
finally{report.finished_at=new Date().toISOString();save();}
