import {deployments} from './receipts.mjs';
import {readFileSync,writeFileSync,mkdtempSync,rmSync,realpathSync} from 'node:fs';
import {spawn,execFileSync} from 'node:child_process';
import {tmpdir,loadavg} from 'node:os';
import {join} from 'node:path';
import {createHash} from 'node:crypto';

const dir=new URL('./',import.meta.url), home='/Users/georgios';
const cred=JSON.parse(readFileSync(home+'/Library/Application Support/Nanocodex/Native/credentials.json','utf8'));
const base=cred.baseUrl.replace(/\/$/,'');
if(base!=='https://nanocodex.gakonst.workers.dev')throw Error('Unexpected origin');
const arm=process.argv[2]??'release';const nano=arm==='release'?'/Users/georgios/github/gakonst/nanocodex-master/output/nanocodex2-cold-master-20260916/nanocodex2':home+'/.nanocodex/bin/nanocodex2';
const workspace=mkdtempSync(join(tmpdir(),'nc-yo-compare-'));
const report={started_at:new Date().toISOString(),prompt:'yo',model:'gpt-6-astra',effort:'low',tier:'standard',repetitions:Number(process.env.REPS??5),
  boundary:'Fresh CLI process and fresh managed agent to first nonempty assistant text delta; no intentional preparation lead or previous conversation.',
  nanocodex2_mode:'Selected managed CLI run. Hosted defaults, local workspace tools, saved account credential.',
  workspace,versions:{nanocodex2:execFileSync(nano,['--version'],{encoding:'utf8'}).trim()},
  nano_binary:nano,nano_install:realpathSync(home+'/.nanocodex/current'),nano_sha256:createHash('sha256').update(readFileSync(realpathSync(arm==='installed'?home+'/.nanocodex/current/nanocodex2':nano))).digest('hex'),
  rows:[],resources:[],requests:[]};
const clean=s=>String(s).replaceAll(cred.apiKey,'[REDACTED]').replace(/sk-[A-Za-z0-9_*-]{16,}/g,'[REDACTED]');
const save=()=>writeFileSync(new URL('cold.json',dir),JSON.stringify(report,(k,v)=>['encrypted_content','obfuscation','websocket_url','authorization','access_token','refresh_token'].includes(k)?'[omitted]':typeof v==='string'?clean(v):v,2)+'\n');
const now=()=>performance.now();
async function request(path,method='GET'){
 const start=now(),r=await fetch(base+path,{method,headers:{authorization:'Bearer '+cred.apiKey},signal:AbortSignal.timeout(30000)});
 const body=await r.text();report.requests.push({path,method,status:r.status,ms:now()-start,request_id:r.headers.get('x-nanocodex-request-id'),server_timing:r.headers.get('server-timing')});
 if(!r.ok)throw Error(clean(`${method} ${path}: ${r.status} ${body.slice(0,300)}`));return body?JSON.parse(body):null;
}
async function history(id){let after='0',all=[];for(let n=0;n<10;n++){const h=await request(`/v1/agents/${id}/events/history?after=${after}&limit=256`);all.push(...h.data);if(!h.has_more)return all;after=h.data.at(-1).cursor;}throw Error('Too many history pages');}
function finish(row){save();console.log(JSON.stringify({path:row.path,rep:row.rep,state:row.state,first_text_ms:row.first_text_ms,completion_ms:row.completion_ms,prompt_to_text_ms:row.prompt_to_text_ms,text:row.text,error:row.error}));}
async function nanoTurn(rep,id){
 const row={path:'nanocodex2',rep,state:id?'resume_session':'fresh_session',events:[],text:'',started_at:new Date().toISOString(),loadavg:loadavg()};report.rows.push(row);
 const args=['run','yo',...(id?['--agent',id]:['--model',report.model,'--thinking',report.effort,'--reasoning-mode','standard'])];
 const start=now(),child=spawn(nano,args,{cwd:workspace,env:{...process.env,NANOCODEX_API_KEY:cred.apiKey,NANOCODEX_MANAGED_URL:base},stdio:['ignore','pipe','pipe']});
 let buf='',stderr='';row.command=['nanocodex2',...args];
 child.stderr.on('data',chunk=>{const t=now()-start;stderr+=clean(chunk);const match=stderr.match(/Managed agent: ([0-9a-f-]+)/);if(match&&!id){id=match[1];row.agent_ready_ms=t;report.resources.push({path:'nanocodex2',id,deleted:false});save();}});
 child.stdout.on('data',chunk=>{buf+=chunk;let p;while((p=buf.indexOf('\n'))>=0){const line=buf.slice(0,p);buf=buf.slice(p+1);if(!line)continue;const e=JSON.parse(line),t=now()-start;row.events.push({t,event:e});
  if(e.type==='run.started')row.run_started_ms??=t;
  if(e.type==='model.call.started')row.model_call_receipt_ms??=t;
  if(e.type==='assistant.delta'&&e.payload?.text){row.first_text_ms??=t;row.text+=e.payload.text;}
  if(e.type==='run.completed'){row.completion_ms=t;row.status=e.payload.status;}
  if(e.type==='model.call.completed')row.model=e.payload;
 }});
 let timer;try{
  const code=await new Promise((resolve,reject)=>{timer=setTimeout(()=>{child.kill('SIGTERM');reject(Error('Nano deadline'));},90000);child.once('error',reject);child.once('close',resolve);});
  row.process_ms=now()-start;row.exit_code=code;row.stderr=stderr;row.session_id=id;
  if(code!==0||!row.first_text_ms||row.status!=='completed')throw Error('Nano incomplete: '+stderr.slice(-1000));
  row.history=(await history(id));
  const newest=row.history.filter(e=>e.type==='turn_accepted').at(-1);row.turn_id=newest?.turn_id;
  row.history=row.history.filter(e=>e.turn_id===row.turn_id);
  const accepted=row.history.find(e=>e.type==='turn_accepted'),started=row.history.find(e=>e.event?.type==='model.call.started'),delta=row.history.find(e=>e.event?.type==='assistant.delta'&&e.event.payload?.text);
  row.server_admission_ms=started&&accepted?started.created_at-accepted.created_at:null;
  row.server_model_to_text_ms=delta&&started?delta.created_at-started.created_at:null;
  row.server_accepted_to_text_ms=delta&&accepted?delta.created_at-accepted.created_at:null;
  row.setup_and_delivery_residual_ms=row.server_accepted_to_text_ms==null?null:row.first_text_ms-row.server_accepted_to_text_ms;
  row.settings=(await request(`/v1/agents/${id}`)).settings;
  return id;
 }catch(e){row.error=clean(e);throw e;}finally{clearTimeout(timer);if(child.exitCode===null)child.kill('SIGTERM');finish(row);}
}

report.arm=arm;report.source_commit=execFileSync('git',['rev-parse','HEAD'],{encoding:'utf8'}).trim();report.versions_start=await deployments();save();
try{for(let rep=1;rep<=report.repetitions;rep++){
 await nanoTurn(rep);
 const resource=report.resources.at(-1);await request(`/v1/agents/${resource.id}`,'DELETE');resource.deleted=true;
 const r=await fetch(base+`/v1/agents/${resource.id}`,{headers:{authorization:'Bearer '+cred.apiKey},signal:AbortSignal.timeout(30000)});resource.cleanup_status=r.status;await r.arrayBuffer();
 if(r.status!==404)throw Error('Agent remains visible after delete');save();
 }report.complete=true;}catch(error){report.error=clean(error);process.exitCode=1;}
finally{for(const resource of report.resources.filter(r=>!r.deleted)){try{await request(`/v1/agents/${resource.id}`,'DELETE');resource.deleted=true;const r=await fetch(base+`/v1/agents/${resource.id}`,{headers:{authorization:'Bearer '+cred.apiKey},signal:AbortSignal.timeout(30000)});resource.cleanup_status=r.status;await r.arrayBuffer();}catch(error){resource.cleanup_error=clean(error);}}report.finished_at=new Date().toISOString();report.versions_end=await deployments();save();rmSync(workspace,{recursive:true,force:true});}
