// Real shipped TUI PTY -> private HTTP runtime -> real Chrome synthetic page.
// Repro: NANOCODEX_TEST_BINARY=/absolute/path/nanocodex2 node --experimental-transform-types js/managed/test/private-input-tui.chrome.mjs
// Only the model event stream, durable storage and Cloudflare browser allocation
// are local adapters. Private runtime validation/redaction and DOM input are real.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import https from 'node:https';
import http from 'node:http';
import {readFileSync,writeFileSync,mkdirSync,mkdtempSync,readdirSync,rmSync,existsSync} from 'node:fs';
import {spawn,execFileSync} from 'node:child_process';
import {registerHooks} from 'node:module';
import {resolve,join} from 'node:path';
import WebSocket,{WebSocketServer} from 'ws';
import xterm from '@xterm/headless';
import {encryptedVaultFixture} from './private-input-vault-fixture.mjs';
const adapter=`export const createBrowserSession=(b,o)=>b.create(o);export const deleteBrowserSession=(b,id)=>b.delete?.(id);
export const connectBrowser=()=>{throw Error('Unused public browser adapter')};
export class DurableBrowserSessionStore {
 constructor(storage){this.storage=storage}
 get(k){return this.storage.get(k)} set(k,v){return this.storage.put(k,v)} delete(k){return this.storage.delete(k)}
 async acquireLock(){return {release:async()=>{}}}
}`;
registerHooks({resolve(specifier,ctx,next){
 if(specifier==='agents/browser/ai')return {url:'data:text/javascript,export const createBrowserRuntime=()=>{throw Error("Provide local browser allocation")};',shortCircuit:true};
 if(specifier==='agents/browser')return {url:'data:text/javascript,'+encodeURIComponent(adapter),shortCircuit:true};
 return next(specifier.startsWith('./browser-')&&!specifier.endsWith('.ts')?specifier+'.ts':specifier,ctx);
}});
const {createBrowserLoginRuntime}=await import('../src/browser-login-runtime.ts');
const {createManagedBrowserRuntime}=await import('../src/browser-runtime.ts');
const packages=new URL('../../../node_modules/.pnpm/',import.meta.url);
const entry=readdirSync(packages).find(name=>/^playwright-core@/.test(name));
const {chromium}=await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`,packages));
const output=resolve(process.env.PRIVATE_INPUT_EVIDENCE||'output/private-input-tui');mkdirSync(output,{recursive:true});
const temp=mkdtempSync(join(output,'run-'));
const chromePath=process.env.CHROME_PATH||[...(process.platform==='darwin'?['/Applications/Google Chrome.app/Contents/MacOS/Google Chrome']:['/usr/bin/google-chrome','/usr/bin/google-chrome-stable','/usr/bin/chromium','/usr/bin/chromium-browser'])].find(existsSync);
assert.ok(chromePath,'Set CHROME_PATH to installed Chrome/Chromium');
const binary=resolve(process.env.NANOCODEX_TEST_BINARY||'target/debug/nanocodex');
const agent='019fc927-b280-79a7-8445-1b9996ad2fb0';
const key=`ncx_live_${'a'.repeat(12)}_${'b'.repeat(43)}`;
const route=`/v1/agents/${agent}`;
const trace={binary_sha256:createHash('sha256').update(readFileSync(binary)).digest('hex'),command:`NANOCODEX_TEST_BINARY=${binary} node --experimental-transform-types js/managed/test/private-input-tui.chrome.mjs`,boundaries:['shipped nanocodex2 attach in PTY','fixture-authenticated loopback HTTP (not production account admission)','production BrowserLoginRuntime and ManagedBrowserRuntime','real Chrome CDP and DOM','production egress Vault broker and encrypted Durable Object'],adapters:['synthetic model WebSocket events','in-memory private-browser metadata storage','local browser allocation','fixture HTTP bearer admission'],assertions:[],requests:[],receipts:[],save_retries:[]};
const pass=(name,details={})=>{trace.assertions.push({name,...details});console.log('PASS '+name);writeFileSync(join(output,'trace.json'),JSON.stringify(trace,null,2)+'\n');};
const delay=ms=>new Promise(r=>setTimeout(r,ms));
let fixtureError;
const wait=async(fn,stage,ms=15000)=>{const until=Date.now()+ms;while(!fn()){if(fixtureError)throw fixtureError;if(Date.now()>until)throw Error('Timed out '+stage);await delay(25);}};
const durable=new Map(),saveCalls=[],privateValues=[];let vault;
// Short numeric fixture codes can occur inside independent UUIDs, safety tokens,
// timestamps or timing logs. A complete numeric/token value is still forbidden;
// longer private inputs retain the stricter substring check everywhere.
const containsPrivateValue=(text,value)=>/^\d{3,6}$/.test(value)
 ? new RegExp(`(?<![A-Za-z0-9])${value}(?![A-Za-z0-9])`).test(text) : text.includes(value);
function assertNoPrivateInput(value,boundary){
 if(typeof value==='string'){
  for(const secret of privateValues)assert.ok(!containsPrivateValue(value,secret),boundary+' must omit private input');
 }else if(typeof value==='number'){
  for(const secret of privateValues)assert.ok(String(value)!==secret,boundary+' must omit numeric private input fields');
 }else if(value&&typeof value==='object'){
  for(const [key,child] of Object.entries(value)){assertNoPrivateInput(key,boundary);assertNoPrivateInput(child,boundary);}
 }
}

const storage={get:async k=>structuredClone(durable.get(k)),put:async(k,v)=>durable.set(k,structuredClone(v)),delete:async k=>durable.delete(k),transaction:async f=>f(storage)};
const ctx={sessionId:'synthetic-owner',callId:'private-input-fixture',signal:new AbortController().signal};
let chrome,browser,site,api,terminal,runtime,managedRuntime;
let screen='',stderr='',cursor=0,activeId='',activePage,ws;
const incomingMessages=[],modelResumptions=[],intakeReceipts=[];const takeoverIds=new Set();const browserPosts=[];let failNextSave=false,saveAttempts=[];
const wss=new WebSocketServer({noServer:true});
const metadata={agent_id:agent,session_id:agent,has_snapshot:false,completed_turns:1,last_active:1,agent_loaded:true,connected_clients:1,active_turns:[],active_turn_details:[],capabilities:{durable_turns:true,resumable_events:true,workspace:'cloudflare-computer',execution_environments:true,execution_namespace:'cwd-root-v1',native_cross_mounts:false},settings:{model:'gpt-6-astra',thinking:'low',reasoning_mode:'standard',fast_mode:false},latest_event_cursor:'0',stream_error:null};
const tools=(name,args)=>runtime.tools.find(t=>t.name===name).handler(args,ctx);
const managedTool=(name,args)=>managedRuntime.tools.find(t=>t.name===name).handler(args,ctx);
const event=(kind,payload,turn='private-turn')=>{const seq=++cursor;const v={cursor:String(seq),created_at:Date.now()/1000,turn_id:turn,type:'event',event:{protocol_version:1,request_id:turn,seq,type:kind,payload}};ws.send(JSON.stringify(v));return v;};
function requestPanel(hint,name='request_browser_login',turn='private-turn'){
 const call_id='private-call-'+cursor;
 event('tool.call',{call_id,tool:name,arguments:{}},turn);
 const result=event('tool.result',{call_id,tool:name,status:'completed',result:hint,structured_result:hint,metadata:null,duration_ns:1},turn);
 return result;
}
const type=text=>terminal.stdin.write(`\x1b[200~${text}\x1b[201~`);
const keypress=async bytes=>{terminal.stdin.write(bytes);await delay(120);};
async function httpControl(body,credential=key,requestAgent=agent){
 const response=await fetch(`http://127.0.0.1:${api.address().port}/v1/agents/${requestAgent}/browser-vault/takeover`,{method:'POST',headers:{authorization:`Bearer ${credential}`,'content-type':'application/json'},body:JSON.stringify(body)});
 return {status:response.status,body:await response.json()};
}
async function newLogin(path='/login'){
 if(activeId){await tools('browser_login_close',{});if(activePage&&!activePage.isClosed())await activePage.close();}
 const operation=crypto.randomUUID();
 const hint=await tools('request_browser_login',{operation_id:operation,url:siteOrigin+path,allowed_origins:[siteOrigin]});
 activeId=hint.request_id;
 if(!browser)browser=await chromium.connectOverCDP(chromeEndpoint);
 await wait(()=>{activePage=browser.contexts().flatMap(c=>c.pages()).find(p=>p.url()===siteOrigin+path&&!p.isClosed());return activePage;},'fixture page');
 await activePage.locator('input').first().waitFor();
 return hint;
}
const ctrlEnter='\x1b[13;5u';
// Match share-pty-bridge.py's window exactly; Ratatui sends cursor-addressed
// differential updates, so stripping ANSI does not reconstruct visible text.
const {Terminal}=xterm;
const emulator=new Terminal({cols:160,rows:32,allowProposedApi:true,scrollback:1000});
const visibleText=()=>Array.from({length:emulator.rows},(_,row)=>emulator.buffer.active.getLine(emulator.buffer.active.viewportY+row)?.translateToString(true)||'').join('\n');
let rendered=Promise.resolve(),lastSafetyToken='';
const capture=()=>{let text=visibleText().replace(/(Type safety token \(keys only\): )([a-f0-9]{32})/g,'$1[redacted safety token]');for(const value of privateValues)text=text.replaceAll(value,'[redacted private input]');writeFileSync(join(output,'terminal.txt'),text);};


let siteOrigin,chromeEndpoint;
try{
 vault=await encryptedVaultFixture();
 execFileSync('openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',join(temp,'key'),'-out',join(temp,'cert'),'-days','1','-subj','/CN=localhost'],{stdio:'ignore'});
 site=https.createServer({key:readFileSync(join(temp,'key')),cert:readFileSync(join(temp,'cert'))},async(req,res)=>{
  if(req.url==='/submitted'){let body='';for await(const b of req)body+=b;browserPosts.push(body);res.end('Synthetic account signed in');return;}
  res.setHeader('content-type','text/html');
  const custom=({
   '/secure-card':'<label>Card number<input id="pan" autocomplete="cc-number"></label><label>Expiry<input id="expiry" autocomplete="cc-exp"></label><label>Billing postal code<input id="zip" autocomplete="postal-code"></label><label>Card security code<input id="cvc" autocomplete="cc-csc"></label>',
   '/reuse-api':'<label>API key<input id="api_key"></label>',
   '/reuse-card':'<label>Card number<input id="card_number" autocomplete="cc-number"></label>',
   '/reuse-address':'<label>Address<input id="address_line_1" autocomplete="address-line1"></label>',
   '/reuse-phone':'<label>Phone<input id="phone_number" type="tel" autocomplete="tel"></label>',
  })[req.url];
  const fields=custom??(req.url==='/username'?'<label>Username<input id="username" autocomplete="username"></label>':req.url==='/password'?'<label>Password<input id="password" type="password" autocomplete="current-password"></label>':req.url==='/otp'?'<label>Verification code<input id="otp" name="otp" autocomplete="one-time-code" inputmode="numeric"></label>':req.url==='/cvc'?'<label>Card security code<input id="cvc" autocomplete="cc-csc" inputmode="numeric"></label>':req.url==='/profile'?'<label>Contact email<input id="email" type="email"></label><label>Country<select id="country"><option value="us">United States</option><option value="ca">Canada</option></select></label><label>Updates<input id="updates" type="checkbox"></label><label>Notes<textarea id="notes"></textarea></label>':'<label>Username<input id="username" autocomplete="username"></label><label>Password<input id="password" type="password" autocomplete="current-password"></label>');
  res.end(`<html><body><h1>Synthetic private input</h1><form method="post" action="/submitted">${fields}<button type="submit">Sign in</button></form><script>window.counts={input:0,change:0};document.addEventListener('input',()=>counts.input++);document.addEventListener('change',()=>counts.change++);</script></body></html>`);
 });
 await new Promise(r=>site.listen(0,'127.0.0.1',r));siteOrigin=`https://127.0.0.1:${site.address().port}`;
 chrome=spawn(chromePath,[...(process.platform==='linux'?['--no-sandbox']:[]),'--headless','--ignore-certificate-errors','--no-first-run','--no-default-browser-check','--remote-debugging-port=0',`--user-data-dir=${join(temp,'chrome')}`,'about:blank'],{stdio:'ignore'});
 await wait(()=>{try{return !!readFileSync(join(temp,'chrome','DevToolsActivePort'))}catch{return false}},'Chrome startup');
 const [port,endpoint]=readFileSync(join(temp,'chrome','DevToolsActivePort'),'utf8').trim().split('\n');chromeEndpoint=`http://127.0.0.1:${port}`;
 const binding={create:async()=>({sessionId:'synthetic-local-browser'}),delete:async()=>{},fetch:async()=>{const socket=new WebSocket(`ws://127.0.0.1:${port}${endpoint}`);await new Promise((r,j)=>{socket.once('open',r);socket.once('error',j)});socket.accept=()=>{};return {webSocket:socket};}};
 runtime=createBrowserLoginRuntime({storage,browser:binding,agentId:agent,authorize(context){assert.equal(context.sessionId,'synthetic-owner');},
  resolveVaultFields:input=>vault.materialize(input),
  savePrivateVault:savePrivateVault
 });
 managedRuntime=await createManagedBrowserRuntime({ctx:{storage},env:{MANAGED_BROWSER_PROVIDER:'cloudflare',BROWSER:binding,LOADER:{}},sessionId:agent,privateOnly:true,
  authorizeVaultAccess(context){assert.equal(context.sessionId,'synthetic-owner');},
  resolveVaultLogin:async input=>{const result=await vault.materialize({vault_id:input.vault_id,expected_origin:input.expected_origin,fields:['username','password']});return result.values;},
  resolveVaultFields:input=>vault.materialize(input),savePrivateVault,
  createRuntime:()=>({connector:{sessionInfo:async()=>({sessionId:'synthetic-local-browser'}),closeSession:async()=>{}},tools:{},runtime:{expirePaused:async()=>{}}})
 });
 async function savePrivateVault(entry){saveAttempts.push(entry.operation_id);if(failNextSave){failNextSave=false;throw Error('Synthetic broker availability failure');}const saved=await vault.save(entry);saveCalls.push({id:saved.id,kind:saved.kind,payload_keys:Object.keys(entry.payload).sort()});return saved;}

 api=http.createServer(async(req,res)=>{
  const path=new URL(req.url,'http://localhost').pathname;res.setHeader('content-type','application/json');
  const send=(status,body)=>{res.statusCode=status;res.end(JSON.stringify(body));};
  if(req.headers.authorization!==`Bearer ${key}`)return send(401,{error:'unauthorized'});
  if(path.startsWith('/v1/agents/')&&!path.startsWith(route+'/')&&path!==route)return send(404,{error:'not_found'});
  if(path==='/v1/credentials'&&req.method==='GET')return send(200,await vault.list());
  if(path===route)return send(200,metadata);
  if(path===route+'/events/history')return send(200,{data:[],has_more:false,latest_cursor:String(cursor)});
  let body;try{let raw='';for await(const c of req)raw+=c;body=raw?JSON.parse(raw):{};}catch{return send(400,{error:'invalid_json'});}
  trace.requests.push({method:req.method,path,action:body.action,keys:Object.keys(body),...(body.challenge_id?{challenge_id:body.challenge_id}:{}),...(body.save_to_vault===undefined?{}:{save_to_vault:body.save_to_vault})});
  if(path.startsWith('/v1/credentials/vault/')){
   try{const operationId=req.headers['x-nanocodex-operation-id'];assert.equal(typeof operationId,'string','Native Vault intake must send an operation ID');assert.match(operationId,/^[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i,'Native Vault intake must send its stable UUIDv5 operation ID');const saved=await vault.create(path.split('/').at(-1),body,operationId);intakeReceipts.push({id:saved.id,kind:saved.kind,operation_id:operationId});return send(200,saved);}catch(error){if(error.code==='ERR_ASSERTION')fixtureError=error;return send(400,{error:'invalid_vault_entry'});}
  }
  if(path===route+'/browser-vault/takeover'){
   try{
    const named=takeoverIds.has(body.challenge_id);
    const answer=await (named?managedRuntime.submitVaultTakeover(body,ctx.signal):runtime.submit(body,ctx.signal));
    if(answer.type==='browser_login_receipt')trace.receipts.push(answer);
    if(named&&['finish','cancel'].includes(body.action))trace.receipts.push({type:'browser_vault_takeover_receipt',challenge_id:body.challenge_id,...answer});
    if(body.action==='retry_vault_save')trace.save_retries.push({request_id:body.challenge_id,body:answer});
    return send(200,answer);
   }catch(error){return send(409,{error:'private_input_unavailable'});}
  }
  if(path===route+'/browser-vault/challenge'||path===route+'/secure-input'){
   try{
    const answer=await (path.endsWith('/challenge')?managedRuntime.submitVaultChallenge(body,ctx.signal):managedRuntime.submitSecureInput(body,ctx.signal));
    if(answer.type==='browser_vault_challenge_receipt'||answer.type==='secure_input_receipt')trace.receipts.push(answer);
    return send(200,answer);
   }catch(error){return send(409,{error:'private_input_unavailable'});}
  }
  if(path===route+'/turns'&&req.method==='POST'){incomingMessages.push(body);const id=body.id||crypto.randomUUID();resumeModel(body,id);return send(202,{id,state:'accepted'});}
  return send(404,{error:'not_found'});
 });
 api.on('upgrade',(req,socket,head)=>{if(req.headers.authorization!==`Bearer ${key}`||req.url.split('?')[0]!==route+'/ws')return socket.destroy();wss.handleUpgrade(req,socket,head,client=>{ws=client;ws.on('message',b=>{const message=JSON.parse(b);incomingMessages.push(message);if(message.type==='prompt')resumeModel(message,message.id);});ws.send(JSON.stringify({type:'ready',...metadata,restored:false}));});});
 await new Promise(r=>api.listen(0,'127.0.0.1',r));
 terminal=spawn('python3',[new URL('../../../bin/nanocodex/tests/share-pty-bridge.py',import.meta.url).pathname,binary,'attach',agent],{cwd:temp,env:{...process.env,HOME:temp,NC_API_KEY:'',CODEX_HOME:join(temp,'.codex'),NANOCODEX_RELOAD_DIR:join(temp,'.reload'),NANOCODEX_DISABLE_HAND:'1',NANOCODEX_COMPUTER:'off',NANOCODEX_MANAGED_URL:`http://127.0.0.1:${api.address().port}`,NANOCODEX_API_KEY:key,TERM:'xterm-256color',SSH_TTY:'/dev/synthetic-pty',TMUX:'',TMUX_PANE:''},stdio:['pipe','pipe','pipe']});
 terminal.stdout.on('data',b=>{screen+=b;rendered=rendered.then(()=>new Promise(done=>emulator.write(b,()=>{capture();done();})));writeFileSync(join(output,'terminal.ansi'),screen)});terminal.stderr.on('data',b=>{stderr+=b});
 await wait(()=>!!ws,'TUI WebSocket attached');await delay(300);
 const hint=await newLogin();requestPanel(hint);
 await wait(()=>trace.requests.some(r=>r.action==='describe'),'private panel describe');
 pass('live tool result opens private panel in shipped TUI');
 // Additional journey steps follow the shipped keyboard protocol.
 await runJourneys();
 trace.status='passed';
}catch(error){trace.status='failed';trace.failure=error.stack;throw error;}
finally{
 await rendered;capture();writeFileSync(join(output,'trace.json'),JSON.stringify(trace,null,2)+'\n');writeFileSync(join(output,'terminal.ansi'),screen);writeFileSync(join(output,'stderr.log'),stderr);
 if(terminal){terminal.stdin.end();await delay(200);if(terminal.exitCode===null)terminal.kill();}
 for(const client of wss.clients)client.terminate();wss.close();
 if(vault)await vault.close();
 if(runtime)await runtime.close().catch(()=>{});if(managedRuntime)await managedRuntime.close().catch(()=>{});if(browser)await browser.close().catch(()=>{});
 if(chrome){chrome.kill();await delay(150);if(chrome.exitCode===null)chrome.kill('SIGKILL');}
 if(api){api.closeAllConnections();api.close();}if(site){site.closeAllConnections();site.close();}
 // Retain synthetic local journals for leakage inspection; remove Chrome profile/certificate.
 rmSync(join(temp,'chrome'),{recursive:true,force:true});rmSync(join(temp,'key'),{force:true});rmSync(join(temp,'cert'),{force:true});
}
function parsedReceipts(messages=incomingMessages){
 const found=[];const visit=value=>{if(typeof value==='string'){try{visit(JSON.parse(value));}catch{}}else if(value&&typeof value==='object'){if(['browser_login_receipt','vault_intake_receipt','private_vault_save_receipt','browser_vault_takeover_receipt','browser_vault_challenge_receipt','secure_input_receipt'].includes(value.type))found.push(value);else for(const child of Object.values(value))visit(child);}};visit(messages);return found;
}
function resumeModel(message,id){
 const receipt=parsedReceipts(message)[0];
 assert.ok(receipt,'Fixture model continuation requires a structured private receipt');
 const delivered=receipt.type==='vault_intake_receipt'?intakeReceipts.find(r=>r.id===receipt.id&&r.kind===receipt.kind):receipt.type==='private_vault_save_receipt'?trace.save_retries.find(r=>r.request_id===receipt.request_id&&r.body.vault_save?.status===receipt.vault_save?.status):trace.receipts.find(r=>r.type===receipt.type&&(r.request_id||r.challenge_id)===(receipt.request_id||receipt.challenge_id)&&r.status===receipt.status);
 if(!delivered){fixtureError=new Error('Receipt must match a completed private HTTP request before resuming: '+JSON.stringify(receipt));return;}
 modelResumptions.push({turn_id:id,request_id:receipt.request_id,challenge_id:receipt.challenge_id,id:receipt.id,type:receipt.type,status:receipt.status});
 setTimeout(()=>{ws.send(JSON.stringify({type:'turn_accepted',id,turn_id:id,created_at:Date.now()/1000,cursor:String(++cursor),input:message.input,replayed:false}));ws.send(JSON.stringify({type:'turn_completed',id,turn_id:id,created_at:Date.now()/1000,cursor:String(++cursor),final_message:'Synthetic model resumed from safe receipt. '+(receipt.request_id||receipt.challenge_id||receipt.id),usage:null,citations:[],usage_error:null}));},100);
}
async function unlock(){
 const safetyToken=()=>visibleText().match(/Type safety token \(keys only\): ([a-f0-9]{32})/)?.[1];
 await wait(()=>{const token=safetyToken();return token&&token!==lastSafetyToken;},'fresh private safety token');
 // Esc first renders a local cancelled phase, then the HTTP receipt replaces
 // it with another guarded phase. Follow a replacement token without retrying
 // the private operation or assuming that an earlier token enabled this phase.
 for(let transitions=0;transitions<5;transitions++){
  const token=safetyToken();assert.ok(token,'private phase must show a safety token');
  terminal.stdin.write(token);
  await wait(()=>visibleText().includes('Safety token verified. Controls above are now enabled.')||(safetyToken()&&safetyToken()!==token),'private controls enabled or phase replaced');
  if(visibleText().includes('Safety token verified. Controls above are now enabled.')){lastSafetyToken=token;return;}
 }
 throw Error('Private safety phase did not settle');
}
async function openFields(){await unlock();await keypress(ctrlEnter);await unlock();}
async function dismiss(){await unlock();await keypress('\x1b');await delay(150);}
async function submit(dismissPanel=true){const before=trace.receipts.length;await keypress(ctrlEnter);await wait(()=>trace.receipts.length>before,'private finish receipt');if(dismissPanel)await dismiss();return trace.receipts.at(-1);}
async function fill(values){for(let i=0;i<values.length;i++){type(values[i]);await delay(90);if(i<values.length-1)await keypress('\t');}}
async function panel(path='/login'){const hint=await newLogin(path);requestPanel(hint);await openFields();return hint;}
async function runJourneys(){
 const queued='queued-private-DO-NOT-EXPOSE';privateValues.push(queued);type(queued);await delay(100);
 await openFields();
 assert.ok(visibleText().includes('[x] Save to Vault'),'save starts enabled');
 const username='private-synthetic-user@example.test',password='synthetic-private-Pass-DO-NOT-EXPOSE';privateValues.push(username,password);
 await fill([username,password]);const first=await submit();
 assert.equal(first.vault_save?.status,'saved');assert.equal(saveCalls.length,1);
 assert.equal(await activePage.locator('#username').inputValue(),username);assert.equal(await activePage.locator('#password').inputValue(),password);
 const restored=await vault.materialize({vault_id:saveCalls[0].id,expected_origin:siteOrigin,fields:['username','password']});
 assert.deepEqual(restored.values,{username,password});
 const encrypted=await vault.encryptedRows();assert.ok(encrypted.length===1&&encrypted.every(row=>row.encrypted&&row.keys.length===1&&row.keys[0]==='envelope'));
 await wait(()=>parsedReceipts().some(r=>r.request_id===first.request_id),'safe receipt resumes synthetic model');
 const resumed=parsedReceipts().find(r=>r.request_id===first.request_id);assert.equal(resumed.status,'finished');assert.equal(resumed.vault_save?.items?.[0]?.id,saveCalls[0].id);
 await wait(()=>visibleText().includes('Synthetic model resumed from safe receipt.'),'model continuation visible');assert.ok(modelResumptions.some(r=>r.request_id===first.request_id));
 pass('Real TUI sends matching safe receipt with saved item ID and model visibly resumes');
 pass('TUI default save writes and decrypts real encrypted Vault item',{receipt:first});
 const beforeReplay=trace.requests.length,receiptsBeforeReplay=parsedReceipts().length;requestPanel({type:'browser_login',status:'input_required',request_id:first.request_id,challenge_id:first.request_id,agent_id:agent,origin:siteOrigin,allowed_origins:[siteOrigin],expires_at:durable.get('browser-login:'+agent).expiresAt,approved:false});await delay(350);assert.equal(trace.requests.length,beforeReplay);assert.equal(parsedReceipts().length,receiptsBeforeReplay);pass('Repeated finished request event does not reopen or submit twice');
 await panel();await keypress('\x1bOQ');await fill(['private-optout-user','synthetic-optout-Pass']);privateValues.push('private-optout-user','synthetic-optout-Pass');
 await submit();assert.equal(saveCalls.length,1);pass('TUI F2 opt-out fills Chrome without saving');
 await panel('/otp');const otp='734921';privateValues.push(otp);await fill([otp]);await submit();assert.equal(await activePage.locator('#otp').inputValue(),otp);assert.equal(saveCalls.length,1);pass('OTP reaches Chrome and is never saved');
 await panel('/cvc');const cvc='683';privateValues.push(cvc);await fill([cvc]);await submit();assert.equal(await activePage.locator('#cvc').inputValue(),cvc);assert.equal(saveCalls.length,1);pass('Card security code reaches Chrome and never enters Vault');
 await panel();await fill(['cancelled-private-user','cancelled-private-password']);privateValues.push('cancelled-private-user','cancelled-private-password');
 const before=trace.receipts.length;await keypress('\x1b');await wait(()=>trace.receipts.length>before,'cancel receipt');await dismiss();assert.equal(trace.receipts.at(-1).status,'cancelled');assert.equal(saveCalls.length,1);pass('Cancel discards private fields without a Vault write');
 await panel();
 await fill(['stale-private-user','stale-private-password']);privateValues.push('stale-private-user','stale-private-password');
 await activePage.reload();const beforeStale=trace.receipts.length;await keypress(ctrlEnter);await wait(()=>visibleText().includes('Private operation could not be confirmed.'),'visible stale-document rejection');await dismiss();assert.equal(trace.receipts.length,beforeStale);assert.equal(saveCalls.length,1);assert.equal(await activePage.locator('#username').inputValue(),'');
 pass('Stale document rejects typed values visibly without a receipt or Vault write');
 await panel('/username');const stepUser='step-private-user@example.test',stepPass='step-private-password';privateValues.push(stepUser,stepPass);
 await fill([stepUser]);await submit();const beforePair=saveCalls.length;
 await tools('browser_login_action',{request_id:activeId,operation_id:crypto.randomUUID(),action:'navigate',url:siteOrigin+'/password'});
 await activePage.locator('#password').waitFor();
 const continued=await tools('request_browser_login_input',{request_id:activeId,operation_id:crypto.randomUUID()});activeId=continued.request_id;
 requestPanel(continued,'request_browser_login_input');await openFields();await fill([stepPass]);const paired=await submit();
 assert.equal(paired.vault_save?.status,'saved');assert.equal(saveCalls.length,beforePair+1);
 const pair=await vault.materialize({vault_id:saveCalls.at(-1).id,expected_origin:siteOrigin,fields:['username','password']});assert.deepEqual(pair.values,{username:stepUser,password:stepPass});
 pass('Two-step username/password navigation pairs one encrypted login within same session');
 await panel();const retryUser='save-retry-private-user',retryPass='save-retry-private-password';privateValues.push(retryUser,retryPass);await fill([retryUser,retryPass]);failNextSave=true;const savedBeforeFailure=saveCalls.length,failed=await submit(false);assert.equal(failed.vault_save?.status,'failed');assert.equal(failed.vault_save?.retryable,true);assert.equal(saveCalls.length,savedBeforeFailure);
 const eventsBeforeRetry=await activePage.evaluate(()=>({...counts})),attempt=saveAttempts.at(-1);
 await unlock();assert.ok(visibleText().includes('F5: retry Vault saving only'));const beforeRetry=trace.save_retries.length;await keypress('\x1b[15~');await wait(()=>trace.save_retries.length>beforeRetry,'TUI F5 save-only receipt');const retry={status:200,body:trace.save_retries.at(-1).body};assert.equal(retry.body.vault_save.status,'saved');await dismiss();await wait(()=>parsedReceipts().some(r=>r.type==='private_vault_save_receipt'&&r.request_id===activeId),'safe save-only retry continuation');assert.equal(saveAttempts.at(-1),attempt);assert.equal(saveCalls.length,savedBeforeFailure+1);assert.deepEqual(await activePage.evaluate(()=>({...counts})),eventsBeforeRetry);
 const replay=await httpControl({challenge_id:activeId,action:'retry_vault_save'});assert.deepEqual(replay,retry);assert.equal(saveCalls.length,savedBeforeFailure+1);assert.deepEqual(await activePage.evaluate(()=>({...counts})),eventsBeforeRetry);pass('Actual TUI F5 retries stable broker save, no repeated Chrome input or duplicate item');
 await panel();const expiring=durable.get('browser-login:'+agent);durable.set('browser-login:'+agent,{...expiring,expiresAt:Date.now()-1});
 await fill(['expired-private-user','expired-private-password']);privateValues.push('expired-private-user','expired-private-password');await keypress(ctrlEnter);await dismiss();assert.equal(await activePage.locator('#username').inputValue(),'');pass('Expired request fails closed without Chrome input');
 const bad=await httpControl({challenge_id:activeId,action:'describe'},'invalid');assert.equal(bad.status,401);
 const wrong=await httpControl({challenge_id:activeId,action:'describe'},key,'another-agent');assert.equal(wrong.status,404);
 pass('Fixture admission rejects bad credentials and cross-agent paths',{limitation:'This is fixture admission; production account ingress is covered separately.'});
 await managedFamilyJourneys();
 await intakeAndReuseJourneys();
 assertNoPrivateInput(screen,'terminal');assertNoPrivateInput(incomingMessages,'model transport');assertNoPrivateInput(trace,'evidence');assertNoPrivateInput([...durable],'managed storage');
 const inspectFiles=dir=>{for(const entry of readdirSync(dir,{withFileTypes:true})){const path=join(dir,entry.name);if(entry.isDirectory()){if(entry.name!=='chrome')inspectFiles(path);}else if(!['key','cert'].includes(entry.name)){const body=readFileSync(path,'utf8');assertNoPrivateInput(body,'local TUI history/journal');}}};inspectFiles(temp);
 trace.model_receipts=incomingMessages;trace.model_resumptions=modelResumptions;trace.intake_receipts=intakeReceipts;
 pass('Private values absent from terminal, model transport, evidence and managed storage');
}

async function intakeAndReuseJourneys(){
 const entries=[
  {kind:'login',name:'PTY login',values:['intake-user@example.test','intake-private-password'],path:'/password',selector:'#password',role:'password',secret:'intake-private-password'},
  {kind:'api_key',name:'PTY API',values:['intake-private-api-key'],path:'/reuse-api',selector:'#api_key',role:'api_key',secret:'intake-private-api-key'},
  {kind:'card',name:'PTY card',values:['4242424242424242','12','2035','90210'],path:'/reuse-card',selector:'#card_number',role:'card_number',secret:'4242424242424242'},
  {kind:'address',name:'PTY address',values:['41 Synthetic Orchard Lane','','Fixturetown','CA','90210','US'],path:'/reuse-address',selector:'#address_line_1',role:'address_line_1',secret:'41 Synthetic Orchard Lane'},
  {kind:'phone',name:'PTY phone',values:['+15555550187'],path:'/reuse-phone',selector:'#phone_number',role:'phone_number',secret:'+15555550187'},
 ];
 for(const [index,entry] of entries.entries()){
  privateValues.push(...entry.values.filter(v=>v.length>=8));
  const hint={type:'vault_intake',status:'input_required',operation:'create',kind:entry.kind,name:entry.name};
  const turn='intake-'+entry.kind;
  
  // Exercise decorated Code Mode content and direct results without an agent_id.
  const wrapped=index%2?{content:[{type:'text',text:'Script completed\nWall time 0.1 seconds\nOutput:\n'+JSON.stringify(hint)}]}:hint;
  requestPanel(wrapped,index%2?'exec':'request_vault_intake',turn);
  await unlock();await keypress('\t');await fill(entry.values);
  const before=intakeReceipts.length;await keypress(ctrlEnter);await wait(()=>intakeReceipts.length>before,'native '+entry.kind+' intake');await dismiss();
  const created=intakeReceipts.at(-1);assert.equal(created.kind,entry.kind);
  await wait(()=>parsedReceipts().some(r=>r.id===created.id),'safe native intake receipt');
  const beforeEcho=trace.requests.length;requestPanel(hint,'request_vault_intake',turn);await delay(350);assert.equal(trace.requests.length,beforeEcho);
  pass('Actual TUI native '+entry.kind+' intake saves encrypted item and duplicate echo stays closed');
  await panel(entry.path);
  await keypress('\x1bOR');await unlock();await delay(100);
  // Safe item names and semantic roles are sufficient to choose; saved values stay broker-side.
  for(let n=0;n<40;n++){
   const rendered=visibleText();
   if(rendered.includes('"'+entry.name+'" · '+entry.kind+' · '+entry.role))break;
   await keypress('\x1b[B');
   if(n===39)throw Error('Vault picker did not offer '+entry.kind);
  }
  await keypress('\r');await unlock();const beforeSave=saveCalls.length;await submit();
  assert.equal(await activePage.locator(entry.selector).inputValue(),entry.secret);
  assert.equal(saveCalls.length,beforeSave);
  pass('Actual TUI F3 reuses '+entry.kind+' through encrypted broker into Chrome');
  if(entry.kind==='phone'){
   requestPanel(hint,'request_vault_intake','later-phone-turn');await unlock();await keypress('\t');await fill(entry.values);
   const beforeLater=intakeReceipts.length;await keypress(ctrlEnter);await wait(()=>intakeReceipts.length>beforeLater,'later identical intake');await dismiss();assert.notEqual(intakeReceipts.at(-1).id,created.id);assert.notEqual(intakeReceipts.at(-1).operation_id,created.operation_id,'Later turn must use a distinct intake operation ID');
   pass('Later turn can open identical native intake metadata again');
  }
 }
 const localPhone='+15555550189';privateValues.push(localPhone);
 type('/vault add phone');await keypress('\r');await unlock();await fill(['Local TUI phone',localPhone]);
 const beforeLocal=intakeReceipts.length;await keypress(ctrlEnter);await wait(()=>intakeReceipts.length>beforeLocal,'caller-local Vault creation');await dismiss();
 assert.equal(intakeReceipts.at(-1).kind,'phone');
 await wait(()=>parsedReceipts().some(r=>r.id===intakeReceipts.at(-1).id),'safe local Vault receipt');
 pass('Caller-local /vault add opens private TUI form and saves through account API without a model request');
 const rows=await vault.encryptedRows();assert.ok(rows.length>=entries.length&&rows.every(r=>r.encrypted&&r.keys.join(',')==='envelope'));
 const brokerLog=vault.logs.join('\n');
 for(const [index,secret] of privateValues.entries()){
  if(containsPrivateValue(brokerLog,secret)){
   let redacted=brokerLog;for(const value of privateValues)redacted=redacted.replaceAll(value,'[private input]');
   writeFileSync(join(output,'broker-redacted.log'),redacted);
   trace.leak_diagnostic={boundary:'broker log',fixture_value_index:index,value_length:secret.length};
  }
  assert.ok(!containsPrivateValue(brokerLog,secret),'encrypted broker logs omit private input');
 }
}

async function managedFamilyJourneys(){
 // A preallocated local Chrome target replaces only Cloudflare allocation.
 // Every named/browser-form operation below uses the production runtime and
 // the same encrypted broker item created by the first actual TUI journey.
 await tools('browser_login_close',{});activeId='';
 if(activePage&&!activePage.isClosed())await activePage.close();
 activePage=await browser.contexts()[0].newPage();
 await activePage.goto(siteOrigin+'/profile');
 const session=await activePage.context().newCDPSession(activePage);
 const {targetInfo}=await session.send('Target.getTargetInfo');await session.detach();
 const identity={vault_id:saveCalls[0].id,target_id:targetInfo.targetId,expected_origin:siteOrigin};
 const named=(name,args={})=>managedTool(name,{...identity,...args});
 const assertResumed=async(receipt)=>{
  const id=receipt.request_id||receipt.challenge_id;
  await wait(()=>modelResumptions.some(r=>r.type===receipt.type&&(r.request_id||r.challenge_id)===id&&r.status===receipt.status),'matching '+receipt.type+' model continuation');
  await wait(()=>visibleText().includes('Synthetic model resumed from safe receipt. '+id),'visible '+receipt.type+' continuation');
  const safe=parsedReceipts().find(r=>r.type===receipt.type&&(r.request_id||r.challenge_id)===id);
  assert.equal(safe.status,receipt.status);
  assertNoPrivateInput(safe,'family receipt');
 };
 const assertRedacted=async(snapshot,values)=>{
  const result=JSON.stringify(snapshot);
  for(const value of values)assert.ok(!containsPrivateValue(result,value),'private snapshot must redact entered values');
 };
 const savedBefore=saveCalls.length,postsBefore=browserPosts.length;
 const snapshot=await named('browser_vault_snapshot');
 const fields=snapshot.elements.filter(e=>e.native_input).map(e=>({ref:e.ref}));
 assert.equal(fields.length,4,'profile exposes text, select, checkbox and multiline fields');
 const takeover=await named('browser_vault_request_takeover',{operation_id:crypto.randomUUID(),snapshot_id:snapshot.snapshot_id,fields});
 assert.equal(takeover.type,'browser_vault_takeover');takeoverIds.add(takeover.challenge_id);
 requestPanel(takeover,'browser_vault_request_takeover');await openFields();
 const contact='named-private-contact@example.test',notes='named-private-first-line\nnamed-private-second-line';privateValues.push(contact,notes,...notes.split('\n'));
 type(contact);await keypress('\t');await keypress('\x1b[B');await keypress('\t');await keypress(' ');await keypress('\t');type(notes);await delay(100);
 const takeoverReceipt=await submit();assert.equal(takeoverReceipt.status,'finished');
 assert.equal(await activePage.locator('#email').inputValue(),contact);
 assert.equal(await activePage.locator('#country').inputValue(),'ca');assert.equal(await activePage.locator('#updates').isChecked(),true);
 assert.equal(await activePage.locator('#notes').inputValue(),notes);
 assert.equal(browserPosts.length,postsBefore,'named native input fills without submitting');
 assert.equal(saveCalls.length,savedBefore,'profile controls do not create a login');
 await activePage.evaluate(values=>{const echo=document.createElement('p');echo.textContent=values.join(' ');document.body.append(echo);},[contact,notes]);
 await assertRedacted(await named('browser_vault_snapshot'),[contact,...notes.split('\n')]);
 await assertResumed(takeoverReceipt);
 pass('Actual TUI named Vault takeover fills text/select/checkbox/multiline and resumes from redacted receipt',{receipt:takeoverReceipt,dom:{country:'ca',updates:true,multiline:true},form_submissions:0});

 await named('browser_vault_action',{operation_id:crypto.randomUUID(),action:'navigate',url:siteOrigin+'/otp'});
 await activePage.locator('#otp').waitFor();
 const otpHint=await named('browser_vault_request_challenge');
 assert.equal(otpHint.type,'browser_vault_challenge');requestPanel(otpHint,'browser_vault_request_challenge');await unlock();
 const otp='862507';privateValues.push(otp);await fill([otp]);
 const otpReceipt=await submit();assert.equal(otpReceipt.status,'submitted');
 await activePage.waitForURL(siteOrigin+'/submitted');
 assert.equal(browserPosts.length,postsBefore+1,'named OTP submits exactly once');
 assert.equal(new URLSearchParams(browserPosts.at(-1)).get('otp'),otp,'merchant receives terminal verification code');
 assert.equal(saveCalls.length,savedBefore,'named OTP is never saved');
 await assertRedacted(await named('browser_vault_snapshot'),[otp]);await assertResumed(otpReceipt);
 pass('Actual TUI named OTP challenge submits once to Chrome merchant and safely resumes without Vault save',{receipt:otpReceipt,form_submissions:1});

 // Bind the legacy password request to the retained named Vault identity.
 await named('browser_vault_action',{operation_id:crypto.randomUUID(),action:'navigate',url:siteOrigin+'/password'});
 await activePage.locator('#password').waitFor();
 const legacy=await managedTool('request_secure_input',{target_id:identity.target_id,expected_origin:siteOrigin,password_selector:'#password',submit:false});
 assert.equal(legacy.kind,'browser_password');requestPanel(legacy,'request_secure_input');await unlock();
 const legacyPassword='legacy-bound-private-password',legacyUsername='legacy-bound-user@example.test';privateValues.push(legacyPassword,legacyUsername);
 await fill([legacyPassword,legacyUsername]);const legacyReceipt=await submit();
 assert.equal(legacyReceipt.status,'filled');assert.equal(legacyReceipt.vault_save?.status,'saved');
 assert.equal(await activePage.locator('#password').inputValue(),legacyPassword);assert.equal(browserPosts.length,postsBefore+1);
 assert.equal(saveCalls.length,savedBefore+1);
 const savedLogin=await vault.materialize({vault_id:legacyReceipt.vault_save.items[0].id,expected_origin:siteOrigin,fields:['username','password']});
 assert.deepEqual(savedLogin.values,{username:legacyUsername,password:legacyPassword});
 await activePage.evaluate(value=>{const echo=document.createElement('p');echo.textContent=value;document.body.append(echo);},legacyPassword);
 await assertRedacted(await managedTool('secure_input_snapshot',{request_id:legacy.request_id}),[legacyPassword]);await assertResumed(legacyReceipt);
 pass('Actual TUI legacy bound password fills Chrome, saves encrypted login and safely resumes',{receipt:legacyReceipt,form_submissions:0});

 await managedTool('browser_vault_close',{});
 await activePage.goto(siteOrigin+'/secure-card');
 const typed=await managedTool('request_secure_input',{target_id:identity.target_id,expected_origin:siteOrigin,submit:false,fields:[
  {id:'pan',kind:'card_number',selector:'#pan'},
  {id:'expiry',kind:'card_expiry',selector:'#expiry'},
  {id:'zip',kind:'sensitive_text',selector:'#zip'},
  {id:'cvc',kind:'card_cvc',selector:'#cvc'},
 ]});
 assert.equal(typed.kind,'browser_form');requestPanel(typed,'request_secure_input');await unlock();
 const card=['5555 5555 5555 4444','11/2036','94107','947'];privateValues.push(...card,card[0].replaceAll(' ',''));
 await fill(card);const cardReceipt=await submit();assert.equal(cardReceipt.status,'filled');assert.equal(cardReceipt.vault_save?.status,'saved');
 for(const [index,id] of ['pan','expiry','zip','cvc'].entries())assert.equal(await activePage.locator('#'+id).inputValue(),card[index]);
 assert.equal(browserPosts.length,postsBefore+1,'typed card input never submits');assert.equal(saveCalls.length,savedBefore+2);
 const savedCard=await vault.materialize({vault_id:cardReceipt.vault_save.items[0].id,expected_origin:siteOrigin,fields:['card_number']});
 assert.ok(savedCard.values.card_number===card[0],'encrypted card round-trips the submitted PAN formatting');
 assert.deepEqual(saveCalls.at(-1).payload_keys,['billing_zip','card_number','expiry_month','expiry_year','name'],'card save excludes CVC');
 await activePage.evaluate(values=>{const echo=document.createElement('p');echo.textContent=values.join(' ');document.body.append(echo);},card);
 await assertRedacted(await managedTool('secure_input_snapshot',{request_id:typed.request_id}),card);await assertResumed(cardReceipt);
 pass('Actual TUI typed secure card form fills without submitting, saves encrypted card and safely resumes',{receipt:cardReceipt,form_submissions:0});
 await managedTool('browser_vault_close',{});await activePage.close();
}
