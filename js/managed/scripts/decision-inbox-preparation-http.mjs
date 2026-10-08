#!/usr/bin/env node
// New preparation routes: real loopback HTTP + shipped proxy/auth/router/DO/alarm.
// Only external inference and Google/connector broker are synthetic; no live account.
// Sends below are restricted to this Miniflare fixture, never real Google.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdir, writeFile, rm } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { build } from 'esbuild';
import { Miniflare } from 'miniflare';
const managed = fileURLToPath(new URL('..', import.meta.url));
const output = fileURLToPath(new URL('../../../output/mobile-decision-inbox/e2e', import.meta.url));
const store = join(output, 'local-preparation-store-' + crypto.randomUUID());
const trace = [], timings = [], checks = [], providerTrace = [], modelEpochs = [];
const startedAt = new Date().toISOString();
const safetyGatedCaptures = process.argv.includes('--safety-gated-captures');
assert.ok(process.argv.slice(2).every(arg=>arg==='--safety-gated-captures'),'unknown harness option');
const captureExpected = safetyGatedCaptures ? 'blocked' : 'ready';
const publicQuery = 'California health insurance comparison';
const publicSource = 'https://www.healthcare.gov/choose-a-plan/comparing-plans/';
const researchTrace = [];
let bundleDigest, sourceHeadAfterBundle;
const sourceHead=spawnSync('git',['rev-parse','HEAD'],{encoding:'utf8'}).stdout.trim();
const connection = 'D'.repeat(43); let sendCalls = 0, unknownSend = false;
let sourceBody = 'Here is the supplied summary.', sourceLabels = ['INBOX', 'UNREAD'], newerMessage = false;
const subjects = new Map(), acceptedSnapshots = [];
const source = `
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey, authenticate } from './src/account-auth.ts';
import { routeTodoRequest } from './src/todo-inbox.ts';
import { routeManaged } from '../account/worker/managedProxy.ts';
export { UserAccount, Organization, ApiKeyRecord, NonceStorage };
export default { async fetch(request,env) {
const url=new URL(request.url);
if(env.EDGE) return await routeManaged(request,env,url) ?? new Response('not_found',{status:404});
if(url.pathname==='/__fixture') {const input=await request.json();await ensureAccount(env,input.user,true);
if(input.decision) return Response.json(await env.NANOCODEX_USERS.getByName(input.user).proposeTodoDecision(input.decision));
const auth=await (await env.NANOCODEX_USERS.getByName(input.user).fetch('https://user.internal/authorization')).json();
return Response.json(await createApiKey(env,{kind:'api_key',userId:input.user,...auth.grant,subjectId:'api_key:'+input.user,credentialId:'fixture',capabilities:auth.grant.capabilities},'synthetic-preparation-http'));}
return await routeTodoRequest(request,env,url,await authenticate(request,env,url)) ?? new Response('not_found',{status:404}); }};`;
const ai = `import {WorkerEntrypoint} from 'cloudflare:workers'; let calls=0; const attempts=new Map();
export class SyntheticAI extends WorkerEntrypoint { async run(model,input,options) {
calls++;if(input.tools||input.stream!==false||options.gateway.collectLog!==false||!options.gateway.skipCache) throw Error('unsafe inference');
const context=JSON.parse(input.messages[1].content);
const fields=Object.keys(input.response_format.json_schema.properties).sort();
if(fields.join(',')==='queries') {
 if(Object.keys(context).sort().join(',')!=='owner_changes,owner_request') throw Error('planner received non-owner evidence');
 return {response:JSON.stringify({queries:["California health insurance comparison"]})};
}
if(!fields.includes('source_references') || !Array.isArray(context.evidence)) throw Error('unexpected synthesis shape');
const attempt=(attempts.get(context.owner_changes)||0)+1;attempts.set(context.owner_changes,attempt);
if(context.owner_changes==='RETRY_ONCE' && attempt===1 || context.owner_changes==='RETRY_EXHAUST') throw Error('fixture inference unavailable');
if(context.owner_changes.startsWith('SLOW_')) await new Promise(resolve=>setTimeout(resolve,400));
if(context.owner_changes==='INVALID_OUTPUT') return {response:JSON.stringify({send:true})};
const blocked=context.owner_request.includes('BLOCK_EXTERNAL');
return {response:JSON.stringify({status:blocked?'blocked':'ready',context:'Account-local owner-provided evidence',recommendation:blocked?'Missing external evidence':'Review the concrete proposal',proposal:blocked?'':'Review the three supplied hypotheses in priority order, record one observation for each, then compare the observations.',body_text:context.kind==='email_reply'?(context.owner_changes?'Updated reply: '+context.owner_changes:'Thanks for sending the supplied summary.'):'',source_references:context.evidence.filter(e=>e.kind==='web').length?context.evidence.filter(e=>e.kind==='web').map(e=>e.reference):[context.evidence[0].reference],missing_information:blocked?'external_research_required':''})}; }
async fetch(){return Response.json({calls,attempts:Object.fromEntries(attempts)});} }
export default {fetch(){return Response.json({calls,attempts:Object.fromEntries(attempts)});}};`;
let mf;
await mkdir(output,{recursive:true});
try {
  const aliases = Object.fromEntries(['rpc','managed-auth','managed-live','managed-access'].map(n=>['nanocodex/cloudflare/'+n,fileURLToPath(new URL('../../nanocodex/cloudflare/'+n+'.mjs',import.meta.url))]));
  const bundled = await build({stdin:{contents:source,resolveDir:managed},bundle:true,write:false,format:'esm',target:'es2022',platform:'browser',external:['cloudflare:workers','node:*'],alias:{...aliases,'node-rsa':join(managed,'node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs')}});
  const script=bundled.outputFiles[0].text;
  bundleDigest=createHash('sha256').update(script).digest('hex');
  sourceHeadAfterBundle=spawnSync('git',['rev-parse','HEAD'],{encoding:'utf8'}).stdout.trim();
  assert.equal(sourceHeadAfterBundle,sourceHead,'HEAD changed while bundling');
  const broker = async request => {
    const url=new URL(request.url);providerTrace.push({method:request.method,path:url.pathname});
    if(url.pathname.startsWith('/subjects/')) {subjects.set(url.pathname.split('/').at(-1),(await request.json()).user_id);return new Response(null,{status:204});}
    if(url.pathname==='/v1/search') {
      assert.equal(url.href,'https://nanocodex.internal/v1/search');assert.equal(request.method,'POST');
      assert.equal(request.headers.get('authorization'),'Bearer NANOCODEX_PROVIDER_CREDENTIAL');
      assert.ok(subjects.has(request.headers.get('x-nanocodex-subject')),'research subject must be owner-bound');
      const query=await request.json();assert.deepEqual(query.commands,{search_query:[{q:publicQuery}],response_length:'long'});
      assert.deepEqual(query.settings,{allowed_callers:['direct'],external_web_access:true});
      assert.equal(query.model,'gpt-6-astra');assert.equal(query.max_output_tokens,undefined);
      assert.deepEqual(Object.keys(query).sort(),['commands','id','model','settings']);
      researchTrace.push({query:publicQuery,source:publicSource,method:request.method,path:url.pathname});
      return Response.json({output:'Comparing health plans ('+publicSource+')\n[Retrieved fixture; wordlim 200] Compare premiums, deductibles, provider networks and covered benefits. Public comparison guidance does not establish personalized eligibility or an eligible quote.'});
    }
    if(url.pathname.endsWith('/connectors')) return Response.json({connectors:{gmail:{connected:true,connections:[{id:connection,label:'Synthetic inbox',capabilities:['gmail'],scopes:['https://www.googleapis.com/auth/gmail.modify']}]}}});
    if(url.hostname !== 'broker.internal') {
      assert.equal(request.headers.get('x-nanocodex-connector-connection'),connection);
      assert.ok(subjects.has(request.headers.get('x-nanocodex-subject')),'provider subject must be owner-bound');
      assert.equal(request.headers.get('authorization'),'Bearer NANOCODEX_PROVIDER_CREDENTIAL');
    }
    const message={id:'mfixture',threadId:'tfixture',internalDate:'1780000000000',labelIds:sourceLabels,payload:{mimeType:'text/plain',headers:[{name:'From',value:'Sender <sender@example.test>'},{name:'To',value:'owner@example.test'},{name:'Subject',value:'Supplied summary'},{name:'Message-ID',value:'<fixture@example.test>'}],body:{data:Buffer.from(sourceBody).toString('base64url')}}};
    if(url.pathname.endsWith('/threads/tfixture')) return Response.json({id:'tfixture',messages:[message,...(newerMessage?[{...message,id:'mnew'}]:[])]});
    if(url.pathname.endsWith('/messages/mfixture')) return Response.json(message);
    if(url.pathname.endsWith('/profile')) return Response.json({emailAddress:'owner@example.test'});
    if(url.pathname.endsWith('/messages/send')) {
      sendCalls++;const body=await request.json(),mime=Buffer.from(body.raw,'base64url').toString();
      assert.equal(body.threadId,'tfixture');assert.match(mime,/To: sender@example.test/);assert.match(mime,/In-Reply-To: <fixture@example.test>/);
      acceptedSnapshots.push({attempt:sendCalls,thread_id:body.threadId,body_text:Buffer.from(mime.split('\r\n\r\n')[1].replaceAll('\r\n',''),'base64').toString(),fixture_outcome:unknownSend?'accepted_then_504':'accepted'});
      if(unknownSend) return new Response('fixture accepted, response lost',{status:504});
      return Response.json({id:'fixture-sent-'+sendCalls,threadId:'tfixture'});
    }
    throw Error('unexpected synthetic provider route '+url.pathname);
  };
  const options={durableObjectsPersist:store,workers:[
    {name:'edge',script,modules:true,compatibilityDate:'2026-07-29',compatibilityFlags:['nodejs_compat'],bindings:{EDGE:true},serviceBindings:{NANOCODEX_BACKEND:'managed'}},
    {name:'managed',script,modules:true,compatibilityDate:'2026-07-29',compatibilityFlags:['nodejs_compat','enable_request_signal'],serviceBindings:{AI:{name:'ai',entrypoint:'SyntheticAI'},NANOCODEX:broker},durableObjects:{NANOCODEX_AUTH:{className:'NonceStorage',useSQLite:true},NANOCODEX_USERS:{className:'UserAccount',useSQLite:true},NANOCODEX_ORGANIZATIONS:{className:'Organization',useSQLite:true},NANOCODEX_API_KEYS:{className:'ApiKeyRecord',useSQLite:true}}},
    {name:'ai',script:ai,modules:true,compatibilityDate:'2026-07-29'}]};
  mf=new Miniflare(options);
  let backend=await mf.getWorker('managed'), aiBinding=await mf.getWorker('ai','SyntheticAI'), base=await mf.ready;
  async function key(user=crypto.randomUUID()){const r=await backend.fetch('https://fixture.test/__fixture',{method:'POST',body:JSON.stringify({user})});assert.equal(r.status,200);return (await r.json()).token;}
  const owner=crypto.randomUUID(), token=await key(owner), other=await key();
  async function call(path,method='GET',body,expected=200,credential=token){
    const start=performance.now();const r=await fetch(new URL('/v1/todo'+path,base),{method,headers:{...(credential?{authorization:'Bearer '+credential}:{}),'content-type':'application/json'},...(body===undefined?{}:{body:JSON.stringify(body)})});
    const text=await r.text();let data;try{data=JSON.parse(text);}catch{data={non_json:true};}
    const ms=Number((performance.now()-start).toFixed(2));trace.push({path,method,status:r.status,data,duration_ms:ms});assert.equal(r.status,expected,method+' '+path+' '+text);return data;
  }
  const metrics=async()=>(await (await aiBinding.fetch('https://fixture.test/metrics')).json()).calls;
  const wait=async(id,state)=>{for(let i=0;i<500;i++){const item=(await call('/items/'+id)).item;if(item.preparation.status===state)return item;if(state==='ready'&&item.preparation.status==='blocked')throw Error('capture readiness required; got blocked/'+item.preparation.error+' (product acceptance HOLD)');await new Promise(r=>setTimeout(r,25));}throw Error('preparation did not reach '+state);};
  const waitCapture = async id => {
    const item=await wait(id,captureExpected);
    if(safetyGatedCaptures){assert.equal(item.preparation.error,'complete_capture_proposal_unverified');assert.equal(item.preparation.draft_id,null);assert.equal(item.preparation.status,'blocked');}
    return item;
  };
  await call('', 'GET', undefined, 401, null);
  const noModelBefore=await metrics(), noProviderBefore=providerTrace.length;
  const deterministic=(await call('','POST',{body:'Format this as bullet points:\nalpha\nβeta',operation_id:crypto.randomUUID()},201)).item;
  const formatted=await wait(deterministic.id,'ready');
  assert.equal(formatted.preparation.proposal,'- alpha\n- βeta');
  assert.equal(formatted.preparation.error,null);assert.equal(formatted.preparation.draft_id,null);
  assert.match(formatted.preparation.scope,/not fact-checked/);
  assert.deepEqual(formatted.preparation.sources.map(s=>s.kind),['user']);
  for(let i=0;i<3;i++) assert.equal((await call('/items/'+deterministic.id)).item.preparation.proposal,formatted.preparation.proposal);
  assert.equal(await metrics(),noModelBefore);assert.equal(providerTrace.length,noProviderBefore);assert.equal(sendCalls,0);
  const parked=(await call('/items/'+deterministic.id,'PATCH',{version:1,status:'parked',operation_id:crypto.randomUUID()})).item;
  assert.equal(parked.preparation.error,'preparation_parked');
  await call('/items/'+deterministic.id+'/prepare','POST',{version:1,text:'Obsolete',operation_id:crypto.randomUUID()},409);
  await call('/items/'+deterministic.id,'PATCH',{version:parked.version,status:'captured',operation_id:crypto.randomUUID()});
  assert.equal((await wait(deterministic.id,'ready')).preparation.proposal,formatted.preparation.proposal);
  assert.equal(await metrics(),noModelBefore);assert.equal(providerTrace.length,noProviderBefore);
  checks.push('strict complete supplied-text bullet projection verifies every line/order; actual account alarm/cache/Park/reopen performs zero model/provider calls, no draft or send');
  const input={body:'Organize these notes: review three supplied hypotheses, record one observation for each, then compare observations. Only transform this owner-provided text.',operation_id:crypto.randomUUID()};
  const first=(await call('','POST',input,201)).item;assert.ok(['pending','preparing','ready',...(safetyGatedCaptures?['blocked']:[])].includes(first.preparation.status));
  const ready=await waitCapture(first.id);assert.ok(ready.preparation.proposal);assert.equal(ready.preparation.draft_id,null);
  const afterReady=await metrics();
  for(let i=0;i<8;i++){const start=performance.now();const item=(await call('/items/'+first.id)).item;assert.equal(item.preparation.status,captureExpected);if(safetyGatedCaptures)assert.equal(item.preparation.error,'complete_capture_proposal_unverified');assert.equal(item.preparation.proposal,ready.preparation.proposal);timings.push(performance.now()-start);}
  assert.equal(await metrics(),afterReady,'detail reads must not run inference');checks.push('capture async alarm preparation '+captureExpected+'; persisted detail reopening triggers zero model calls');
  await call('/items/'+first.id,'GET',undefined,404,other);
  assert.equal((await call('','POST',input)).item.id,first.id);
  await call('','POST',{...input,body:'changed replay'},409);
  await call('/items/'+first.id+'/prepare','POST',{version:999,operation_id:crypto.randomUUID(),text:'Change this'},409);
  const change={version:1,operation_id:crypto.randomUUID(),text:'INVALID_OUTPUT'};
  await call('/items/'+first.id+'/prepare','POST',change,202);
  const failed=await wait(first.id,'failed');assert.equal(failed.preparation.error,'invalid_preparation');checks.push('malformed external model response surfaces failed, never Sent');
  await call('/items/'+first.id+'/prepare','POST',{...change,text:'conflicting operation'},409);
  const retry={version:1,operation_id:crypto.randomUUID(),text:'Recover using only the supplied facts'};
  await call('/items/'+first.id+'/prepare','POST',retry,202);await waitCapture(first.id);
  const beforeReplay=await metrics();await call('/items/'+first.id+'/prepare','POST',retry);await new Promise(r=>setTimeout(r,100));assert.equal(await metrics(),beforeReplay);checks.push('natural-language reprepare, operation conflict, stale version rejection, recovery and idempotent replay');
  const blocked=(await call('','POST',{body:'Organize these notes: BLOCK_EXTERNAL: research an unknown external topic and propose a complete answer',operation_id:crypto.randomUUID()},201)).item;
  const blockedReady=await wait(blocked.id,'blocked');assert.equal(blockedReady.preparation.error,'external_research_required');checks.push('bounded-scope incomplete evidence is discoverably blocked, not falsely ready');
  const queue=await call('');assert.ok(queue.items.some(x=>x.id===blocked.id&&x.preparation.status==='blocked'));assert.ok(queue.items.some(x=>x.id===first.id&&x.preparation.status===captureExpected));
  // Actual shipped Worker research path, not a helper-only/unit fixture.
  const searchesBefore=researchTrace.length;
  const publicCapture=(await call('','POST',{body:'Compare current California health insurance plans using public coverage guidance.',operation_id:crypto.randomUUID()},201)).item;
  const publicResult=await wait(publicCapture.id,'blocked');
  assert.equal(publicResult.preparation.error,'complete_capture_proposal_unverified');
  assert.equal(publicResult.preparation.draft_id,null);assert.ok(publicResult.preparation.proposal);
  assert.equal(researchTrace.length,searchesBefore+1);
  assert.ok(publicResult.preparation.sources.some(source=>source.kind==='web'&&source.reference===publicSource));
  assert.match(publicResult.preparation.scope,/not a completed decision/);
  assert.equal(sendCalls,0);
  checks.push('actual Worker planner exact anonymous generic query -> fixed read-only search broker -> public snippet citation; model ready claim remains blocked/complete_capture_proposal_unverified, no external action');
  // Exact source/thread/recipient preparation and persisted complete draft.
  const seeded=await backend.fetch('https://fixture.test/__fixture',{method:'POST',body:JSON.stringify({user:owner,decision:{source_key:crypto.randomUUID(),title:'Review supplied summary',context:'Synthetic source',source_label:'Gmail',source_url:'https://mail.google.com/',choices:[{id:'reply',title:'Reply'}],source_connection_id:connection,source_thread_id:'tfixture',source_message_id:'mfixture',prepare:true}})});
  assert.equal(seeded.status,200);const decisionID=(await seeded.json()).id;
  let decision;
  for(let i=0;i<100;i++){decision=(await call('/decisions/'+decisionID)).decision;if(decision.preparation.status==='ready')break;await new Promise(r=>setTimeout(r,25));}
  assert.equal(decision.preparation.status,'ready');assert.ok(decision.preparation.draft_id);assert.equal(decision.status,'needs_you');
  const draft=(await call('/mail/drafts/'+decision.preparation.draft_id)).draft;
  assert.deepEqual(draft.to,['sender@example.test']);assert.equal(draft.thread_id,'tfixture');assert.equal(draft.reply_message_id,'mfixture');assert.equal(draft.body_text,'Thanks for sending the supplied summary.');assert.equal(draft.status,'draft');
  assert.equal(decision.preparation.prepared_draft.id,draft.id);assert.equal(decision.preparation.prepared_draft.version,draft.version);
  const beforeDecisionReads=await metrics();for(let i=0;i<5;i++) await call('/decisions/'+decisionID);assert.equal(await metrics(),beforeDecisionReads);assert.equal(sendCalls,0);
  checks.push('asynchronously prepared decision source/thread/recipient exact draft persisted, detail snapshot matches current version; opening does not run a model or send');
  const waitDecision=async(id,state='ready')=>{for(let i=0;i<500;i++){const d=(await call('/decisions/'+id)).decision;if(d.preparation.status===state)return d;await new Promise(r=>setTimeout(r,25));}throw Error('decision did not reach '+state);};
  const approval=draft=>({draft_id:draft.id,version:draft.version,operation_id:crypto.randomUUID()});
  const saveInput=draft=>Object.fromEntries(['id','version','connection_id','mode','to','cc','bcc','subject','body_text','thread_id','reply_message_id'].map(k=>[k,draft[k]]));
  // Preparation Change fences the old exact draft, even while the next alarm runs.
  const changed=await call('/decisions/'+decisionID+'/prepare','POST',{version:decision.version,operation_id:crypto.randomUUID(),text:'Change to a concise acknowledgment'},202);
  assert.equal(changed.version,2);
  await call('/mail/send','POST',approval(draft),409);assert.equal(sendCalls,0);
  const changedDecision=await waitDecision(decisionID), current=changedDecision.preparation.prepared_draft;
  assert.notEqual(current.id,draft.id);assert.equal(current.body_text,'Updated reply: Change to a concise acknowledgment');
  await call('/mail/send','POST',approval(draft),409);
  await call('/decisions/'+decisionID+'/prepare','POST',{version:1,operation_id:crypto.randomUUID(),text:'Stale target approval'},409);
  // Draft edits invalidate approval for the old exact version, not merely the decision.
  let edited=(await call('/mail/drafts','POST',{...saveInput(current),body_text:'Owner reviewed the exact fixture reply.'})).draft;
  assert.equal(edited.version,current.version+1);
  await call('/mail/send','POST',approval(current),409);
  assert.equal((await call('/decisions/'+decisionID)).decision.preparation.prepared_draft.version,edited.version);
  for(const mismatch of [{mode:'compose',thread_id:null,reply_message_id:null},{thread_id:'wrong_thread'},{reply_message_id:'wrong_message'}]){
    const mismatched=(await call('/mail/drafts','POST',{...saveInput(edited),...mismatch})).draft;
    assert.equal((await call('/mail/send','POST',approval(mismatched),409)).error,'stale_prepared_draft');
    edited=(await call('/mail/drafts','POST',{...saveInput(edited),version:mismatched.version})).draft;
  }
  const refresh=async(text)=>{const d=(await call('/decisions/'+decisionID)).decision;await call('/decisions/'+decisionID+'/prepare','POST',{version:d.version,operation_id:crypto.randomUUID(),text},202);return (await waitDecision(decisionID)).preparation.prepared_draft;};
  sourceBody='A changed content fact invalidates this proposal.';
  assert.equal((await call('/mail/send','POST',approval(edited),409)).error,'stale_source_context');assert.equal(sendCalls,0);sourceBody='Here is the supplied summary.';
  await call('/mail/send','POST',approval(edited),409); // Restoring source never resurrects old approval.
  let fresh=await refresh('Fresh review after changed content');
  newerMessage=true;assert.equal((await call('/mail/send','POST',approval(fresh),409)).error,'stale_source_context');newerMessage=false;
  for(const labels of [['INBOX','SPAM'],['INBOX','TRASH'],['INBOX','DRAFT'],['UNREAD']]){
    sourceLabels=['INBOX','UNREAD'];fresh=await refresh('Fresh source review before '+labels.join('_'));
    sourceLabels=labels;assert.equal((await call('/mail/send','POST',approval(fresh),409)).error,'stale_source_context');
  }
  sourceLabels=['INBOX','UNREAD'];fresh=await refresh('Fresh final review before fixture acceptance');
  const finalEdited=(await call('/mail/drafts','POST',{...saveInput(fresh),body_text:'Owner reviewed the exact fixture reply.'})).draft;
  sourceLabels=['INBOX']; // Read/unread change is not content; no provider mutation.
  checks.push('Change generation fences old/orphan draft; stale target and exact draft versions/mode/thread/source-message mismatch reject; changed content/newer message/spam/trash/draft/archive reject and persistently invalidate approval; restoring source cannot resurrect it');
  const send=approval(finalEdited), accepted=await call('/mail/send','POST',send);
  assert.equal(accepted.receipt.status,'sent');assert.equal(sendCalls,1);
  assert.equal(acceptedSnapshots[0].body_text,finalEdited.body_text);
  assert.equal((await call('/decisions/'+decisionID)).decision.status,'resolved');
  await call('/mail/send','POST',send);await call('/mail/send','POST',{...send,operation_id:crypto.randomUUID()});assert.equal(sendCalls,1);
  checks.push('fresh exact version approval reaches fixture provider once; MIME body is exact edited content; accepted receipt resolves decision; operation/draft replay never resends; label-only read change remains applicable');
  // Unknown receipt must survive destruction/reopening of the actual worker + SQLite store.
  const seededUnknown=await backend.fetch('https://fixture.test/__fixture',{method:'POST',body:JSON.stringify({user:owner,decision:{source_key:crypto.randomUUID(),title:'Unknown fixture',context:'Synthetic source',source_label:'Gmail',source_url:'https://mail.google.com/',choices:[{id:'reply',title:'Reply'}],source_connection_id:connection,source_thread_id:'tfixture',source_message_id:'mfixture',prepare:true}})});
  assert.equal(seededUnknown.status,200);const unknownID=(await seededUnknown.json()).id;
  const unknownDecision=await waitDecision(unknownID), unknownDraft=unknownDecision.preparation.prepared_draft, unknownApproval=approval(unknownDraft);
  unknownSend=true;assert.equal((await call('/mail/send','POST',unknownApproval)).receipt.status,'unknown');assert.equal(sendCalls,2);
  assert.equal((await call('/decisions/'+unknownID)).decision.status,'needs_you');
  modelEpochs.push(await metrics());await mf.dispose();mf=new Miniflare(options);backend=await mf.getWorker('managed');aiBinding=await mf.getWorker('ai','SyntheticAI');base=await mf.ready;
  assert.equal((await call('/items/'+deterministic.id)).item.preparation.proposal,formatted.preparation.proposal);
  assert.equal((await call('/mail/drafts/'+unknownDraft.id)).draft.status,'unknown');
  assert.equal((await call('/mail/drafts/'+finalEdited.id)).draft.status,'sent');
  assert.equal((await call('/mail/send','POST',unknownApproval)).receipt.status,'unknown');
  assert.equal((await call('/mail/send','POST',{...unknownApproval,operation_id:crypto.randomUUID()})).receipt.status,'unknown');
  await call('/decisions/'+unknownID+'/prepare','POST',{version:unknownDecision.version,operation_id:crypto.randomUUID(),text:'Do not replace unknown receipt'},409);
  await call('/mail/send','POST',{...unknownApproval,operation_id:send.operation_id},409);
  assert.equal(sendCalls,2);unknownSend=false;
  checks.push('fixture acceptance then 504 is unknown, not Sent/resolved; real Worker destroy/SQLite reopen retains unknown and sent receipts; same/new operation IDs never retry; Change cannot create replacement after unknown');
  // Durable alarms, bounded transient retry, terminal exhaustion and complete/reopen generations.
  await call('/items/'+first.id+'/prepare','POST',{version:1,operation_id:crypto.randomUUID(),text:'RETRY_ONCE'},202);
  await waitCapture(first.id);
  assert.equal((await (await aiBinding.fetch('https://fixture.test/metrics')).json()).attempts.RETRY_ONCE,2);
  await call('/items/'+first.id+'/prepare','POST',{version:1,operation_id:crypto.randomUUID(),text:'RETRY_EXHAUST'},202);
  const exhausted=await wait(first.id,'failed');assert.equal(exhausted.preparation.error,'preparation_unavailable');
  assert.equal((await (await aiBinding.fetch('https://fixture.test/metrics')).json()).attempts.RETRY_EXHAUST,3);
  const done=(await call('/items/'+first.id,'PATCH',{version:1,operation_id:crypto.randomUUID(),status:'done'})).item;
  assert.equal(done.preparation.status,'blocked');assert.equal(done.preparation.error,'capture_completed');
  await call('/items/'+first.id+'/prepare','POST',{version:1,operation_id:crypto.randomUUID(),text:'Old capture version'},409);
  const reopened=(await call('/items/'+first.id,'PATCH',{version:done.version,operation_id:crypto.randomUUID(),status:'captured'})).item;
  assert.equal(reopened.version,done.version+1);await waitCapture(first.id);
  assert.equal(sendCalls,2);
  checks.push('actual durable alarm transient inference failure retries twice then '+captureExpected+'; exhaustion bounded to three inference calls; complete invalidates result; versioned reopen enqueues fresh preparation without send');
  // A real asynchronous inference RPC races a newer Change and a source refresh.
  const seededRace=await backend.fetch('https://fixture.test/__fixture',{method:'POST',body:JSON.stringify({user:owner,decision:{source_key:crypto.randomUUID(),title:'Inference overlap fixture',context:'Synthetic source',source_label:'Gmail',source_url:'https://mail.google.com/',choices:[{id:'reply',title:'Reply'}],source_connection_id:connection,source_thread_id:'tfixture',source_message_id:'mfixture',prepare:true}})});
  assert.equal(seededRace.status,200);const raceID=(await seededRace.json()).id;
  let race=await waitDecision(raceID);
  const waitModel=async(instructions)=>{for(let i=0;i<100;i++){const a=(await (await aiBinding.fetch('https://fixture.test/metrics')).json()).attempts;if(a[instructions])return;await new Promise(r=>setTimeout(r,10));}throw Error('slow fixture model was not reached');};
  await call('/decisions/'+raceID+'/prepare','POST',{version:race.version,operation_id:crypto.randomUUID(),text:'SLOW_OLD_GENERATION'},202);
  await waitModel('SLOW_OLD_GENERATION');race=(await call('/decisions/'+raceID)).decision;
  await call('/decisions/'+raceID+'/prepare','POST',{version:race.version,operation_id:crypto.randomUUID(),text:'Newest generation only'},202);
  const raceReady=await waitDecision(raceID);assert.equal(raceReady.preparation.prepared_draft.body_text,'Updated reply: Newest generation only');
  await new Promise(r=>setTimeout(r,500));
  assert.equal((await call('/decisions/'+raceID)).decision.preparation.draft_id,raceReady.preparation.draft_id);
  await call('/decisions/'+raceID+'/prepare','POST',{version:raceReady.version,operation_id:crypto.randomUUID(),text:'SLOW_SOURCE_CHANGE'},202);
  await waitModel('SLOW_SOURCE_CHANGE');sourceBody='Fact changed while model inference was in flight';
  const raceBlocked=await waitDecision(raceID,'blocked');assert.equal(raceBlocked.preparation.error,'stale_source_context');assert.equal(raceBlocked.preparation.draft_id,null);
  sourceBody='Here is the supplied summary.';assert.equal(sendCalls,2);
  checks.push('actual inference RPC overlapped by newer Change cannot publish old generation; source content changed during inference blocks before any generated draft/send');
  modelEpochs.push(await metrics());
  const sorted=timings.sort((a,b)=>a-b);
  assert.equal(sendCalls,2,'exactly acceptance + accepted-then-504 fixture sends');
  await writeFile(join(output,'local-preparation-result.json'),JSON.stringify({command:'node js/managed/scripts/decision-inbox-preparation-http.mjs'+(safetyGatedCaptures?' --safety-gated-captures':''),status:safetyGatedCaptures?'safety_gates_passed':'passed',product_acceptance:safetyGatedCaptures?'HOLD':'PASS',product_hold_reason:safetyGatedCaptures?'capture_full_completion_unimplemented':null,safety_gated_captures:safetyGatedCaptures,started_at:startedAt,finished_at:new Date().toISOString(),bundle_sha256:bundleDigest,source_head_at_bundle_start:sourceHead,source_head_at_bundle_end:sourceHeadAfterBundle,source_head_at_test_end:spawnSync('git',['rev-parse','HEAD'],{encoding:'utf8'}).stdout.trim(),public_research_fixture:researchTrace,model_calls_by_worker_epoch:modelEpochs,model_calls_total:modelEpochs.reduce((a,b)=>a+b,0),checks,auth:'Synthetic account owner and API key through shipped ensureAccount/createApiKey/authenticate; never live credentials',transport:'Loopback HTTP -> shipped account proxy -> shipped TODO router -> actual UserAccount SQLite DO and alarms',external_fixture:'Workers AI RPC output and Google/connector broker only',read_snapshot_ms:{n:sorted.length,p50:sorted[Math.ceil(sorted.length*.5)-1],p95:sorted[Math.ceil(sorted.length*.95)-1]},provider_send_calls:sendCalls,live_provider_send_calls:0,fixture_provider_snapshots:acceptedSnapshots,branch_deployed:false,revision:{base:'5be647ae2',source:'recorded current integrated HEAD plus explicitly scoped harness; not deployed'},interruptions:'Worker restarted after unknown receipt; newer-generation inference overlap is tested. Active same-generation lease interruption/attempt-overlap is not claimed by this HTTP harness'},null,2)+'\n');
  console.log(JSON.stringify({status:safetyGatedCaptures?'safety_gates_passed':'passed',product_acceptance:safetyGatedCaptures?'HOLD':'PASS',product_hold_reason:safetyGatedCaptures?'capture_full_completion_unimplemented':null,checks,model_calls_total:modelEpochs.reduce((a,b)=>a+b,0),evidence:join(output,'local-preparation-result.json')},null,2));
} catch(e){console.error(e.message);process.exitCode=1;await writeFile(join(output,'local-preparation-result.json'),JSON.stringify({status:'failed',product_acceptance:'HOLD',product_hold_reason:'capture_full_completion_unimplemented',safety_gated_captures:safetyGatedCaptures,source_head_at_bundle_start:sourceHead,source_head_at_bundle_end:sourceHeadAfterBundle,started_at:startedAt,bundle_sha256:bundleDigest,failure:e.message,checks,provider_send_calls:sendCalls,live_provider_send_calls:0,branch_deployed:false},null,2)+'\n');}
finally{await writeFile(join(output,'local-preparation-trace.json'),JSON.stringify({trace,provider_trace:providerTrace,provider_send_calls:sendCalls,accepted_snapshots:acceptedSnapshots},null,2)+'\n');if(mf)await mf.dispose();await rm(store,{recursive:true,force:true});}
