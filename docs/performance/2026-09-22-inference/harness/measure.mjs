// Synthetic scratch agents only. Persist IDs immediately; never persist credentials, response headers or inventories.
import {readFileSync,writeFileSync,mkdirSync,existsSync,renameSync} from 'node:fs';
import {homedir,loadavg} from 'node:os';
import {join,resolve} from 'node:path';
import {createRequire} from 'node:module';
import {randomUUID} from 'node:crypto';
import {AsyncLocalStorage} from 'node:async_hooks';
import {channel} from 'node:diagnostics_channel';
import {monitorEventLoopDelay} from 'node:perf_hooks';
import {execFileSync,spawn} from 'node:child_process';
import {withManagedAccess} from '../../../../js/nanocodex/managed/Access.mjs';
import {deployments} from '../../2026-09-16-nanocodex2-cold-master/receipts.mjs';
const require=createRequire(join(process.env.NC_DEPS_ROOT,'js/managed/package.json')),WS=require('ws');
const cred=JSON.parse(readFileSync(join(homedir(),'Library/Application Support/Nanocodex/Native/credentials.json'),'utf8'));
const base=cred.baseUrl.replace(/\/$/,'');if(base!=='https://nanocodex.gakonst.workers.dev')throw Error('Unexpected origin');
const out=resolve(process.env.NC_OUTPUT??'output/inference-before-20260922');mkdirSync(out,{recursive:true});
if(existsSync(join(out,'measurements.json')))throw Error('Choose a new output directory; journals must not be overwritten');
for(const [key,max] of [['NC_LIFECYCLE',3],['NC_REPS',2],['NC_IDLE_MS',60000]]){if(process.env[key]!==undefined&&(!/^\d+$/.test(process.env[key])||Number(process.env[key])>max))throw Error('Cohort bound exceeded');}
if(process.env.NC_ARMS&&process.env.NC_ARMS.split(',').some(a=>!['immediate','prepare_race','prepare_5s'].includes(a)))throw Error('Unknown arm');
const now=()=>performance.now(),sleep=ms=>new Promise(r=>setTimeout(r,ms));
const submitTransport=process.env.NC_SUBMIT??'http';
if(!['http','ws'].includes(submitTransport))throw Error('Invalid submit transport');
const settings={model:'gpt-6-astra',thinking:'low',reasoning_mode:'standard',fast_mode:false};
const report={pid:process.pid,state:'starting',phase:process.env.NC_PHASE??'before',started_at:new Date().toISOString(),harness:'HTTP/WS inference with simultaneous WS/SSE; 2 agents, at most 10 turns including optional idle, 45s per turn',source_commit:execFileSync('git',['rev-parse','HEAD'],{encoding:'utf8'}).trim(),client_clock:'performance.now monotonic; wall clock for trace correlation only',submit_transport:submitTransport,settings,requests:[],attempts:[],resources:[],turns:[],failures:[],loadavg_start:loadavg()};
const safeError=e=>({name:e?.name??'Error',code:typeof e?.code==='string'?e.code:undefined,kind:e?.safeKind??'transport_or_runtime_error'});
const save=()=>{writeFileSync(join(out,'measurements.json.tmp'),JSON.stringify(report,null,2)+'\n');renameSync(join(out,'measurements.json.tmp'),join(out,'measurements.json'));writeFileSync(join(out,'status.json'),JSON.stringify({pid:process.pid,state:report.state,updated_at:new Date().toISOString(),resources:report.resources.map(r=>({id:r.id,deleted:r.deleted,cleanup_status:r.cleanup_status})),turns:report.turns.map(t=>({id:t.turn_id,label:t.label,terminal:t.ws?.terminal,error:t.error})),tail_pid:report.tail_pid},null,2)+'\n');writeFileSync(resolve('output/inference-journeys-progress.md'),`# Inference journeys progress\n\nState: ${report.state}; PID ${process.pid}; updated ${new Date().toISOString()}.\n\nOutput: ${out}.\nResources: ${report.resources.map(r=>r.id+' deleted='+r.deleted+' verify='+r.cleanup_status).join('; ')||'none yet'}.\nTurns: ${report.turns.length}; failures: ${report.failures.length}.\n\nDeployment fingerprints and measurements are in measurements.json; no credentials or response headers persisted.\n`);};
function failed(kind){const e=Error(kind);e.safeKind=kind;return e;}
async function provenance(){try{const p=await deployments();return Object.fromEntries(Object.entries(p).map(([k,v])=>[k,{id:v.id,created_on:v.created_on,versions:v.versions,source:v.annotations?.['workers/message']}]))}catch(e){return {error:safeError(e)}}}
let activeCohort;
const requestScope=new AsyncLocalStorage(), requestRows=new WeakMap(), knownSockets=new WeakSet();
const eventLoop=monitorEventLoopDelay({resolution:20});eventLoop.enable();
channel('undici:request:create').subscribe(({request})=>{const row=requestScope.getStore();if(row){requestRows.set(request,row);row.diag_created=now();}});
channel('undici:client:beforeConnect').subscribe(()=>{const row=requestScope.getStore();if(row){row.connect_started=now();row.new_connections=(row.new_connections??0)+1;}});
channel('undici:client:connected').subscribe(()=>{const row=requestScope.getStore();if(row&&row.connect_started!==undefined)row.connection_setup_ms=now()-row.connect_started;});
channel('undici:client:sendHeaders').subscribe(({request,socket})=>{const row=requestRows.get(request);if(row){row.diag_sent=now();row.socket_reused=knownSockets.has(socket);row.pre_send_ms=row.diag_sent-row.diag_created;}knownSockets.add(socket);});
channel('undici:request:bodySent').subscribe(({request})=>{const row=requestRows.get(request);if(row){row.diag_uploaded=now();row.upload_enqueue_ms=row.diag_uploaded-row.diag_sent;}});
channel('undici:request:headers').subscribe(({request})=>{const row=requestRows.get(request);if(row){row.upstream_wait_ms=now()-(row.diag_uploaded??row.diag_sent);}});
// Diagnostics contain timing and reuse only. Never serialize request/socket objects.

function freshTransport(){return withManagedAccess(async(input,init)=>{const req=new Request(input,init),start=now();const row={method:req.method,path:new URL(req.url).pathname,used_access_snapshot:req.headers.has('x-nanocodex-access'),cohort:activeCohort};report.attempts.push(row);try{const r=await requestScope.run(row,()=>fetch(req));Object.assign(row,{status:r.status,headers_ms:now()-start,request_id:r.headers.get('x-nanocodex-request-id')});return r}catch(e){Object.assign(row,{ms:now()-start,error:safeError(e)});throw e}})}
let transport=freshTransport();
async function request(path,{method='GET',body,label=path,live=false,allowFailure=false}={}){
 const t=now(),row={label,path,method,cohort:activeCohort,started_at:new Date().toISOString(),start_mono:t};report.requests.push(row);
 try{const response=await(live?fetch:transport)(base+path,{method,headers:{authorization:'Bearer '+cred.apiKey,'content-type':'application/json'},...(body===undefined?{}:{body:JSON.stringify(body)}),signal:AbortSignal.timeout(30000)});
 Object.assign(row,{status:response.status,headers_ms:now()-t,request_id:response.headers.get('x-nanocodex-request-id')});const raw=await response.text();row.ms=now()-t;row.bytes=Buffer.byteLength(raw);let value;try{value=raw?JSON.parse(raw):null}catch{throw failed('invalid_json')}
 if(!response.ok){row.error_code=typeof value?.error==='string'&&/^[a-z_0-9.-]{1,100}$/i.test(value.error)?value.error:'http_error';if(!allowFailure)throw failed('http_'+response.status)}
 if(label==='agents.create'&&response.ok){const createdId=value?.agent_id??value?.session_id;if(/^[0-9a-f-]{36}$/.test(createdId))row.created_agent_id=createdId;}save();return{value,row};
 }catch(e){Object.assign(row,{ms:now()-t,error:safeError(e)});save();throw e}
}
async function create(cohort){activeCohort=cohort;transport=freshTransport();const {value,row}=await request('/v1/agents',{method:'POST',body:{settings},label:'agents.create'});const id=value?.agent_id??value?.session_id;if(!/^[0-9a-f-]{36}$/.test(id))throw failed('invalid_agent_id');const resource={id,cohort,created_request_id:row.request_id,create_ms:row.ms,created_at:row.started_at,deleted:false};report.resources.push(resource);save();return resource}
async function cleanup(resource){try{await request(`/v1/agents/${resource.id}`,{method:'DELETE',label:'agents.delete'});resource.deleted=true;const {row}=await request(`/v1/agents/${resource.id}`,{label:'agents.verify_deleted',allowFailure:true});resource.cleanup_status=row.status;if(row.status!==404)throw failed('cleanup_not_404')}catch(e){resource.cleanup_error=safeError(e)}save()}
async function readState(id,label='agents.get'){const {value,row}=await request(`/v1/agents/${id}`,{label});row.settings=value?.settings;row.routing_enabled=value?.model_routing_enabled;row.routing_automatic=value?.model_routing_automatic;row.accepted_turns=value?.accepted_turns;save();return value}
async function history(id,label='history.get'){let after='0',events=[];for(let page=0;page<8;page++){const {value,row}=await request(`/v1/agents/${id}/events/history?after=${after}&limit=256`,{label});row.event_count=value?.data?.length;events.push(...value.data);if(!value.has_more)return events;after=value.data.at(-1)?.cursor}throw failed('history_page_bound')}
const terminalTypes=['turn_completed','turn_failed','turn_cancelled'];
function safeEvent(e,t){const p=e.event?.payload;let payload;
 if(e.event?.type==='assistant.delta')payload={text:p?.text,phase:p?.phase};
 else if(['model.call.completed','run.completed'].includes(e.event?.type))payload=Object.fromEntries(Object.entries(p??{}).filter(([k,v])=>typeof v==='number'||typeof v==='boolean'||['status','model','transport','provider'].includes(k)));
 return{received_mono:t,type:e.type,turn_id:e.turn_id,cursor:e.cursor,created_at:e.created_at,event_type:e.event?.type,event_request_id:e.event?.request_id,event_seq:e.event?.seq,...(payload?{payload}:{})}}
const isDelta=e=>e.event_type==='assistant.delta'&&typeof e.payload?.text==='string'&&e.payload.text.length>0;
function record(stream,e){const frame=safeEvent(e,now());stream.frames.push(frame);stream.onFrame?.(frame)}
async function connectWS(id,cursor='latest'){
 const started=now(),stream={kind:'ws',frames:[],closed:false},row={label:'websocket.ready',agent_id:id,cursor,started_at:new Date().toISOString()};report.requests.push(row);
 const socket=new WS(base.replace('https:','wss:')+`/v1/agents/${id}/ws?cursor=${cursor}`,{headers:{authorization:'Bearer '+cred.apiKey},handshakeTimeout:15000});stream.socket=socket;
 socket.on('error',()=>{stream.error='websocket_error'});socket.on('close',()=>{stream.closed=true});
 await new Promise((resolve,reject)=>{const timer=setTimeout(()=>{socket.terminate();reject(failed('websocket_timeout'))},15000);socket.on('open',()=>{row.open_ms=now()-started});socket.on('error',()=>{clearTimeout(timer);reject(failed('websocket_error'))});socket.on('message',raw=>{let e;try{e=JSON.parse(raw)}catch{return}record(stream,e);if(e.type==='ready'){clearTimeout(timer);row.ready_ms=now()-started;row.status=101;row.latest_cursor=e.latest_event_cursor;stream.latest_cursor=e.latest_event_cursor;resolve()}})});save();return stream;
}
async function connectSSE(id,cursor='latest'){
 const started=now(),abort=new AbortController(),stream={kind:'sse',frames:[],abort,closed:false},row={label:'sse.ready',agent_id:id,cursor,started_at:new Date().toISOString()};report.requests.push(row);
 const timer=setTimeout(()=>abort.abort(),15000);let response;
 try{response=await fetch(base+`/v1/agents/${id}/events?cursor=${cursor}`,{headers:{authorization:'Bearer '+cred.apiKey,accept:'text/event-stream'},signal:abort.signal});row.headers_ms=now()-started;row.status=response.status;row.request_id=response.headers.get('x-nanocodex-request-id');if(!response.ok||!response.headers.get('content-type')?.startsWith('text/event-stream'))throw failed('sse_http_'+response.status)}finally{clearTimeout(timer)}
 const reader=response.body.getReader(),decoder=new TextDecoder();stream.reader=reader;
 stream.pump=(async()=>{let buf='';try{while(true){const {done,value}=await reader.read();if(done)break;buf+=decoder.decode(value,{stream:true}).replaceAll('\r\n','\n');let split;while((split=buf.indexOf('\n\n'))>=0){const chunk=buf.slice(0,split);buf=buf.slice(split+2);const data=chunk.split('\n').filter(l=>l.startsWith('data:')).map(l=>l.slice(5).trimStart()).join('\n');if(!data)continue;let e;try{e=JSON.parse(data)}catch{continue}record(stream,e)}}}catch(e){if(!abort.signal.aborted)stream.error=safeError(e)}finally{stream.closed=true}})();save();return stream;
}
async function closeStreams(streams){for(const s of streams.filter(Boolean)){if(s.kind==='ws')s.socket.terminate();else s.abort.abort()}await Promise.all(streams.filter(s=>s?.pump).map(s=>s.pump));}
function metrics(stream,id,start){const frames=stream.frames.filter(e=>e.turn_id===id),accepted=frames.find(e=>e.type==='turn_accepted'),model=frames.find(e=>e.event_type==='model.call.started'),delta=frames.find(isDelta),terminal=frames.find(e=>terminalTypes.includes(e.type));return {accepted_ms:accepted?accepted.received_mono-start:null,first_delta_ms:delta?delta.received_mono-start:null,terminal_ms:terminal?terminal.received_mono-start:null,terminal:terminal?.type??null,terminal_cursor:terminal?.cursor??null,text:frames.filter(isDelta).map(e=>e.payload.text).join(''),server_admission_ms:accepted&&model?model.created_at-accepted.created_at:null,server_model_to_delta_ms:model&&delta?delta.created_at-model.created_at:null,frames};}
async function runTurn(resource,streams,transport,label,cancel=false){
 if(report.turns.length>=10)throw failed('turn_bound');
 const id='inference-'+randomUUID(),start=now(),row={agent_id:resource.id,turn_id:id,label,submit_transport:transport,cancel_transport:cancel?transport:undefined,started_at:new Date().toISOString(),start_mono:start,loadavg:loadavg()};report.turns.push(row);report.state='turn '+label;save();
 let cancelPromise,acceptedMono,timer;
 const requestCancel=trigger=>{if(row.cancel_requested_ms!==undefined)return;row.cancel_requested_ms=now()-start;row.cancel_trigger=trigger;save();if(transport==='ws'){streams.ws.socket.send(JSON.stringify({type:'cancel',id}));row.cancel_send_ms=now()-start;cancelPromise=Promise.resolve()}else cancelPromise=request(`/v1/agents/${resource.id}/turns/${id}/cancel`,{method:'POST',label:'turns.cancel',allowFailure:true}).then(({row:r})=>{row.cancel_http_ms=r.ms;row.cancel_http_status=r.status;row.cancel_request_id=r.request_id;save()}).catch(e=>{row.cancel_error=safeError(e);save()})};
 const onFrame=e=>{if(e.turn_id!==id)return;if(e.type==='turn_accepted'&&acceptedMono===undefined){acceptedMono=e.received_mono;if(cancel)timer=setTimeout(()=>requestCancel('5s_after_accepted'),5000)}if(cancel&&isDelta(e))requestCancel('first_delta')};streams.ws.onFrame=onFrame;streams.sse.onFrame=onFrame;
 try{
  if(transport==='http'){const {row:r}=await request(`/v1/agents/${resource.id}/turns`,{method:'POST',body:{id,input:'Return exactly 42. Do not call any tools.'},label:'turns.create'});row.http_accept_ms=r.ms;row.request_id=r.request_id;if(cancel&&acceptedMono===undefined){acceptedMono=now();timer=setTimeout(()=>requestCancel('5s_after_http_accepted'),5000)}}
  else{streams.ws.socket.send(JSON.stringify({type:'prompt',id,input:'Return exactly 42. Do not call any tools.'}));row.ws_send_ms=now()-start}
  while(now()-start<45000){const ws=metrics(streams.ws,id,start),sse=metrics(streams.sse,id,start);if(ws.terminal&&sse.terminal){row.ws=ws;row.sse=sse;break}if(streams.ws.closed&&streams.sse.closed)throw failed('both_streams_closed');await sleep(5)}
  row.ws??=metrics(streams.ws,id,start);row.sse??=metrics(streams.sse,id,start);
  if(!row.ws.terminal||!row.sse.terminal)throw failed('turn_or_stream_timeout_45s');
  row.sse_minus_ws_first_delta_ms=row.sse.first_delta_ms!==null&&row.ws.first_delta_ms!==null?row.sse.first_delta_ms-row.ws.first_delta_ms:null;
  row.sse_minus_ws_terminal_ms=row.sse.terminal_ms-row.ws.terminal_ms;
  row.same_event_cursors=JSON.stringify(row.ws.frames.map(e=>e.cursor))===JSON.stringify(row.sse.frames.map(e=>e.cursor));
  if(cancel){row.cancel_outcome=row.ws.terminal;row.cancel_to_ws_terminal_ms=row.cancel_requested_ms!==undefined?row.ws.terminal_ms-row.cancel_requested_ms:null;row.cancel_to_sse_terminal_ms=row.cancel_requested_ms!==undefined?row.sse.terminal_ms-row.cancel_requested_ms:null}
  else if(row.ws.terminal!=='turn_completed'||row.ws.text.trim()!=='42'||row.sse.text.trim()!=='42')throw failed('unexpected_turn_outcome');
 }catch(e){row.ws=metrics(streams.ws,id,start);row.sse=metrics(streams.sse,id,start);row.error=safeError(e);report.failures.push({label,error:row.error});if(!row.ws.terminal&&!row.sse.terminal){try{await request(`/v1/agents/${resource.id}/turns/${id}/cancel`,{method:'POST',label:'turns.timeout_cleanup_cancel',allowFailure:true})}catch(e){row.cleanup_cancel_error=safeError(e)}}}
 finally{clearTimeout(timer);streams.ws.onFrame=undefined;streams.sse.onFrame=undefined;await cancelPromise;row.event_loop_delay_ms={mean:eventLoop.mean/1e6,max:eventLoop.max/1e6,p99:eventLoop.percentile(99)/1e6};eventLoop.reset();save();console.log(JSON.stringify({label,turn_id:id,ws_first_delta_ms:row.ws?.first_delta_ms,sse_first_delta_ms:row.sse?.first_delta_ms,ws_terminal_ms:row.ws?.terminal_ms,sse_terminal_ms:row.sse?.terminal_ms,terminal:row.ws?.terminal,cancel:row.cancel_outcome,error:row.error}))}
 return row;
}
async function replay(resource,turn){const original=turn.ws.frames;if(original.length<2)return;const after=original[0].cursor,expected=original.slice(1).map(e=>e.cursor),row={agent_id:resource.id,turn_id:turn.turn_id,after,expected_cursors:expected,started_at:new Date().toISOString()};report.replay??=[];report.replay.push(row);let ws,sse;const t=now();try{ws=await connectWS(resource.id,after);sse=await connectSSE(resource.id,after);while(now()-t<15000){if([ws,sse].every(s=>s.frames.some(e=>e.cursor===turn.ws.terminal_cursor)))break;await sleep(5)}for(const s of [ws,sse]){const actual=s.frames.filter(e=>e.turn_id===turn.turn_id).map(e=>e.cursor);row[s.kind]={cursors:actual,exact:JSON.stringify(actual)===JSON.stringify(expected),duplicate:actual.length!==new Set(actual).size};}row.ms=now()-t;if(!row.ws.exact||!row.sse.exact)throw failed('replay_mismatch')}catch(e){row.error=safeError(e);report.failures.push({label:'replay',error:row.error})}finally{await closeStreams([ws,sse]);save()}}
const versionKey=p=>Object.fromEntries(Object.entries(p??{}).map(([k,v])=>[k,JSON.stringify(v.versions??v.error)]));
function writeSummary(){const warm=report.turns.filter(t=>t.label.includes('warm'));const fmt=n=>typeof n==='number'?n.toFixed(1):'—';let lines=[`# Inference baseline (${report.phase})`,``,`Started ${report.started_at}; finished ${report.finished_at}. Fixed gpt-6-astra, low thinking, standard reasoning, fast mode false, routing disabled and verified. Synthetic prompt: Return exactly 42. Do not call any tools. Two scratch agents, ten-turn maximum including optional idle, 45-second turn deadline. Client intervals use one monotonic clock; creation and stream setup excluded.`,``,`Deployment versions stable: ${report.versions_stable}. ${report.versions_stable?'This cohort can serve as a version-pinned baseline.':'Cohort invalidated for before/after attribution; deployment versions changed or unavailable.'}`,``,`| Turn | HTTP receipt ms | WS delta ms | SSE delta ms | WS terminal ms | SSE terminal ms | Terminal |`,`|---|---:|---:|---:|---:|---:|---|`,...report.turns.map(t=>`| ${t.label} | ${fmt(t.http_accept_ms)} | ${fmt(t.ws?.first_delta_ms)} | ${fmt(t.sse?.first_delta_ms)} | ${fmt(t.ws?.terminal_ms)} | ${fmt(t.sse?.terminal_ms)} | ${t.ws?.terminal??'missing'} |`),``,`Warm means the second and third turns on each transport's retained scratch agent. Sample size is two warm turns per transport; sequential arms and provider/load variability prevent a causal speedup claim. Cancellation uses the same short 42 prompt, so a completed terminal may win the race; negative cancel-to-terminal means it did.`,``,`| Cancel | Trigger | HTTP response ms/status | WS terminal minus cancel ms | SSE terminal minus cancel ms | Outcome |`,`|---|---|---|---:|---:|---|`,...report.turns.filter(t=>t.cancel_transport).map(t=>`| ${t.cancel_transport} | ${t.cancel_trigger??'none'} | ${fmt(t.cancel_http_ms)} / ${t.cancel_http_status??'WS'} | ${fmt(t.cancel_to_ws_terminal_ms)} | ${fmt(t.cancel_to_sse_terminal_ms)} | ${t.cancel_outcome??'missing'} |`),``,`Replay: ${JSON.stringify(report.replay??[])}.`,``,`Cleanup: ${report.resources.map(r=>r.id+': deleted='+r.deleted+', verification='+r.cleanup_status).join('; ')}. Tail process: ${report.tail_pid??'none'}, stopped=${report.tail_stopped??false}. Failures: ${JSON.stringify(report.failures)}.`,``,`Version fingerprints:`,``,`\`\`\`json`,JSON.stringify({start:report.provenance_start,end:report.provenance_end},null,2),`\`\`\``,``,`Sanitized source data: ${out}/measurements.json; raw allowlisted tails remain in output and are not published wholesale. Harness: docs/performance/2026-09-22-inference/harness/measure.mjs. No ordinary GET performance cohorts or deployments were run.`];writeFileSync(resolve(`docs/performance/2026-09-22-inference/summary-${report.phase}.md`),lines.join('\n')+'\n')}
let collector;
try{
 save();report.provenance_start=await provenance();save();if(report.provenance_start.error)throw failed('missing_start_versions');
 if(process.env.NC_WRANGLER){collector=spawn('python3',[new URL('./tail.py',import.meta.url).pathname,out],{stdio:'ignore',env:process.env});report.tail_pid=collector.pid;save();await sleep(6000);report.trace_collector_started=true;save()}
 const transports=(process.env.NC_TRANSPORTS??'http,ws').split(',');if(transports.length>2||new Set(transports).size!==transports.length||transports.some(t=>!['http','ws'].includes(t)))throw failed('transport_bounds');
 for(const transport of transports){
  const resource=await create(transport);let ws,sse;try{
   await request(`/v1/agents/${resource.id}/routing`,{method:'POST',body:{model:settings.model,thinking:settings.thinking},label:'routing.disable'});
   const state=await readState(resource.id,'config.verify');resource.settings=state.settings;resource.routing_enabled=state.model_routing_enabled;resource.routing_automatic=state.model_routing_automatic;save();if(state.model_routing_enabled!==false||Object.entries(settings).some(([k,v])=>state.settings?.[k]!==v))throw failed('settings_mismatch');
   ws=await connectWS(resource.id);sse=await connectSSE(resource.id);
   const idleMs=Number(process.env.NC_IDLE_MS??0);
   const labels=['cold','warm1','warm2',...(idleMs>0?['idle']:[]),'cancel'];
   let last;for(const label of labels){
    let idleElapsed;
    if(label==='idle'){
     const idleStart=now(),ping=setInterval(()=>{if(ws.socket.readyState===WS.OPEN)ws.socket.send(JSON.stringify({type:'ping'}))},5000);
     try{await sleep(idleMs);idleElapsed=now()-idleStart}finally{clearInterval(ping)}
    }
    last=await runTurn(resource,{ws,sse},transport,transport+'_'+label,label==='cancel');
    if(idleElapsed!==undefined){last.idle_before_ms=idleElapsed;save()}
    if(last.error&&last.error.kind!=='unexpected_turn_outcome')break;
   }
   await closeStreams([ws,sse]);ws=sse=undefined;
   const replayTurn=report.turns.find(t=>t.agent_id===resource.id&&t.label===transport+'_warm2'&&t.ws?.terminal);if(replayTurn)await replay(resource,replayTurn);
  }catch(e){resource.error=safeError(e);report.failures.push({cohort:transport,error:resource.error})}
  finally{await closeStreams([ws,sse]);await cleanup(resource)}
 }
}catch(e){report.fatal_error=safeError(e);report.failures.push({label:'fatal',error:report.fatal_error});process.exitCode=1}
finally{
 report.state='cleanup';save();for(const resource of report.resources.filter(r=>!r.deleted))await cleanup(resource);
 report.provenance_end=await provenance();report.versions_stable=!report.provenance_end.error&&!report.provenance_start?.error&&JSON.stringify(versionKey(report.provenance_start))===JSON.stringify(versionKey(report.provenance_end));save();
 if(collector){await sleep(3000);collector.kill('SIGTERM');await new Promise(r=>{if(collector.exitCode!==null)return r();collector.once('exit',r);setTimeout(r,5000)});report.tail_stopped=collector.exitCode!==null;report.tail_exit_code=collector.exitCode;if(!report.tail_stopped){collector.kill('SIGKILL');report.failures.push({label:'tail_stop_timeout'})}}
 eventLoop.disable();report.finished_at=new Date().toISOString();report.loadavg_end=loadavg();report.state='complete';save();writeSummary();console.log(JSON.stringify({done:true,pid:process.pid,resources:report.resources.length,turns:report.turns.length,failures:report.failures.length,versions_stable:report.versions_stable,cleanup:report.resources.map(r=>r.cleanup_status),tail_stopped:report.tail_stopped}));
}
