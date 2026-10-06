// Synthetic real Chrome + production login runtime + encrypted workerd broker.
// node --experimental-strip-types js/managed/test/browser-injection.chrome.mjs
import assert from 'node:assert/strict';
import https from 'node:https';
import {readFileSync,writeFileSync,mkdirSync,mkdtempSync,readdirSync,rmSync} from 'node:fs';
import {spawn,execFileSync} from 'node:child_process';
import {registerHooks} from 'node:module';
import {resolve,join} from 'node:path';
import WebSocket from 'ws';
import {encryptedVaultFixture} from './private-input-vault-fixture.mjs';
registerHooks({resolve(specifier,ctx,next){
 if(specifier==='agents/browser')return {url:'data:text/javascript,export const createBrowserSession=(b,o)=>b.create(o);export const deleteBrowserSession=(b,id)=>b.delete?.(id);',shortCircuit:true};
 return next(specifier.startsWith('./browser-')&&!specifier.endsWith('.ts')?specifier+'.ts':specifier,ctx);
}});
const {createBrowserLoginRuntime}=await import('../src/browser-login-runtime.ts');
const packages=new URL('../../../node_modules/.pnpm/',import.meta.url);
const entry=readdirSync(packages).find(name=>/^playwright-core@/.test(name));
const {chromium}=await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`,packages));
const output=resolve(process.env.BROWSER_INJECTION_EVIDENCE||'output/browser-injection');mkdirSync(output,{recursive:true});
const temp=mkdtempSync(join(output,'run-'));
const trace={boundaries:['production BrowserLoginRuntime model tools and human submit','real Chrome native DOM/CDP','production workerd egress and encrypted Durable Object broker'],adapters:['local browser allocation','in-memory browser metadata storage','synthetic site and identities'],assertions:[]};
const pass=name=>{trace.assertions.push(name);console.log('PASS '+name);writeFileSync(join(output,'trace.json'),JSON.stringify(trace,null,2)+'\n');};
const delay=ms=>new Promise(r=>setTimeout(r,ms));
const wait=async(fn)=>{for(let i=0;i<300;i++){if(await fn())return;await delay(50);}throw Error('Fixture wait timed out');};
const durable=new Map(),storage={get:async k=>structuredClone(durable.get(k)),put:async(k,v)=>durable.set(k,structuredClone(v)),delete:async k=>durable.delete(k),transaction:async f=>f(storage)};
const ctx={sessionId:'fixture-owner',signal:new AbortController().signal};
let chrome,browser,site,runtime,vault,requestId,page,origin,resolutions=0,saves=0,resolveHook,loseSaveReply=false;
const saveOperations=[];
const tool=(name,args)=>runtime.tools.find(t=>t.name===name).handler(args,ctx);
const fields=[['username','username'],['password','current-password'],['api_key','off'],['card_number','cc-number'],['expiry_month','cc-exp-month'],['expiry_year','cc-exp-year'],['billing_zip','postal-code'],['address_line_1','address-line1'],['address_line_2','address-line2'],['city','address-level2'],['state','address-level1'],['zip','postal-code'],['country','country'],['phone_number','tel'],['otp','one-time-code'],['cvv','cc-csc']];
const kinds=[
 {kind:'login',payload:{name:'Synthetic login',username:'fixture@example.test',password:'fixture-injection-password'}},
 {kind:'api_key',payload:{name:'Synthetic API',api_key:'fixture-injection-api-key'}},
 {kind:'card',payload:{name:'Synthetic card',card_number:'4111111111111111',expiry_month:'09',expiry_year:'2031',billing_zip:'10001'}},
 {kind:'address',payload:{name:'Synthetic address',address_line_1:'1 Fixture Way',address_line_2:'Suite Fixture',city:'Fixture City',state:'Fixture State',zip:'10558',country:'GR'}},
 {kind:'phone',payload:{name:'Synthetic phone',phone_number:'+306900000000'}},
];
async function fresh(){
 if(requestId){await tool('browser_login_close',{});await page.close();}
 const hint=await tool('request_browser_login',{operation_id:crypto.randomUUID(),url:origin,allowed_origins:[origin],defer_input:true});requestId=hint.request_id;
 await wait(()=>{page=browser.contexts().flatMap(c=>c.pages()).find(p=>p.url()===origin+'/'&&!p.isClosed());return page;});
 await page.locator('#username').waitFor();return tool('browser_login_snapshot',{request_id:requestId});
}
const mapping=(snapshot,item,only)=>Object.keys(item.payload).filter(f=>f!=='name'&&(!only||only.includes(f))).map(field=>({field,vault_id:item.id,ref:snapshot.elements.find(e=>e.text===field)?.ref}));
try{
 vault=await encryptedVaultFixture();
 for(const item of kinds){const saved=await vault.save({operation_id:crypto.randomUUID(),kind:item.kind,payload:item.payload});item.id=saved.id;}
 execFileSync('openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',join(temp,'key'),'-out',join(temp,'cert'),'-days','1','-subj','/CN=localhost'],{stdio:'ignore'});
 site=https.createServer({key:readFileSync(join(temp,'key')),cert:readFileSync(join(temp,'cert'))},(_req,res)=>{
  res.setHeader('content-type','text/html');res.end(`<html lang="en"><body><form action="/submit" onsubmit="event.preventDefault()">${fields.map(([f,h])=>`<label for="${f}">${f}</label>${f==='country'?'<select id="country" autocomplete="country"><option value="">Choose</option><option value="GR">Greece</option></select>':f==='expiry_month'?'<select id="expiry_month" autocomplete="cc-exp-month"><option value="">Choose</option><option value="9">09</option></select>':`<input id="${f}" autocomplete="${h}" type="${f==='password'?'password':'text'}">`}<br>`).join('')}<button>Submit</button></form><div id="echo"></div><script>window.inputCount=0;document.addEventListener('input',()=>inputCount++);</script></body></html>`);
 });await new Promise(r=>site.listen(0,'127.0.0.1',r));origin=`https://127.0.0.1:${site.address().port}`;
 chrome=spawn(process.env.CHROME_PATH||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',[...(process.platform==='linux'?['--no-sandbox']:[]),'--headless','--ignore-certificate-errors','--no-first-run','--no-default-browser-check','--remote-debugging-port=0',`--user-data-dir=${join(temp,'chrome')}`,'about:blank'],{stdio:'ignore'});
 await wait(()=>{try{return !!readFileSync(join(temp,'chrome','DevToolsActivePort'));}catch{return false;}});
 const [port,endpoint]=readFileSync(join(temp,'chrome','DevToolsActivePort'),'utf8').trim().split('\n');browser=await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
 const binding={create:async()=>({sessionId:'fixture-browser'}),delete:async()=>{},fetch:async()=>{const socket=new WebSocket(`ws://127.0.0.1:${port}${endpoint}`);await new Promise((r,j)=>{socket.once('open',r);socket.once('error',j);});socket.accept=()=>{};return {webSocket:socket};}};
 runtime=createBrowserLoginRuntime({storage,browser:binding,agentId:crypto.randomUUID(),authorize:context=>assert.equal(context.sessionId,ctx.sessionId),savePrivateVault:async e=>{saves++;saveOperations.push(e.operation_id);const result=await vault.save(e);if(loseSaveReply){loseSaveReply=false;throw Error('Synthetic lost save reply');}return result;},resolveVaultFields:async args=>{resolutions++;const result=await vault.materialize(args);if(resolveHook)await resolveHook();return result;}});
 for(const item of kinds){
  const snap=await fresh(),operation_id=crypto.randomUUID(),args={request_id:requestId,operation_id,snapshot_id:snap.snapshot_id,fields:mapping(snap,item)};
  const result=await tool('browser_login_inject_fields',args);assert.deepEqual(result,{status:'filled'});
  for(const [field,value] of Object.entries(item.payload).filter(([f])=>f!=='name'))assert.equal(await page.locator('#'+field).inputValue(),field==='expiry_month'?'9':value);
  const before=await page.evaluate(()=>inputCount);assert.deepEqual(await tool('browser_login_inject_fields',args),result);assert.equal(await page.evaluate(()=>inputCount),before);
  await page.evaluate(()=>document.querySelector('#echo').textContent=[...document.querySelectorAll('input')].map(e=>e.value).join(' '));
  const view=JSON.stringify(await tool('browser_login_snapshot',{request_id:requestId}));
  for(const value of Object.values(item.payload).filter(v=>v.length>6))assert.ok(!view.includes(value),'redacted values');
  pass(item.kind+' model injection, native values, same-operation replay, redacted snapshot');
 }
 const login=kinds[0],phone=kinds[4];
 let snap=await fresh();const before=resolutions;await page.locator('#password').evaluate(e=>e.autocomplete='one-time-code');
 assert.equal((await tool('browser_login_inject_fields',{request_id:requestId,operation_id:crypto.randomUUID(),snapshot_id:snap.snapshot_id,fields:mapping(snap,login)})).status,'outcome_unknown');
 assert.equal(resolutions,before);assert.equal(await page.locator('#username').inputValue(),'');pass('stale snapshot rejected before broker resolution and any DOM injection');
 snap=await fresh();resolveHook=()=>page.locator('#password').evaluate(e=>e.disabled=true);
 assert.equal((await tool('browser_login_inject_fields',{request_id:requestId,operation_id:crypto.randomUUID(),snapshot_id:snap.snapshot_id,fields:mapping(snap,login)})).status,'outcome_unknown');resolveHook=undefined;
 assert.equal(await page.locator('#username').inputValue(),'');pass('document mutation during broker resolution rejects entire batch before first fill');
 snap=await fresh();await page.locator('#password').evaluate(e=>e.addEventListener('change',()=>document.querySelector('#username').value=''));
 assert.deepEqual(await tool('browser_login_inject_fields',{request_id:requestId,operation_id:crypto.randomUUID(),snapshot_id:snap.snapshot_id,fields:mapping(snap,login)}),{status:'outcome_unknown'});
 pass('input handler clearing earlier value yields outcome_unknown');
 snap=await fresh();const beforeOtp=resolutions;
 assert.equal((await tool('browser_login_inject_fields',{request_id:requestId,operation_id:crypto.randomUUID(),snapshot_id:snap.snapshot_id,fields:[{ref:snap.elements.find(e=>e.text==='otp').ref,field:'api_key',vault_id:kinds[1].id}]})).status,'outcome_unknown');assert.equal(resolutions,beforeOtp);pass('OTP field cannot receive reusable API key');
 snap=await fresh();const hint=await tool('request_browser_login_input',{request_id:requestId,operation_id:crypto.randomUUID(),snapshot_id:snap.snapshot_id,fields:mapping(snap,phone).map(({ref})=>({ref}))});requestId=hint.request_id;
 await runtime.submit({challenge_id:requestId,action:'approve'},ctx.signal);
 const observed=await runtime.submit({challenge_id:requestId,action:'observe',native_fields:true,native_field_hints:true,native_field_controls:true},ctx.signal);
 const human={challenge_id:requestId,action:'fill_vault_fields',document_id:observed.native_form.document_id,fields:[{ref:observed.native_form.fields[0].ref,vault_id:phone.id,field:'phone_number'}]};
 await runtime.submit(human,ctx.signal);assert.equal(await page.locator('#phone_number').inputValue(),phone.payload.phone_number);await assert.rejects(runtime.submit(human,ctx.signal));
 await runtime.submit({challenge_id:requestId,action:'finish'},ctx.signal);assert.equal(saves,0);pass('human Vault reuse is single-use, finishes, and never resaves item');
 for(const item of kinds){
  snap=await fresh();const chosen=mapping(snap,item);
  const panel=await tool('request_browser_login_input',{request_id:requestId,operation_id:crypto.randomUUID(),snapshot_id:snap.snapshot_id,fields:chosen.map(({ref})=>({ref}))});requestId=panel.request_id;
  await runtime.submit({challenge_id:requestId,action:'approve'},ctx.signal);
  const sheet=await runtime.submit({challenge_id:requestId,action:'observe',native_fields:true,native_field_hints:true,native_field_controls:true},ctx.signal);
  const roles={},values=sheet.native_form.fields.map((f,i)=>{roles[f.ref]=chosen[i].field;return {ref:f.ref,value:f.type==='select'?'1':item.payload[chosen[i].field]};});
  await runtime.submit({challenge_id:requestId,action:'fill_fields',document_id:sheet.native_form.document_id,fields:values,save_to_vault:true,save_details:{fields:roles}},ctx.signal);
  const receipt=await runtime.submit({challenge_id:requestId,action:'finish'},ctx.signal);assert.equal(receipt.vault_save.status,'saved');assert.equal(receipt.vault_save.items[0].kind,item.kind);
  const count=saves;await runtime.submit({challenge_id:requestId,action:'finish'},ctx.signal);assert.equal(saves,count);
  const saved=await vault.materialize({vault_id:receipt.vault_save.items[0].id,expected_origin:origin,fields:chosen.map(f=>f.field)});
  for(const f of chosen)assert.equal(saved.values[f.field],f.field==='expiry_month'?'9':item.payload[f.field]);
  assert.ok(!JSON.stringify(receipt).includes(item.payload.password||item.payload.api_key||item.payload.card_number||item.payload.address_line_1||item.payload.phone_number));
  pass(item.kind+' native autosave, actual selected option values, save once, encrypted reuse');
 }
 // Lifecycle journeys retain the real private browser across rotating panels.
 const panelFor=async names=>{
  const current=await tool('browser_login_snapshot',{request_id:requestId});
  const panel=await tool('request_browser_login_input',{request_id:requestId,operation_id:crypto.randomUUID(),snapshot_id:current.snapshot_id,fields:names.map(name=>({ref:current.elements.find(e=>e.text===name).ref}))});
  requestId=panel.request_id;await runtime.submit({challenge_id:requestId,action:'approve'},ctx.signal);
  return runtime.submit({challenge_id:requestId,action:'observe',native_fields:true,native_field_hints:true,native_field_controls:true},ctx.signal);
 };
 const fillPanel=async (sheet,values,save=true,roles)=>runtime.submit({challenge_id:requestId,action:'fill_fields',document_id:sheet.native_form.document_id,fields:sheet.native_form.fields.map((f,i)=>({ref:f.ref,value:values[i]})),...(save===undefined?{}:{save_to_vault:save}),...(roles?{save_details:{fields:Object.fromEntries(sheet.native_form.fields.map((f,i)=>[f.ref,roles[i]]))}}:{})},ctx.signal);
 const finish=()=>runtime.submit({challenge_id:requestId,action:'finish'},ctx.signal);
 await fresh();let sheet=await panelFor(['username']);await fillPanel(sheet,['two-step@example.test']);assert.equal((await finish()).vault_save.status,'not_saved');
 sheet=await panelFor(['password']);await fillPanel(sheet,['two-step-password']);let receipt=await finish();assert.equal(receipt.vault_save.status,'saved');
 let material=await vault.materialize({vault_id:receipt.vault_save.items[0].id,expected_origin:origin,fields:['username','password']});assert.equal(material.values.username,'two-step@example.test');assert.equal(material.values.password,'two-step-password');
 sheet=await panelFor(['password']);await fillPanel(sheet,['second-password']);assert.equal((await finish()).vault_save.status,'not_saved');pass('two-step request rotation joins username/password and success clears carryover');
 await fresh();sheet=await panelFor(['username']);await fillPanel(sheet,['optout@example.test']);await finish();sheet=await panelFor(['password']);await fillPanel(sheet,['optout-password'],false);assert.equal((await finish()).vault_save,undefined);
 sheet=await panelFor(['password']);await fillPanel(sheet,['after-optout-password']);assert.equal((await finish()).vault_save.status,'not_saved');pass('opt-out prevents save and clears prior two-step username');
 await fresh();sheet=await panelFor(['username','password']);await fillPanel(sheet,['cancel@example.test','cancel-password']);const beforeCancel=saves;await runtime.submit({challenge_id:requestId,action:'cancel'},ctx.signal);await assert.rejects(runtime.submit({challenge_id:requestId,action:'retry_vault_save'},ctx.signal));assert.equal(saves,beforeCancel);pass('cancel discards staged reusable values and denies save-only retry');
 await fresh();sheet=await panelFor(['otp','cvv']);await fillPanel(sheet,['123456','123'],true,['api_key','password']);assert.equal((await finish()).vault_save.status,'not_saved');pass('OTP and CVC stay transient despite explicit reusable role classification');
 await fresh();sheet=await panelFor(['username','password']);await fillPanel(sheet,['retry@example.test','retry-password']);const inputs=await page.evaluate(()=>inputCount);loseSaveReply=true;receipt=await finish();assert.equal(receipt.vault_save.status,'failed');assert.equal(receipt.vault_save.retryable,true);const attempts=saves;
 assert.equal((await finish()).vault_save.status,'failed');assert.equal(saves,attempts);
 receipt=await runtime.submit({challenge_id:requestId,action:'retry_vault_save'},ctx.signal);assert.equal(receipt.vault_save.status,'saved');assert.equal(saveOperations.at(-1),saveOperations.at(-2));assert.equal(await page.evaluate(()=>inputCount),inputs);assert.equal(saves,attempts+1);
 await runtime.submit({challenge_id:requestId,action:'retry_vault_save'},ctx.signal);assert.equal(saves,attempts+1);pass('lost broker reply retries only save with same operation ID, no duplicate entry or DOM events');
 await fresh();sheet=await panelFor(['username','password']);await page.locator('#password').evaluate(e=>e.addEventListener('change',()=>document.querySelector('#username').value=''));
 const beforePartial=saves;await assert.rejects(fillPanel(sheet,['partial@example.test','partial-password']));assert.equal((await finish()).vault_save,undefined);assert.equal(saves,beforePartial);pass('earlier field cleared by page handler prevents whole-batch autosave');
 await fresh();sheet=await panelFor(['username','password']);await page.locator('form').evaluate(e=>e.action='https://other.example/');await assert.rejects(fillPanel(sheet,['stale@example.test','stale-password']));assert.equal((await finish()).vault_save,undefined);pass('changed form destination prevents fill and staging');
 await fresh();sheet=await panelFor(['username','password']);await fillPanel(sheet,['expire@example.test','expire-password']);const realNow=Date.now,beforeExpiry=saves;try{const later=realNow()+11*60_000;Date.now=()=>later;await runtime.expire();}finally{Date.now=realNow;}
 await assert.rejects(runtime.submit({challenge_id:requestId,action:'retry_vault_save'},ctx.signal));assert.equal(saves,beforeExpiry);pass('expiry discards pending save and username carryover');
 const rows=await vault.encryptedRows();assert.equal(rows.length,12);assert.ok(rows.every(r=>r.encrypted));pass('all five persisted Vault entries encrypted at rest');
 trace.status='passed';writeFileSync(join(output,'trace.json'),JSON.stringify(trace,null,2)+'\n');
}catch(error){console.error(error);throw error;}finally{await runtime?.close();await browser?.close();if(chrome&&chrome.exitCode===null){chrome.kill();await new Promise(r=>chrome.once('exit',r));}await new Promise(r=>site?site.close(r):r());await vault?.close();rmSync(temp,{recursive:true,force:true,maxRetries:5,retryDelay:100});}
