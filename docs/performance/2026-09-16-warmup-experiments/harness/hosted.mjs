import {readFile,writeFile} from 'node:fs/promises';
import {loadavg} from 'node:os';
import {randomUUID} from 'node:crypto';
import {createRequire} from 'node:module';
import {withManagedAccess} from '/Users/georgios/github/gakonst/nanocodex-master/js/nanocodex/managed/Access.mjs';
import {deployments} from './receipts.mjs';
const require=createRequire('/Users/georgios/github/gakonst/nanocodex-master/js/managed/package.json');
const WS=require('ws');
const credentials=JSON.parse(await readFile('/Users/georgios/Library/Application Support/Nanocodex/Native/credentials.json','utf8'));
const base=credentials.baseUrl.replace(/\/$/,'');
if(base!=='https://nanocodex.gakonst.workers.dev')throw Error('Unexpected origin');
let wrapped=withManagedAccess(fetch),out=new URL('./',import.meta.url),phase=process.argv[2]??'measured';
const report={phase,started_at:new Date().toISOString(),model:'gpt-6-astra',settings:{model:'gpt-6-astra',thinking:'low',reasoning_mode:'standard',fast_mode:false},requests:[],http_attempts:[],journeys:[],resources:[],versions_start:await deployments()};
function redact(k,v){if(['encrypted_content','obfuscation','websocket_url','authorization','access_token','refresh_token','apiKey','arguments','encrypted_content'].includes(k))return '[omitted]';return typeof v==='string'?v.replaceAll(credentials.apiKey,'[REDACTED]'):v;}
const save=()=>writeFile(new URL(phase+'.json',out),JSON.stringify(report,redact,2)+'\n');
function freshTransport(){return withManagedAccess(async(input,init)=>{const req=new Request(input,init),began=performance.now();const r=await fetch(req);report.http_attempts.push({path:new URL(req.url).pathname,method:req.method,used_access_snapshot:req.headers.has('x-nanocodex-access'),headers_ms:performance.now()-began,status:r.status,request_id:r.headers.get('x-nanocodex-request-id')});return r;});}
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function request(path,method='GET',body,label=path,live=false){const began=performance.now(),at=Date.now();const response=await(live?fetch:wrapped)(base+path,{method,headers:{authorization:'Bearer '+credentials.apiKey,'content-type':'application/json'},...(body===undefined?{}:{body:JSON.stringify(body)}),signal:AbortSignal.timeout(60000)});const row={label,method,path,at,headers_ms:performance.now()-began,status:response.status,request_id:response.headers.get('x-nanocodex-request-id'),server_timing:response.headers.get('server-timing')};const text=await response.text();row.complete_ms=performance.now()-began;report.requests.push(row);await save();if(!response.ok)throw Error(`${label}: HTTP ${response.status}: ${text.slice(0,160)}`);return{text:text?JSON.parse(text):null,metrics:row};}
async function connect(id,cursor='latest') {const at=Date.now(),began=performance.now(),frames=[];const socket=new WS(base.replace('https:','wss:')+`/v1/agents/${id}/ws?cursor=${cursor}`,{headers:{authorization:'Bearer '+credentials.apiKey},handshakeTimeout:30000});await new Promise((resolve,reject)=>{const timeout=setTimeout(()=>{socket.terminate();reject(Error('WS timeout'));},30000);socket.on('error',reject);socket.on('message',raw=>{const event=JSON.parse(raw);frames.push({...event,received_at:Date.now(),received_mono:performance.now()});if(event.type==='ready'){clearTimeout(timeout);resolve();}});});report.requests.push({label:'events.websocket.ready',path:`/v1/agents/${id}/ws`,at,complete_ms:performance.now()-began,status:101});return{socket,frames,ready_ms:performance.now()-began};}
async function history(id){const data=[];let after='0';for(let n=0;n<30;n++){const {text}=await request(`/v1/agents/${id}/events/history?after=${after}&limit=256`,'GET',undefined,'events.history');data.push(...text.data);if(!text.has_more)return data;after=text.data.at(-1)?.cursor;}throw Error('History page bound');}
const arms=[{id:'events_only_5s',idle:5000,warm:false},{id:'runtime_0s',idle:0,warm:true},{id:'runtime_1s',idle:1000,warm:true},{id:'runtime_5s',idle:5000,warm:true}];
const tasks=Array.from({length:Number(process.env.REPS??3)},(_,i)=>arms.map((_,n)=>({...arms[(n+i)%arms.length],sample:i+1,prompt:'Return exactly 42. Do not call any tools.'}))).flat().filter(t=>!process.env.ARMS||process.env.ARMS.split(',').includes(t.id));
function prepareRuntime(id,resource){
 const began=performance.now();const socket=new WS(base.replace('https:','wss:')+`/v1/agents/${id}/tool-host`,{headers:{authorization:'Bearer '+credentials.apiKey},handshakeTimeout:30000});
 resource.prepare={started_at:Date.now(),event_types:[]};socket.on('open',()=>{resource.prepare.open_ms=performance.now()-began;resource.prepare.opened_at=Date.now();});
 socket.on('message',raw=>{try{resource.prepare.event_types.push(JSON.parse(raw).type);}catch{}});
 socket.on('error',e=>{resource.prepare.error=e.message;});socket.on('close',(code)=>{resource.prepare.close_code=code;resource.prepare.closed_ms=performance.now()-began;});return socket;
}
async function turn(agent,stream,input,task,sample,index){const id='audit-'+randomUUID(),began=Date.now(),beganMono=performance.now();const accepted=await request(`/v1/agents/${agent}/turns`,'POST',{id,input},'turns.create');const record={loadavg:loadavg(),task,sample,turn:index,agent,id,began,http_accept_ms:accepted.metrics.complete_ms,admission_request_id:accepted.metrics.request_id};report.journeys.push(record);await save();let terminal;const deadline=began+240000;while(!(terminal=stream.frames.find(e=>e.turn_id===id&&['turn_completed','turn_failed','turn_cancelled'].includes(e.type)))){if(Date.now()>deadline)throw Error('Turn timeout '+id);if(stream.socket.readyState!==WS.OPEN)throw Error('Stream disconnected');await sleep(25);}const events=(await history(agent)).filter(e=>e.turn_id===id);record.events=events;record.frames=stream.frames.filter(e=>e.turn_id===id);record.terminal=terminal.type;record.completion_ms=terminal.received_mono-beganMono;record.first_text_ms=record.frames.find(e=>e.event?.type==='assistant.delta'&&e.event.payload?.text)?.received_mono-beganMono;record.first_final_text_ms=record.frames.find(e=>e.event?.type==='assistant.delta'&&e.event.payload?.phase==='final_answer')?.received_mono-beganMono;record.answer=record.frames.filter(e=>e.event?.type==='assistant.delta').map(e=>e.event.payload.text).join('');record.model_calls=events.filter(e=>e.event?.type==='model.call.completed').map(e=>e.event.payload);record.run=events.find(e=>e.event?.type==='run.completed')?.event?.payload;record.tool_events=events.filter(e=>/^tool\./.test(e.event?.type??''));await request(`/v1/agents/${agent}/turns/${id}`,'GET',undefined,'turns.get');await save();console.log(JSON.stringify({task,sample,turn:index,ttft_ms:record.first_text_ms,completion_ms:record.completion_ms,tool_calls:record.run?.tool_calls,model_calls:record.run?.model_calls,terminal:record.terminal}));if(terminal.type!=='turn_completed')throw Error('Turn failed');return record;}
try{
 for(const task of tasks.filter(t=>!process.env.AUDIT_TASKS||process.env.AUDIT_TASKS.split(',').includes(t.id))){for(let sample=task.sample;sample<=task.sample;sample++){
  // Each pair starts a fresh authority cache. The second turn reuses this agent and socket.
  wrapped=freshTransport();
  const start=performance.now(),created=await request('/v1/agents','POST',{settings:report.settings},'agents.create');
  const agent=created.text.agent_id??created.text.session_id;
  const resource={agent,task:task.id,sample,created_request_id:created.metrics.request_id,create_ms:created.metrics.complete_ms};report.resources.push(resource);await save();let stream,warming;
  try{
   stream=await connect(agent);resource.ws_ready_ms=stream.ready_ms;const idleBegan=performance.now();if(task.warm)warming=prepareRuntime(agent,resource);await sleep(task.idle);resource.intentional_idle_ms=performance.now()-idleBegan;resource.prepare_open_before_prompt=resource.prepare?.open_ms!==undefined;resource.before_prompt_event_types=stream.frames.map(e=>e.event?.type??e.type);
   resource.pre_prompt_ms=performance.now()-start;
   const first=await turn(agent,stream,task.prompt.replaceAll('${ID}',agent.slice(0,8)+'-'+sample),task.id,sample,1);
   resource.create_to_first_text_ms=resource.pre_prompt_ms+first.first_text_ms;

   await request(`/v1/agents/${agent}`,'GET',undefined,'agents.get');
   await request(`/v1/agents/${agent}/settings`,'PATCH',{thinking:'low'},'settings.patch');
   await request('/v1/agents?limit=1','GET',undefined,'agents.list.cached_auth');
   await request('/v1/agents?limit=1','GET',undefined,'agents.list.live_auth',true);
  } catch(error){resource.error=String(error);console.error(JSON.stringify({task:task.id,sample,error:String(error)}));process.exitCode=1;}
  finally{if(warming){if(warming.readyState===WS.CONNECTING)warming.terminate();else warming.close();}stream?.socket.close();try{await request(`/v1/agents/${agent}`,'DELETE',undefined,'agents.delete');resource.deleted=true;}catch(e){resource.cleanup_error=String(e);process.exitCode=1;}await save();}
 }}
}catch(error){report.error=String(error);console.error(report.error);process.exitCode=1;}
finally{report.loadavg_end=loadavg();report.versions_end=await deployments();report.finished_at=new Date().toISOString();await save();}
