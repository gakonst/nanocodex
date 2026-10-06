// Human-only takeover protocol in real Chrome, using synthetic content and fake input.
import assert from 'node:assert/strict';
import {createNamecheapFixture,namecheapSynthetic} from './fixtures/namecheap-login.mjs';
import https from 'node:https';
import { readFileSync, mkdtempSync, rmSync, readdirSync, mkdirSync, writeFileSync } from 'node:fs';
import { execFileSync, spawn } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { registerHooks, createRequire } from 'node:module';
// Cloudflare allocation/storage and the unused public browser factory are local
// adapters; the private runtime, HTTPS route, CDP and DOM all execute unchanged.
const browserAdapter = `export const createBrowserSession=(b,o)=>b.create(o);
export const deleteBrowserSession=(b,id)=>b.delete?.(id);
export const connectBrowser=()=>{throw Error('Unused public browser adapter')};
export class DurableBrowserSessionStore {
  constructor(storage){this.storage=storage}
  get(k){return this.storage.get(k)} set(k,v){return this.storage.put(k,v)} delete(k){return this.storage.delete(k)}
  async acquireLock(){return {release:async()=>{}}}
}`;
registerHooks({resolve(specifier, context, nextResolve) {
  if (specifier === 'agents/browser') return {url:'data:text/javascript,'+encodeURIComponent(browserAdapter),shortCircuit:true};
  if (specifier === 'agents/browser/ai') return {url:'data:text/javascript,export const createBrowserRuntime=()=>{throw Error("Provide local browser allocation")};',shortCircuit:true};
  return nextResolve(specifier.startsWith('./browser-') && !specifier.endsWith('.ts') ? specifier + '.ts' : specifier, context);
}});
const { createBrowserLoginRuntime } = await import('../src/browser-login-runtime.ts');
const { createManagedBrowserRuntime } = await import('../src/browser-runtime.ts');
const { browserTakeover, decodeVaultIntake } = await import('../../account/src/vaultIntake.ts');
const { PrivateBrowserCdp, fillBrowserVault } = await import('../src/browser-vault.ts');
const { default: WebSocket } = await import('ws');
const { privateVaultTakeover, releasePrivateVaultTakeover } = await import('../src/browser-vault-takeover.ts');
const packages = new URL('../../../node_modules/.pnpm/', import.meta.url);
const entry = readdirSync(packages).find(name => /^playwright-core@/.test(name));
const { chromium } = await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`, packages));
const temp = mkdtempSync(join(tmpdir(), 'private-touch-'));
// Synthetic localhost fixture only. Ubuntu CI restricts unprivileged user
// namespaces; this does not configure production browser sessions.
const testChromeFlags = process.platform === 'linux' ? ['--no-sandbox'] : [];
async function debuggingEndpoint(child, profile) {
  for (let i = 0; i < 100; i++) {
    if (child.exitCode !== null || child.signalCode !== null) {
      throw new Error(`Synthetic Chromium exited during startup (${child.signalCode ?? child.exitCode})`);
    }
    try { return readFileSync(join(profile, 'DevToolsActivePort'), 'utf8').trim().split('\n'); }
    catch { await new Promise(resolve => setTimeout(resolve, 100)); }
  }
  throw new Error('Synthetic Chromium did not start within 10 seconds');
}
async function stopChrome(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  let timer;
  const ended = new Promise(resolve => child.once('exit', resolve));
  child.kill();
  await Promise.race([ended, new Promise(resolve => { timer = setTimeout(resolve, 3000); })]);
  clearTimeout(timer);
  if (child.exitCode === null && child.signalCode === null) {
    child.kill('SIGKILL');
    await Promise.race([ended, new Promise(resolve => { timer = setTimeout(resolve, 3000); })]);
    clearTimeout(timer);
  }
}
const namecheapFixture=createNamecheapFixture();
const requireAccount=createRequire(new URL('../../account/package.json',import.meta.url));
const requireManaged=createRequire(new URL('../package.json',import.meta.url));
const reactBundle=requireManaged('esbuild').buildSync({stdin:{resolveDir:new URL('../../../',import.meta.url).pathname,contents:`
  import React from ${JSON.stringify(requireAccount.resolve('react'))};
  import {createRoot} from ${JSON.stringify(requireAccount.resolve('react-dom/client'))};
  function App(){const [checked,setChecked]=React.useState(false);
    return React.createElement('form',{onSubmit:e=>{e.preventDefault();fetch('/react-submit',{method:'POST',body:JSON.stringify({checked})});}},
      React.createElement('label',{},'Agreement',React.createElement('input',{id:'agreement',type:'checkbox',checked,onChange:e=>{window.reactChanges=(window.reactChanges||0)+1;setChecked(e.target.checked);}})),
      React.createElement('output',{id:'react-state'},String(checked)),
      React.createElement('button',{type:'submit'},'Save checkbox choice'));
  }
  createRoot(document.getElementById('root')).render(React.createElement(App));
`},bundle:true,write:false,format:'iife'}).outputFiles[0].text;
const reactPosts=[];

let browser, server, chrome, privateCdp, loginRuntime, vaultRuntime, handleControl, loginBrowser, runtimeChrome;
try {
  execFileSync('openssl', ['req','-x509','-newkey','rsa:2048','-nodes','-keyout',join(temp,'key'),'-out',join(temp,'cert'),'-days','1','-subj','/CN=localhost'],{stdio:'ignore'});
  server = https.createServer({key:readFileSync(join(temp,'key')),cert:readFileSync(join(temp,'cert'))}, (req,res) => {
    if (namecheapFixture.handler(req,res)) return;
    if (req.url === '/react-checkbox.js') {res.setHeader('content-type','application/javascript');res.end(reactBundle);return;}
    if (req.url === '/react-checkbox') {res.setHeader('content-type','text/html');res.end('<meta name="viewport" content="width=device-width,initial-scale=1"><div id="root"></div><script src="/react-checkbox.js"></script>');return;}
    if (req.url === '/react-submit') {let body='';req.on('data',c=>body+=c);req.on('end',()=>{reactPosts.push(JSON.parse(body));res.end('Saved');});return;}

    if (req.url.startsWith('/v1/agents/')) { handleControl(req,res); return; }
    res.setHeader('Content-Type','text/html');
    if (req.url === '/profile-start') {
      res.end('<button id="open-profile" onclick="location.href=\'/profile\'">Open delivery preferences</button>');
      return;
    }
    if (req.url === '/profile') {
      res.end('<meta name="viewport" content="width=device-width,initial-scale=1"><style>body{font:16px sans-serif}label,input,select,textarea{display:block;margin:8px;min-height:24px}</style><h1>Delivery preferences</h1><label>Contact email<input id="contact" type="email" autocomplete="email"></label><label>Delivery country<select id="country"><option value="">Choose country</option><option value="ca">Canada</option><option value="uk" label="United&#10;Kingdom">United Kingdom</option><option disabled>Unavailable</option></select></label><label>Delivery notes<textarea id="notes"></textarea></label><label>Send status updates<input id="updates" type="checkbox"></label><label>Unrelated field<input id="unrelated"></label><label>Unavailable choices<select id="unavailable"><option disabled>Unavailable</option><option hidden>Hidden</option><optgroup disabled><option>Disabled group</option></optgroup></select></label><button id="save">Save preferences</button><script>window.counts={input:0,change:0,click:0,remoteClick:0};document.addEventListener("input",()=>counts.input++);document.addEventListener("change",()=>counts.change++);document.addEventListener("click",e=>{counts.click++;if(e.isTrusted)counts.remoteClick++});</script>');
      return;
    }
    if (req.url === '/otp-countdown') {
      res.end('<meta name="viewport" content="width=device-width,initial-scale=1"><form method="post" action="/verify"><fieldset><label>Verification code<input id="countdown-otp" name="code" autocomplete="one-time-code" inputmode="numeric"></label><input id="nonce" type="hidden" name="nonce" value="initial"><select name="delivery"><option>Phone</option><option>Email</option></select><p id="countdown">Resend in 30 seconds</p><button type="submit">Verify</button></fieldset></form><script>window.ticks=0;window.startCountdown=()=>window.timer=setInterval(()=>{document.getElementById("countdown").textContent="Resend in "+(30-++window.ticks)+" seconds"},40);document.querySelector("form").addEventListener("submit",e=>e.preventDefault())</script>');
      return;
    }
    if (req.url === '/otp') {
      res.end('<meta name="viewport" content="width=device-width,initial-scale=1"><style>input{display:block;height:40px;margin:12px}</style><span id="code-label">Verification code</span> <span id="delivery-label">from your device</span><input id="otp" aria-labelledby="code-label delivery-label" aria-label="Fallback label" autocomplete="section-login one-time-code" inputmode="numeric"><input id="account" aria-label="Account" autocomplete="username webauthn"><input id="unsupported" aria-label="Other" autocomplete="arbitrary-private-marker" inputmode="none"><iframe title="Embedded unsupported input" srcdoc="<input autocomplete=one-time-code>"></iframe>');
      return;
    }
    res.end('<meta name="viewport" content="width=device-width,initial-scale=1"><style>body{margin:20px;font:16px sans-serif;min-height:2800px}input{display:block;height:48px;width:90%;margin:16px 0;font:inherit}</style><h1>Private browser fixture</h1><input type="email" placeholder="Email"><input type="password" placeholder="Password"><textarea aria-label="Notes"></textarea><input type="hidden" value="never exposed"><input disabled placeholder="Disabled"><input readonly placeholder="Read only"><div contenteditable>Custom fallback</div><p>Swipe this page</p><script>window.counts={input:0,change:0};document.addEventListener("input",()=>counts.input++);document.addEventListener("change",()=>counts.change++);Object.defineProperty(document.querySelector("input[type=email]"),"value",{get(){return Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,"value").get.call(this)},set(){throw Error("framework setter must be bypassed")}})</script>');
  });
  await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
  const origin = `https://127.0.0.1:${server.address().port}`;
  chrome = spawn(process.env.CHROME_PATH || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    [...testChromeFlags,'--headless','--ignore-certificate-errors','--no-first-run','--no-default-browser-check','--remote-debugging-port=0',`--user-data-dir=${join(temp,'profile')}`,'about:blank'],{stdio:'ignore'});
  const [port, endpoint] = await debuggingEndpoint(chrome, join(temp, 'profile'));
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  const context = await browser.newContext({ignoreHTTPSErrors:true});
  const page = await context.newPage();
  await page.goto(origin);
  const session = await context.newCDPSession(page);
  const socket = new WebSocket(`ws://127.0.0.1:${port}${endpoint}`);
  await new Promise((resolve,reject) => { socket.once('open',resolve); socket.once('error',reject); });
  socket.accept = () => {};
  const cdp = privateCdp = new PrivateBrowserCdp(socket);
  const { targetInfo } = await session.send('Target.getTargetInfo');
  const identity={vault_id:'a'.repeat(22),expected_origin:origin,target_id:targetInfo.targetId}, touch={};
  const act = action => privateVaultTakeover(cdp,identity,action,touch);
  let frame=await act({action:'observe',viewport:{width:390,height:740,mobile:true}});
  assert.equal(frame.width,390); assert.equal(frame.height,740);
  assert.equal(frame.native_form,undefined,'legacy clients receive no new frame keys');
  frame=await act({action:'observe',native_fields:true});
  assert.deepEqual(frame.native_form.fields.map(f=>f.label),['Email','Password','Notes']);
  const batch = (view, values) => ({action:'fill_fields',document_id:view.native_form.document_id,fields:view.native_form.fields.map((f,i)=>({ref:f.ref,value:values[i]}))});
  const values=['synthetic@example.test','synthetic-password-78235','Unicode 🙂 notes'];
  const firstBatch=batch(frame,values);
  const started=performance.now(); frame=await act(firstBatch); const batchMs=Math.round(performance.now()-started);
  assert.deepEqual(await page.locator('input:not([type=hidden]):not([disabled]):not([readonly]),textarea').evaluateAll(es=>es.map(e=>e.value)),values);
  assert.deepEqual(await page.evaluate(()=>counts),{input:3,change:3});
  assert.ok(!JSON.stringify(frame.native_form).includes(values[1]));
  await assert.rejects(act(firstBatch),'replay must fail after rotation');
  frame=await act({action:'observe',native_fields:true});
  const removedBatch=batch(frame,['must-not-fill','must-not-fill','must-not-fill']);
  await page.locator('input[type=password]').evaluate(e=>e.replaceWith(e.cloneNode()));
  await assert.rejects(act(removedBatch));
  assert.equal(await page.locator('input[type=email]').inputValue(),values[0],'all refs validated before first mutation');
  frame=await act({action:'observe',native_fields:true});
  const coveredBatch=batch(frame,['must-not-fill','must-not-fill','must-not-fill']);
  await page.locator('input[type=password]').evaluate(e=>{const r=e.getBoundingClientRect(),overlay=document.createElement('div');overlay.id='fixture-overlay';Object.assign(overlay.style,{position:'fixed',left:r.left+'px',top:r.top+'px',width:r.width+'px',height:r.height+'px',zIndex:9999});document.body.append(overlay);});
  await assert.rejects(act(coveredBatch));
  assert.equal(await page.locator('input[type=email]').inputValue(),values[0],'occluded field rejects batch before mutation');
  await page.locator('#fixture-overlay').evaluate(e=>e.remove());
  frame=await act({action:'observe',native_fields:true});
  const staleBatch=batch(frame,['must-not-fill','must-not-fill','must-not-fill']);
  await page.reload(); await assert.rejects(act(staleBatch));
  assert.equal(await page.locator('input[type=email]').inputValue(),'');
  frame=await act({action:'observe',native_fields:true});
  const forged=batch(frame,values); forged.fields[0].ref=crypto.randomUUID();
  await assert.rejects(act(forged)); assert.equal(await page.locator('input[type=password]').inputValue(),'');
  frame=await act({action:'observe',native_fields:true});
  const duplicate=batch(frame,values); duplicate.fields[1].ref=duplicate.fields[0].ref;
  await assert.rejects(act(duplicate));
  // If CDP loses the response after applying values, the batch is consumed and
  // only an explicit observation recovers. Never replay possibly completed input.
  frame=await act({action:'observe',native_fields:true});
  const lostBatch=batch(frame,values);
  const lostResponse={attachTarget: target=>cdp.attachTarget(target),send:async(method,params,sid)=>{
    const result=await cdp.send(method,params,sid);
    if(method==='Runtime.callFunctionOn' && params.arguments?.[0]?.value===lostBatch.document_id) throw Error('Synthetic lost batch response');
    return result;
  }};
  await assert.rejects(privateVaultTakeover(lostResponse,identity,lostBatch,touch));
  assert.deepEqual(await page.locator('input:not([type=hidden]):not([disabled]):not([readonly]),textarea').evaluateAll(es=>es.map(e=>e.value)),values);
  await assert.rejects(act(lostBatch));
  frame=await act({action:'observe',native_fields:true});
  await act(batch(frame,['','','']));
  frame=await act({action:'observe',native_fields:false});
  assert.equal(frame.native_form,undefined,'client may explicitly return to the legacy viewport');
  const email=frame.inputs.find(input=>input.type==='email');
  assert.ok(email); assert.equal(frame.keyboard,undefined);
  const x=email.x+email.width/2,y=email.y+email.height/2;
  await act({action:'touch',phase:'start',x,y});
  frame=await act({action:'touch',phase:'end'});
  assert.deepEqual(frame.keyboard,{type:'email',multiline:false});
  await act({action:'edit',delete_backward:0,text:'fake🙂'});
  await act({action:'edit',delete_backward:1,text:'!'});
  assert.equal(await page.locator('input[type=email]').inputValue(),'fake!');
  await act({action:'key',key:'Tab'});
  frame=await act({action:'observe',native_fields:true});
  assert.deepEqual(frame.keyboard,{type:'password',multiline:false});
  await act({action:'touch',phase:'start',x:0.85,y:0.8});
  await act({action:'touch',phase:'move',x:0.85,y:0.55});
  await act({action:'touch',phase:'move',x:0.85,y:0.3});
  await act({action:'touch',phase:'end'});
  assert.ok(await page.evaluate(()=>scrollY)>100,'touch swipes must scroll the actual page');
  await act({action:'touch',phase:'start',x:0.5,y:0.5});
  await act({action:'observe',native_fields:true}); // Explicit recovery cancels the finger, never repeats input.
  assert.equal(touch.active,false);
  await assert.rejects(act({action:'touch',phase:'move',x:0.5,y:0.4}));
  // Simulate a failed edit response: no finger is active, but runtime marks
  // the lease uncertain and requires an explicit refresh before further input.
  const failedEditCdp = {attachTarget: target => cdp.attachTarget(target), send: async (method, params, sid) => {
    if (method === 'Input.insertText') throw new Error('Synthetic disconnected edit response');
    return cdp.send(method, params, sid);
  }};
  await assert.rejects(privateVaultTakeover(failedEditCdp,identity,{action:'edit',delete_backward:0,text:'synthetic'},touch));
  touch.uncertain = true;
  await act({action:'observe',native_fields:true});
  assert.equal(touch.uncertain,false);
  await releasePrivateVaultTakeover(cdp,identity.target_id);
  assert.notEqual((await session.send('Page.getLayoutMetrics')).cssLayoutViewport.clientWidth,390);
  // Public private-login runtime and shipped account decoder over HTTPS.
  // Only allocation/storage are local adapters; CDP, DOM and transport are real.
  // A synthetic JS login with no form must return action_required, not an
  // ambiguous failure. Methodless JS forms retain their submit handler.
  for(const mode of ['formless','methodless']) {
    await page.setContent((mode==='methodless'?'<form>':'')+'<input id="user" type="email"><input id="pass" type="password"><button type="button" id="login">Sign in</button>'+(mode==='methodless'?'</form>':'')+'<script>window.signedIn=false;document.querySelector("button").onclick=()=>window.signedIn=true;</script>');
    const fill=await fillBrowserVault({cdp,sessionId:'synthetic-vault',request:{...identity,username_selector:'#user',password_selector:'#pass',submit:true},resolve:async()=>({username:'fake@example.test',password:'synthetic-password'}),quarantine:async()=>{}});
    assert.deepEqual(fill,{status:'filled',submission:'action_required'});
    assert.equal(await page.evaluate(()=>signedIn),false);
    assert.equal(await page.locator('#pass').inputValue(),'synthetic-password');
  }
  const durable=new Map();
  const storage={get:async k=>structuredClone(durable.get(k)),put:async(k,v)=>durable.set(k,structuredClone(v)),delete:async k=>durable.delete(k),transaction:async f=>f(storage)};
  // Runtime owns a separate browser, as in production. Connecting Playwright
  // before Target.createTarget races its debugger-paused auto-attachment with
  // the runtime's Page.navigate. Attach the observer after creation/navigation.
  runtimeChrome=spawn(process.env.CHROME_PATH || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    [...testChromeFlags,'--headless','--ignore-certificate-errors','--no-first-run','--no-default-browser-check','--remote-debugging-port=0',`--user-data-dir=${join(temp,'runtime-profile')}`,'about:blank'],{stdio:'ignore'});
  const [runtimePort,runtimeEndpoint] = await debuggingEndpoint(runtimeChrome, join(temp, 'runtime-profile'));
  let allocations=0;
  const binding={create:async()=>{allocations++;return {sessionId:'native-fields-fixture'};},delete:async()=>{},fetch:async()=>{
    const socket=new WebSocket(`ws://127.0.0.1:${runtimePort}${runtimeEndpoint}`);
    await new Promise((resolve,reject)=>{socket.once('open',resolve);socket.once('error',reject);});socket.accept=()=>{};return {webSocket:socket};
  }};
  loginRuntime=createBrowserLoginRuntime({storage,browser:binding,agentId:'fixture-agent',authorize:ctx=>{if(ctx.sessionId!=='owner')throw Error('forbidden');}});
  const ctx={sessionId:'owner',callId:'fixture',signal:new AbortController().signal};
  const tool=(name,args)=>loginRuntime.tools.find(t=>t.name===name).handler(args,ctx);
  const operation=crypto.randomUUID();
  const login=await tool('request_browser_login',{operation_id:operation,url:origin,allowed_origins:[origin]});
  let activeId=operation;
  const human=action=>loginRuntime.submit({challenge_id:activeId,...action},ctx.signal);
  await assert.rejects(human({action:'observe',native_fields:true}));
  await human({action:'approve'});
  loginBrowser=await chromium.connectOverCDP(`http://127.0.0.1:${runtimePort}`);
  let loginPage;
  for(let i=0;i<100;i++){loginPage=loginBrowser.contexts().flatMap(c=>c.pages()).find(p=>p.url().startsWith(origin));if(loginPage)break;await new Promise(resolve=>setTimeout(resolve,50));}
  assert.ok(loginPage,'private login target navigated: '+JSON.stringify(loginBrowser.contexts().map(c=>c.pages().map(p=>p.url()))));
  await loginPage.locator('input[type=email]').waitFor();
  let intake={operation:'browser_login',kind:'login',agent_id:'fixture-agent',challenge_id:operation,request_id:operation,allowed_origins:[origin]};
  handleControl=(req,res)=>{let body='';req.on('data',c=>body+=c);req.on('end',async()=>{
    res.setHeader('content-type','application/json');res.setHeader('cache-control','no-store');
    try{res.end(JSON.stringify(await loginRuntime.submit(JSON.parse(body),ctx.signal)));}catch{res.statusCode=409;res.end('{}');}
  });};
  const requestPrivate=(url,init)=>new Promise((resolve,reject)=>{
    const req=https.request(new URL(url,origin),{method:init.method,headers:init.headers,rejectUnauthorized:false},res=>{
      let body='';res.on('data',c=>body+=c);res.on('end',()=>resolve(new Response(body,{status:res.statusCode,headers:res.headers})));
    });req.on('error',reject);req.end(init.body);
  });
  const webFrame=await browserTakeover(intake,{action:'observe'},requestPrivate);
  assert.equal(webFrame.status,'active');assert.equal(webFrame.native_form,undefined);
  const nativeFrame=await human({action:'observe',native_fields:true});
  assert.equal(nativeFrame.native_form.fields.length,3);
  // A legacy web observer explicitly switches the lease back to the old response schema.
  const legacyAgain=await human({action:'observe'}); assert.equal(legacyAgain.native_form,undefined);
  const decoded=await browserTakeover(intake,{action:'observe'},requestPrivate);
  assert.equal(decoded.status,'active');assert.equal(decoded.native_form,undefined);
  const beforeFill=await human({action:'observe',native_fields:true});
  await loginPage.locator('input[type=email]').evaluate(e=>e.multiple=true);
  const privateValues=['  synthetic@example.test \r\n, \tsecond@example.test  ','synthetic\r\n-password-78235','Unicode 🙂 notes\r\nwith newlines\rand more'];
  await browserTakeover(intake,batch(beforeFill,privateValues),requestPrivate);
  const normalized=await loginPage.locator('input:not([type=hidden]):not([disabled]):not([readonly]),textarea').evaluateAll(es=>es.map(e=>e.value));
  assert.deepEqual(normalized,['synthetic@example.test,second@example.test','synthetic-password-78235','Unicode 🙂 notes\nwith newlines\nand more']);
  await loginPage.evaluate(vals=>{const p=document.createElement('p');p.textContent=vals.join(' ');document.body.append(p);},normalized);
  // Native OTP sheet: the user types once into a labeled native field, with the
  // code keyboard/autofill purpose preserved, then sends a private batch over HTTPS.
  assert.equal((await browserTakeover(intake,{action:'finish'},requestPrivate)).status,'finished');
  const firstSnapshot=JSON.stringify(await tool('browser_login_snapshot',{request_id:operation}));
  for(const value of [...privateValues,...normalized])assert.ok(!firstSnapshot.includes(value));
  await loginPage.evaluate(()=>sessionStorage.setItem('synthetic-session-marker','retained'));
  await tool('browser_login_action',{request_id:operation,operation_id:crypto.randomUUID(),action:'navigate',url:origin+'/otp'});
  await loginPage.locator('#otp').waitFor();
  const beforeReentry=structuredClone(durable.get('browser-login:fixture-agent'));
  const reentryOperation=crypto.randomUUID();
  const followup=await tool('request_browser_login_input',{request_id:operation,operation_id:reentryOperation,reason:'Use the private browser to complete sign-in.'});
  assert.equal(followup.status,'input_required');assert.equal(followup.approved,true);
  assert.notEqual(followup.request_id,operation);assert.equal(followup.request_id,followup.challenge_id);
  assert.deepEqual(await tool('request_browser_login_input',{operation_id:reentryOperation,request_id:operation,reason:'Use the private browser to complete sign-in.'}),followup,'reordered retry replays the same fresh panel');
  assert.equal(allocations,1,'reentry reuses the original browser');
  const afterReentry=durable.get('browser-login:fixture-agent');
  assert.equal(afterReentry.sessionId,beforeReentry.sessionId);assert.equal(afterReentry.targetId,beforeReentry.targetId);
  assert.equal(await loginPage.evaluate(()=>sessionStorage.getItem('synthetic-session-marker')),'retained');
  for(const action of [{action:'observe'},{action:'approve'},{action:'cancel'},batch(beforeFill,privateValues)])
    await assert.rejects(loginRuntime.submit({challenge_id:operation,...action},ctx.signal),'old panel cannot control the new epoch');
  assert.deepEqual(await loginRuntime.submit({challenge_id:operation,action:'finish'},ctx.signal),{type:'browser_login_receipt',status:'finished',request_id:operation});
  assert.equal(durable.get('browser-login:fixture-agent').phase,'human','old finish receipt must not release the new epoch');
  await assert.rejects(tool('browser_login_snapshot',{request_id:followup.request_id}),'model stays blocked during native input');
  activeId=followup.request_id;
  intake=decodeVaultIntake({name:'request_browser_login_input',status:'completed',output:JSON.stringify(followup)});
  assert.ok(intake,'account client recognizes followup native intake');
  const described=await requestPrivate('/v1/agents/fixture-agent/browser-vault/takeover',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({challenge_id:activeId,action:'describe'})});
  assert.equal((await described.json()).approved,true,'authenticated describe skips repeated origin review');
  const oldNative=await human({action:'observe',native_fields:true});
  assert.ok(oldNative.native_form.fields.every(f=>Object.keys(f).sort().join(',')==='label,multiline,ref,type'),'old native clients receive the original descriptor keys');
  const hinted=await human({action:'observe',native_fields:true,native_field_hints:true});
  const descriptors=hinted.native_form.fields.map(({ref,...f})=>f);
  assert.deepEqual(descriptors,[
    {label:'Verification code from your device',type:'text',multiline:false,autocomplete:'one-time-code',inputmode:'numeric'},
    {label:'Account',type:'text',multiline:false,autocomplete:'username'},
    {label:'Other',type:'text',multiline:false},
  ]);
  assert.ok(!JSON.stringify(hinted.native_form).includes('webauthn'),'autocomplete is never represented as passkey capability');
  const otpValues=['783492','synthetic-otp-user','synthetic-other'];
  await browserTakeover(intake,batch(hinted,otpValues),requestPrivate);
  assert.deepEqual(await loginPage.locator('input').evaluateAll(es=>es.map(e=>e.value)),otpValues);
  const changedPurpose=await human({action:'observe',native_fields:true,native_field_hints:true});
  await loginPage.locator('#otp').evaluate(e=>e.autocomplete='cc-csc');
  await assert.rejects(browserTakeover(intake,batch(changedPurpose,['must-not-fill','must-not-fill','must-not-fill']),requestPrivate));
  assert.deepEqual(await loginPage.locator('input').evaluateAll(es=>es.map(e=>e.value)),otpValues,'changed purpose rejects the entire batch before mutation');
  await human({action:'observe',native_fields:true,native_field_hints:true});
  await loginPage.evaluate(vals=>{const p=document.createElement('p');p.textContent=vals.join(' ');document.body.append(p);},[...normalized,...otpValues]);
  assert.equal((await browserTakeover(intake,{action:'finish'},requestPrivate)).status,'finished');
  const snapshot=JSON.stringify(await tool('browser_login_snapshot',{request_id:activeId}));
  for(const value of [...privateValues,...normalized,...otpValues]){assert.ok(!snapshot.includes(value));assert.ok(!JSON.stringify([...durable]).includes(value));}
  await tool('browser_login_close',{});
  // Agent chooses the sheet from a redacted page; all user input crosses the
  // authenticated HTTPS boundary, never a model argument or remote click.
  const nativeHuman=async action=>{
    const response=await requestPrivate('/v1/agents/fixture-agent/browser-vault/takeover',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({challenge_id:activeId,...action})});
    assert.equal(response.status,200);return response.json();
  };
  // Detach the observer before runtime target creation, as for the initial
  // login and Namecheap journey; Playwright auto-attachment can pause navigation.
  await loginBrowser.close();loginBrowser=undefined;
  const prepared=await tool('request_browser_login',{operation_id:crypto.randomUUID(),url:origin+'/profile-start',allowed_origins:[origin],defer_input:true});
  assert.equal(prepared.status,'page_ready');activeId=prepared.request_id;
  loginBrowser=await chromium.connectOverCDP(`http://127.0.0.1:${runtimePort}`);
  assert.equal(decodeVaultIntake({name:'request_browser_login',status:'completed',output:JSON.stringify(prepared)}),undefined,'preparation does not present a premature sheet');
  loginPage=loginBrowser.contexts().flatMap(c=>c.pages()).find(p=>p.url()===origin+'/profile-start');
  for(let i=0;!loginPage&&i<100;i++){await new Promise(r=>setTimeout(r,30));loginPage=loginBrowser.contexts().flatMap(c=>c.pages()).find(p=>p.url()===origin+'/profile-start');}
  await loginPage.locator('#open-profile').waitFor();
  const preparedSnapshot=await tool('browser_login_snapshot',{request_id:activeId});
  const preparedAction={request_id:activeId,operation_id:crypto.randomUUID(),action:'click',snapshot_id:preparedSnapshot.snapshot_id,ref:preparedSnapshot.elements.find(e=>e.text==='Open delivery preferences').ref};
  const preparedClick=await tool('browser_login_action',preparedAction);
  assert.equal(preparedClick.status,'action_requested','agent may open the form before asking for input');
  assert.deepEqual(await tool('browser_login_action',preparedAction),preparedClick);
  await loginPage.waitForURL(origin+'/profile');
  await loginPage.locator('#country').waitFor();
  await assert.rejects(human({action:'observe',native_fields:true}),'prepared browser cannot accept human input until requested/reviewed');
  let profileSnapshot=await tool('browser_login_snapshot',{request_id:activeId});
  const selectedFields=snapshot=>[
    {ref:snapshot.elements.find(e=>e.text==='Delivery country').ref,label:'Country for this delivery'},
    {ref:snapshot.elements.find(e=>e.text==='Contact email').ref},
    {ref:snapshot.elements.find(e=>e.text==='Delivery notes').ref},
    {ref:snapshot.elements.find(e=>e.text==='Send status updates').ref},
  ];
  assert.equal(profileSnapshot.elements.find(e=>e.role==='select').native_input,true);
  assert.equal(profileSnapshot.elements.find(e=>e.role==='select').input_type,'select');
  assert.equal(profileSnapshot.elements.find(e=>e.text==='Unavailable choices').native_input,false,'no-selectable-option controls cannot be requested as native');
  const reason='Choose where to deliver and add contact details and instructions.';
  const requestSelection=snapshot=>({request_id:activeId,operation_id:crypto.randomUUID(),snapshot_id:snapshot.snapshot_id,fields:selectedFields(snapshot),reason});
  const invalid=requestSelection(profileSnapshot);invalid.fields[0].value='must-not-accept';
  await assert.rejects(tool('request_browser_login_input',invalid),'model cannot send field values');
  const invalidSelections=[
    {snapshot_id:profileSnapshot.snapshot_id},
    {fields:selectedFields(profileSnapshot)},
    {snapshot_id:profileSnapshot.snapshot_id,fields:[]},
    {snapshot_id:null}, {fields:null}, {reason:''}, {reason:42}, {reason:'x'.repeat(501)},
  ];
  for(const selection of invalidSelections) {
    await assert.rejects(tool('request_browser_login_input',{request_id:activeId,operation_id:crypto.randomUUID(),...selection}));
    assert.equal(durable.get('browser-login:fixture-agent').phase,'prepared','invalid selection cannot acquire user control');
  }
  const staleRequest=requestSelection(profileSnapshot);
  await loginPage.locator('#country option').nth(1).evaluate(e=>e.textContent='Changed country');
  const stale=await tool('request_browser_login_input',staleRequest);
  assert.equal(stale.status,'stale_page');assert.equal(stale.request_id,activeId);
  assert.deepEqual(await tool('request_browser_login_input',staleRequest),stale,'stale request retry is a stable receipt');
  assert.equal(durable.get('browser-login:fixture-agent').phase,'prepared','stale request does not acquire user control');
  await loginPage.locator('#country option').nth(1).evaluate(e=>e.textContent='Canada');
  profileSnapshot=await tool('browser_login_snapshot',{request_id:activeId});
  const selectionArgs=requestSelection(profileSnapshot), selectedPanel=await tool('request_browser_login_input',selectionArgs);
  assert.equal(selectedPanel.status,'input_required');assert.equal(selectedPanel.approved,false);
  assert.deepEqual(await tool('request_browser_login_input',selectionArgs),selectedPanel);
  activeId=selectedPanel.request_id;
  await nativeHuman({action:'approve'});
  const controls={action:'observe',native_fields:true,native_field_hints:true,native_field_controls:true,viewport:{width:390,height:740,mobile:true}};
  let selectedFrame=await nativeHuman(controls);
  assert.equal(selectedFrame.native_form.reason,reason);
  assert.deepEqual(selectedFrame.native_form.fields.map(f=>[f.label,f.type,f.multiline]),[
    ['Country for this delivery','select',false],['Contact email','email',false],['Delivery notes','text',true],['Send status updates','checkbox',false],
  ]);
  assert.equal(selectedFrame.native_form.fields[1].autocomplete,'email');
  assert.deepEqual(selectedFrame.native_form.fields[0].options,[{index:0,label:'Choose country'},{index:1,label:'Canada'},{index:2,label:'United Kingdom'}]);
  assert.equal(selectedFrame.native_form.fields[3].checked,false);
  assert.ok(!JSON.stringify(selectedFrame.native_form).includes('Unrelated field'));
  // Page replacement while the sheet is open returns an explicit stale marker.
  await loginPage.locator('#country').evaluate(e=>e.replaceWith(e.cloneNode(true)));
  const staleSheet=await nativeHuman(controls);
  assert.equal(staleSheet.native_form_status,'stale');assert.equal(staleSheet.native_form,undefined);
  assert.equal((await nativeHuman(controls)).native_form_status,'stale','refresh must not silently switch the requested fields');
  await nativeHuman({action:'finish'});
  profileSnapshot=await tool('browser_login_snapshot',{request_id:activeId});
  const recoveredPanel=await tool('request_browser_login_input',requestSelection(profileSnapshot));
  assert.equal(recoveredPanel.approved,true);activeId=recoveredPanel.request_id;
  selectedFrame=await nativeHuman(controls);
  const chosenValues=['2','synthetic-profile@example.test','Leave by the side door.\nRing once.','true'];
  const invalidOption=batch(selectedFrame,chosenValues);invalidOption.fields[0].value='3';
  const denied=await requestPrivate('/v1/agents/fixture-agent/browser-vault/takeover',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({challenge_id:activeId,...invalidOption})});
  assert.equal(denied.status,409,'disabled options fail before any input');
  assert.equal(await loginPage.locator('#contact').inputValue(),'');
  selectedFrame=await nativeHuman(controls);
  await nativeHuman(batch(selectedFrame,chosenValues));
  assert.equal(await loginPage.locator('#country').inputValue(),'uk');
  assert.equal(await loginPage.locator('#contact').inputValue(),chosenValues[1]);
  assert.equal(await loginPage.locator('#notes').inputValue(),chosenValues[2]);
  assert.equal(await loginPage.locator('#updates').isChecked(),true);
  assert.equal(await loginPage.locator('#unrelated').inputValue(),'');
  assert.deepEqual(await loginPage.evaluate(()=>counts),{input:4,change:4,click:1,remoteClick:0},'native sheet fills all chosen controls without remote clicks or submitting');
  await loginPage.evaluate(values=>{const p=document.createElement('p');p.textContent=values.join(' ');document.body.append(p);},chosenValues);
  await nativeHuman({action:'finish'});
  const profileAfter=await tool('browser_login_snapshot',{request_id:activeId});
  for(const value of chosenValues.slice(1,3)){assert.ok(!JSON.stringify(profileAfter).includes(value));assert.ok(!JSON.stringify([...durable]).includes(value));}
  const profileOutput=new URL('../../../output/private-native-fields/',import.meta.url);mkdirSync(profileOutput,{recursive:true});
  writeFileSync(new URL('agent-selected-input-journey.json',profileOutput),JSON.stringify({prepared,selection:selectionArgs,stale_request:stale,selected_fields:selectedFrame.native_form.fields,reason,stale_sheet:staleSheet.native_form_status,event_counts:await loginPage.evaluate(()=>counts),recovery:'fresh snapshot and sheet in retained browser',redaction:'private values absent from model snapshot and durable storage'},null,2));
  await loginPage.screenshot({path:new URL('agent-selected-profile.png',profileOutput).pathname});
  await tool('browser_login_close',{});
  console.log('PASS: page-aware native selection, grounded labels and reason, private select/checkbox/multiline fill with zero remote clicks, stale page handback and recovery');
  // Public Namecheap login structure, then a synthetic server-verified OTP.
  // No live Namecheap credentials, post-password markup or WebAuthn are used.
  await loginBrowser.close();loginBrowser=undefined;
  const namecheapOperation=crypto.randomUUID();
  const namecheapRequest=await tool('request_browser_login',{operation_id:namecheapOperation,url:origin+'/namecheap/login',allowed_origins:[origin]});
  activeId=namecheapOperation;
  intake=decodeVaultIntake({name:'request_browser_login',status:'completed',output:JSON.stringify(namecheapRequest)});
  assert.ok(intake);
  await human({action:'approve'});
  loginBrowser=await chromium.connectOverCDP(`http://127.0.0.1:${runtimePort}`);
  loginPage=loginBrowser.contexts().flatMap(c=>c.pages()).find(p=>p.url()===origin+'/namecheap/login');
  assert.ok(loginPage);
  await loginPage.locator('input[name=LoginPassword]:visible').waitFor();
  const namecheapFrame=await human({action:'observe',native_fields:true,native_field_hints:true,viewport:{width:390,height:740,mobile:true}});
  assert.deepEqual(namecheapFrame.native_form.fields.map(({ref,...field})=>field),[
    {label:'Username',type:'text',multiline:false},
    {label:'Password',type:'password',multiline:false},
  ],'hidden duplicates and offscreen newsletter excluded; placeholder-only labels survive');
  const namecheapValues=[namecheapSynthetic.username,namecheapSynthetic.password];
  await browserTakeover(intake,batch(namecheapFrame,namecheapValues),requestPrivate);
  assert.deepEqual(await loginPage.locator('input[name=LoginUserName],input[name=LoginPassword]').evaluateAll(es=>es.map(e=>e.value)),['','',...namecheapValues]);
  assert.deepEqual(namecheapFixture.counts,{passwordPosts:0,otpPosts:0,authenticatedVisits:0},'native fill never submits');
  const namecheapClickReceipts=[];
  const clickNamecheap=async label=>{
    assert.equal((await browserTakeover(intake,{action:'finish'},requestPrivate)).status,'finished');
    const snapshot=await tool('browser_login_snapshot',{request_id:activeId});
    const submit=snapshot.elements.find(el=>el.role==='button'&&el.text===label);
    assert.ok(submit,'agent can identify the form submit from the redacted snapshot');
    for(const value of Object.values(namecheapSynthetic))assert.ok(!JSON.stringify(snapshot).includes(value));
    const action={request_id:activeId,operation_id:crypto.randomUUID(),action:'click',snapshot_id:snapshot.snapshot_id,ref:submit.ref};
    const receipt=await tool('browser_login_action',action);
    assert.ok(['action_requested','outcome_unknown'].includes(receipt.status));
    assert.deepEqual(await tool('browser_login_action',action),receipt,'stable operation replay cannot submit twice');
    namecheapClickReceipts.push(receipt.status);
  };
  await clickNamecheap('Submit form');
  await loginPage.waitForURL(origin+'/namecheap/otp');
  assert.equal(namecheapFixture.counts.passwordPosts,1);
  assert.ok(JSON.stringify(await tool('browser_login_snapshot',{request_id:activeId})).includes('Synthetic second-factor code'));
  const namecheapState=structuredClone(durable.get('browser-login:fixture-agent'));
  const namecheapAllocations=allocations;
  const otpRequest=await tool('request_browser_login_input',{request_id:activeId,operation_id:crypto.randomUUID()});
  assert.equal(otpRequest.approved,true);
  assert.notEqual(otpRequest.request_id,activeId);
  const otpState=durable.get('browser-login:fixture-agent');
  assert.equal(otpState.targetId,namecheapState.targetId);assert.equal(otpState.sessionId,namecheapState.sessionId);
  assert.equal(allocations,namecheapAllocations);
  activeId=otpRequest.request_id;
  intake=decodeVaultIntake({name:'request_browser_login_input',status:'completed',output:JSON.stringify(otpRequest)});
  assert.ok(intake);
  const namecheapOtp=await human({action:'observe',native_fields:true,native_field_hints:true});
  assert.deepEqual(namecheapOtp.native_form.fields.map(({ref,...field})=>field),[{label:'Verification code',type:'text',multiline:false,autocomplete:'one-time-code',inputmode:'numeric'}]);
  await browserTakeover(intake,batch(namecheapOtp,[namecheapSynthetic.otp]),requestPrivate);
  assert.equal(namecheapFixture.counts.otpPosts,0,'OTP native fill never submits');
  await clickNamecheap('Verify');
  await loginPage.waitForURL(origin+'/namecheap/account');
  const namecheapSnapshot=JSON.stringify(await tool('browser_login_snapshot',{request_id:activeId}));
  assert.ok(namecheapSnapshot.includes('Synthetic Namecheap-shaped account verified'));
  assert.deepEqual(namecheapFixture.counts,{passwordPosts:1,otpPosts:1,authenticatedVisits:1});
  for(const value of Object.values(namecheapSynthetic)){
    assert.ok(!namecheapSnapshot.includes(value));assert.ok(!JSON.stringify([...durable]).includes(value));
  }
  const namecheapOutput=new URL('../../../output/private-native-fields/',import.meta.url);mkdirSync(namecheapOutput,{recursive:true});
  writeFileSync(new URL('namecheap-journey.json',namecheapOutput),JSON.stringify({
    public_source:'https://www.namecheap.com/myaccount/login/',observed:'2026-10-03',
    scope:'Synthetic structural compatibility only; no authenticated Namecheap or WebAuthn test',
    descriptors:namecheapFrame.native_form.fields.map(({ref,...field})=>field),
    otp_descriptors:namecheapOtp.native_form.fields.map(({ref,...field})=>field),
    merchant_counts:namecheapFixture.counts,click_receipts:namecheapClickReceipts,same_session:true,new_browser_allocations:allocations-namecheapAllocations,
    checks:['duplicate hidden header credentials left empty','offscreen newsletter excluded','placeholder-only labels and generic autocomplete=on','native HTTPS fill does not submit','native fill then Finish then redacted snapshot agent click reaches synthetic OTP','OTP reentry keeps target and pending session cookie','native code hints survive HTTPS','native OTP fill then Finish then redacted snapshot agent click reaches server-authenticated synthetic account','input submit has fixed label without exposing its value','stable action UUID replay submits each form once','no synthetic inputs in snapshot or durable metadata'],
    limitations:['OTP markup is synthetic, not inspected after a live Namecheap login','CAPTCHA/trusted-device challenges untested','no iOS system credential autofill or passkey assertion tested'],
  },null,2));
  await tool('browser_login_close',{});
  console.log('PASS: Namecheap-shaped native fill/Finish -> agent snapshot submit -> retained-session native OTP/Finish -> agent verify -> server-confirmed fixture account; no live Namecheap authentication or passkey claim');
  // Vault Finish: actual runtime and CDP, with HTTP response loss simulated only
  // after the real finish completed. Retry cannot release a newer lease.
  const vaultData=new Map();
  const vaultStorage={get:async k=>structuredClone(vaultData.get(k)),put:async(k,v)=>vaultData.set(k,structuredClone(v)),delete:async k=>vaultData.delete(k),transaction:async f=>f(vaultStorage)};
  const vaultBinding={fetch:async()=>{
    const ws=new WebSocket(`ws://127.0.0.1:${port}${endpoint}`);
    await new Promise((resolve,reject)=>{ws.once('open',resolve);ws.once('error',reject);});ws.accept=()=>{};return {webSocket:ws};
  }};
  const makeVaultRuntime=()=>createManagedBrowserRuntime({ctx:{storage:vaultStorage},env:{MANAGED_BROWSER_PROVIDER:'cloudflare',BROWSER:vaultBinding,LOADER:{}},sessionId:'vault-fixture',privateOnly:true,
    resolveVaultLogin:async()=>({username:'synthetic-vault-user',password:'synthetic-vault-password'}),authorizeVaultAccess:()=>{},
    createRuntime:()=>({connector:{sessionInfo:async()=>({sessionId:'vault-session'}),closeSession:async()=>{}},tools:{},runtime:{expirePaused:async()=>{}}})});
  vaultRuntime=await makeVaultRuntime();
  const vaultTool=(name,args={})=>vaultRuntime.tools.find(t=>t.name===name).handler({...identity,...args},ctx);
  handleControl=(req,res)=>{let body='';req.on('data',c=>body+=c);req.on('end',async()=>{
    res.setHeader('content-type','application/json');res.setHeader('cache-control','no-store');
    try{res.end(JSON.stringify(await vaultRuntime.submitVaultTakeover(JSON.parse(body),ctx.signal)));}catch{res.statusCode=409;res.end('{}');}
  });};
  await page.goto(origin+'/profile');
  const availableNative=await act({...controls});
  assert.ok(!availableNative.native_form.fields.some(f=>f.label==='Unavailable choices'));
  let vaultSnapshot=await vaultTool('browser_vault_snapshot');
  const vaultSelection={operation_id:crypto.randomUUID(),snapshot_id:vaultSnapshot.snapshot_id,fields:selectedFields(vaultSnapshot),reason:'Complete the delivery preferences for this account.'};
  await page.locator('#country option').nth(1).evaluate(e=>e.textContent='Changed country');
  const staleVault=await vaultTool('browser_vault_request_takeover',vaultSelection);
  assert.equal(staleVault.status,'stale_page');
  assert.deepEqual(await vaultTool('browser_vault_request_takeover',vaultSelection),staleVault);
  assert.equal(vaultData.has('browser-vault-takeover:private:cloudflare:vault-fixture'),false,'stale named Vault selection does not acquire human control');
  await page.locator('#country option').nth(1).evaluate(e=>e.textContent='Canada');
  vaultSnapshot=await vaultTool('browser_vault_snapshot');
  const namedArgs={...vaultSelection,operation_id:crypto.randomUUID(),snapshot_id:vaultSnapshot.snapshot_id,fields:selectedFields(vaultSnapshot)};
  const namedPanel=await vaultTool('browser_vault_request_takeover',namedArgs);
  assert.equal(namedPanel.status,'input_required');
  assert.deepEqual(await vaultTool('browser_vault_request_takeover',namedArgs),namedPanel,'named Vault selected sheet replay retains its lease');
  const vaultHuman=async action=>{
    const response=await requestPrivate('/v1/agents/vault-fixture/browser-vault/takeover',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({challenge_id:namedPanel.challenge_id,...action})});
    assert.equal(response.status,200);return response.json();
  };
  const namedFrame=await vaultHuman(controls);
  assert.equal(namedFrame.native_form.reason,namedArgs.reason);
  assert.deepEqual(namedFrame.native_form.fields.map(f=>f.label),['Country for this delivery','Contact email','Delivery notes','Send status updates']);
  const namedValues=['1','synthetic-named@example.test','Deliver upstairs','true'];
  await vaultHuman(batch(namedFrame,namedValues));
  assert.equal(await page.locator('#country').inputValue(),'ca');
  assert.equal(await page.locator('#contact').inputValue(),namedValues[1]);
  assert.equal(await page.locator('#updates').isChecked(),true);
  assert.deepEqual(await page.evaluate(()=>counts),{input:4,change:4,click:1,remoteClick:0});
  await page.evaluate(values=>{const p=document.createElement('p');p.textContent=values.join(' ');document.body.append(p);},namedValues);
  await vaultHuman({action:'finish'});
  const namedAfter=await vaultTool('browser_vault_snapshot');
  for(const value of namedValues.slice(1,3))assert.ok(!JSON.stringify(namedAfter).includes(value));
  writeFileSync(new URL('named-vault-selected-input-journey.json',profileOutput),JSON.stringify({request:namedArgs,stale_request:staleVault,fields:namedFrame.native_form.fields,event_counts:await page.evaluate(()=>counts),redaction:'user values absent from model snapshot'},null,2));
  console.log('PASS: named Vault page-aware selection/reason, stable lease replay, stale snapshot recovery, mixed private HTTPS fill with zero remote clicks and redacted snapshot');
  await vaultTool('browser_vault_action',{operation_id:crypto.randomUUID(),action:'navigate',url:origin+'/react-checkbox'});
  await page.waitForURL(origin+'/react-checkbox');await page.locator('#agreement').waitFor();
  const reactSnapshot=await vaultTool('browser_vault_snapshot');
  let reactPanel=await vaultTool('browser_vault_request_takeover',{operation_id:crypto.randomUUID(),snapshot_id:reactSnapshot.snapshot_id,
    fields:[{ref:reactSnapshot.elements.find(e=>e.role==='checkbox').ref}]});
  const reactHuman=async action=>{
    const response=await requestPrivate('/v1/agents/vault-fixture/browser-vault/takeover',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({challenge_id:reactPanel.challenge_id,...action})});
    assert.equal(response.status,200);return response.json();
  };
  let reactFrame=await reactHuman(controls);
  const reactFilled=await reactHuman(batch(reactFrame,['true']));
  assert.equal(reactFilled.native_form_status,undefined,'successful React rerender is not a stale input failure');
  assert.equal(await page.locator('#agreement').isChecked(),true);
  assert.equal(await page.locator('#react-state').textContent(),'true','controlled React state follows native checkbox activation');
  assert.equal(await page.evaluate(()=>window.reactChanges),1);
  assert.equal(reactPosts.length,0,'native checkbox fill does not submit the form');
  await reactHuman({action:'finish'});
  const sameStateSnapshot=await vaultTool('browser_vault_snapshot');
  reactPanel=await vaultTool('browser_vault_request_takeover',{operation_id:crypto.randomUUID(),snapshot_id:sameStateSnapshot.snapshot_id,
    fields:[{ref:sameStateSnapshot.elements.find(e=>e.role==='checkbox').ref}]});
  reactFrame=await reactHuman(controls);
  await reactHuman(batch(reactFrame,['true']));
  assert.equal(await page.evaluate(()=>window.reactChanges),1,'already matching checkbox state is not activated again');
  await reactHuman({action:'finish'});
  const reactSubmitSnapshot=await vaultTool('browser_vault_snapshot');
  await vaultTool('browser_vault_action',{operation_id:crypto.randomUUID(),action:'click',snapshot_id:reactSubmitSnapshot.snapshot_id,
    ref:reactSubmitSnapshot.elements.find(e=>e.text==='Save checkbox choice').ref});
  for(let i=0;reactPosts.length===0&&i<100;i++)await new Promise(r=>setTimeout(r,10));
  assert.deepEqual(reactPosts,[{checked:true}],'authorized agent submit sends the updated React state');
  writeFileSync(new URL('react-checkbox-journey.json',profileOutput),JSON.stringify({checked:true,react_state:'true',change_calls:1,native_fill_submissions:0,agent_submit_posts:reactPosts},null,2));
  console.log('PASS: controlled React checkbox native activation updates state once; native fill does not submit; agent submit sends checked:true');
  // Keep a native OTP draft while unrelated form text changes in real Chrome.
  const countdownEvidence=[];
  for (const mutation of ['countdown','countdown-before-sheet','hidden','action','method','target','label','purpose','replacement','option']) {
    await page.goto(origin+'/otp-countdown');
    const snapshot=await vaultTool('browser_vault_snapshot');
    if(mutation==='countdown-before-sheet'){await page.evaluate(()=>startCountdown());await page.waitForFunction(()=>ticks>=3);}
    const harmless=mutation.startsWith('countdown');
    const panel=await vaultTool('browser_vault_request_takeover',{operation_id:crypto.randomUUID(),snapshot_id:snapshot.snapshot_id,
      fields:[{ref:snapshot.elements.find(e=>e.text==='Verification code').ref}]});
    assert.equal(panel.status,'input_required');
    const send=action=>requestPrivate('/v1/agents/vault-fixture/browser-vault/takeover',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({challenge_id:panel.challenge_id,...action})});
    const observe=await send(controls);assert.equal(observe.status,200);
    const frame=await observe.json(), draft='314159';
    assert.equal(frame.native_form.fields.length,1);
    if(mutation!=='countdown-before-sheet')await page.evaluate(()=>startCountdown());
    const ticksBeforeDraft=await page.evaluate(()=>ticks);
    await page.waitForFunction(before=>ticks>=before+3,ticksBeforeDraft);
    await page.evaluate(kind=>{
      if(kind==='hidden')document.querySelector('#nonce').value='rotated';
      if(kind==='action')document.querySelector('form').action='/other-verification';
      if(kind==='method')document.querySelector('form').method='get';
      if(kind==='target')document.querySelector('form').target='_blank';
      if(kind==='label')document.querySelector('label').firstChild.textContent='Recovery secret';
      if(kind==='purpose')document.querySelector('#countdown-otp').autocomplete='new-password';
      if(kind==='replacement'){const e=document.querySelector('#countdown-otp');e.replaceWith(e.cloneNode(true));}
      if(kind==='option')document.querySelector('select').selectedIndex=1;
    },mutation);
    const filled=await send(batch(frame,[draft]));
    assert.equal(filled.status,harmless?200:409,mutation+' while native draft is pending');
    assert.equal(await page.locator('#countdown-otp').inputValue(),harmless?draft:'');
    const refreshed=await send(controls);assert.equal(refreshed.status,200);
    const after=await refreshed.json();
    assert.equal(after.native_form_status,harmless?undefined:'stale',mutation+' observation after fill');
    countdownEvidence.push({mutation,ticks:await page.evaluate(()=>ticks),fill_status:filled.status,observation:after.native_form_status||'active'});
    assert.equal((await send({action:'finish'})).status,200);
  }
  writeFileSync(new URL('otp-countdown-journey.json',profileOutput),JSON.stringify(countdownEvidence,null,2));
  console.log('PASS: native OTP draft survives form countdown; hidden value, destination, method, target, label, purpose, node replacement and selected-option changes reject before filling');
  for(const selection of invalidSelections) {
    await assert.rejects(vaultTool('browser_vault_request_takeover',{operation_id:crypto.randomUUID(),...selection}));
    assert.equal(vaultData.has('browser-vault-takeover:private:cloudflare:vault-fixture'),false,'invalid selection cannot acquire a Vault lease');
  }
  const fallbackArgs={operation_id:crypto.randomUUID(),reason:'Use the private browser because the native sheet is unavailable.'};
  const lease=await vaultTool('browser_vault_request_takeover',fallbackArgs);
  assert.deepEqual(await vaultTool('browser_vault_request_takeover',fallbackArgs),lease,'fallback retries retain their lease');
  await assert.rejects(vaultTool('browser_vault_snapshot'),'model remains blocked during browser fallback');
  const vaultIntake={operation:'browser_takeover',kind:'login',agent_id:'vault-fixture',challenge_id:lease.challenge_id};
  const fallbackFrame=await browserTakeover(vaultIntake,{action:'observe'},requestPrivate);
  assert.equal(fallbackFrame.status,'active');
  assert.ok(fallbackFrame.image,'browser fallback returns its private viewport');
  assert.equal(fallbackFrame.native_form,undefined,'legacy browser fallback needs no native sheet');
  const fallbackInput=await page.locator('#countdown-otp').boundingBox();
  await browserTakeover(vaultIntake,{action:'click',x:(fallbackInput.x+fallbackInput.width/2)/fallbackFrame.width,y:(fallbackInput.y+fallbackInput.height/2)/fallbackFrame.height},requestPrivate);
  await browserTakeover(vaultIntake,{action:'type',text:'synthetic-fallback-code'},requestPrivate);
  assert.ok((await page.locator('#countdown-otp').inputValue()).endsWith('synthetic-fallback-code'),'private browser keyboard input reaches the existing page');
  writeFileSync(new URL('browser-fallback-journey.json',profileOutput),JSON.stringify({
    login:{request:{operation_id:reentryOperation,request_id:operation,reason:'Use the private browser to complete sign-in.'},status:followup.status,same_session:true,replay:true},
    vault:{request:fallbackArgs,status:lease.status,viewport:{width:fallbackFrame.width,height:fallbackFrame.height},native_form:false,private_keyboard_input:true,replay:true},
    rejected_selections:invalidSelections,model_observation_blocked:true,
  },null,2));
  console.log('PASS: reason-only fallback for login and Vault; partial native selections rejected; private HTTPS viewport and keyboard work with model observation blocked');
  const lostFinish=async(url,init)=>{const response=await requestPrivate(url,init);assert.equal(response.status,200);await response.body.cancel();throw Error('Synthetic lost Finish response');};
  await assert.rejects(browserTakeover(vaultIntake,{action:'finish'},lostFinish));
  assert.equal((await browserTakeover(vaultIntake,{action:'finish'},requestPrivate)).status,'finished');
  const newerLease=await vaultTool('browser_vault_request_takeover');
  assert.notEqual(newerLease.challenge_id,lease.challenge_id);
  assert.equal((await browserTakeover(vaultIntake,{action:'finish'},requestPrivate)).status,'finished');
  assert.equal(vaultData.get('browser-vault-takeover:private:cloudflare:vault-fixture').id,newerLease.challenge_id,'old finish does not release newer lease');
  await assert.rejects(vaultRuntime.submitVaultTakeover({challenge_id:lease.challenge_id,action:'observe'},ctx.signal));
  await assert.rejects(vaultRuntime.submitVaultTakeover({challenge_id:crypto.randomUUID(),action:'finish'},ctx.signal));
  assert.equal((await browserTakeover({...vaultIntake,challenge_id:newerLease.challenge_id},{action:'finish'},requestPrivate)).status,'finished');
  assert.equal((await browserTakeover(vaultIntake,{action:'finish'},requestPrivate)).status,'finished','first receipt survives second completion');
  await vaultRuntime.close();vaultRuntime=await makeVaultRuntime();
  assert.equal((await browserTakeover({...vaultIntake,challenge_id:newerLease.challenge_id},{action:'finish'},requestPrivate)).status,'finished','durable finish survives runtime recreation');
  assert.equal((await browserTakeover(vaultIntake,{action:'finish'},requestPrivate)).status,'finished','both epochs remain retryable after recreation');
  const output=new URL('../../../output/private-native-fields/',import.meta.url);mkdirSync(output,{recursive:true});
  writeFileSync(new URL('takeover-journey.json',output),JSON.stringify({batch_ms:batchMs,checks:['vault Finish response loss retries exact durable receipt','old vault Finish cannot release newer lease','both vault Finish epochs remain retryable after later completion and runtime recreation','same-session fresh-ID OTP reentry without allocation/navigation','reordered operation replay returns identical fresh panel','old panel input and cancel rejected; old finish does not release new epoch','authenticated describe reports prior approval','original and OTP secrets remain redacted after reentry','OTP autocomplete and numeric keyboard hints survive private HTTPS fill','aria-labelledby labels identify OTP fields','hint opt-in preserves old native descriptor schema','unrecognized hint values and iframe fields are excluded','webauthn suffix is not passkey capability','changed input purpose rejects batch before mutation','OTP values redacted from model snapshot and durable storage','legacy clients receive no native_form until explicit opt-in','explicit opt-out and legacy observation retain viewport','document-bound labels and types without values','single native setter plus input/change per field','Unicode batch','replayed batch rejected','lost batch response is consumed; explicit observation recovers','HTTPS account decoder accepts new optional metadata','browser-normalized CR/LF and multiple-email whitespace variants redacted','formless and methodless custom JS login returns filled/action_required','private-login batch values redacted from model snapshot and durable storage','replaced or occluded element rejects entire batch before mutation','same-origin reload rejects stale document','forged and duplicate refs rejected','viewport touch/keyboard fallback retained']},null,2));
  console.log('PASS: vault Finish loss/retry over HTTPS, newer-lease protection, durable receipt after runtime recreation');
  console.log('PASS: same-session OTP reentry, fresh challenge, replay, stale-panel fencing, redaction continuity');
  console.log('PASS: native OTP hints, accessible labels, private HTTPS fill, purpose-change rejection, legacy descriptor compatibility');
  console.log('PASS: HTTPS decoder compatibility, raw/normalized private-login snapshot redaction, synthetic JS custom-login action_required');
  console.log('PASS: native batched form fill, stale/replaced/forged refs fail closed; batch '+batchMs+'ms');
  console.log('PASS: mobile viewport, native touch focus, keyboard traits, Unicode edit/delete, real touch scrolling, cancel recovery, viewport cleanup');
} finally {
  await vaultRuntime?.close();
  await loginRuntime?.close();
  privateCdp?.close();
  await loginBrowser?.close();
  await stopChrome(runtimeChrome);
  await browser?.close();
  await stopChrome(chrome);
  if(server) { server.closeAllConnections(); await new Promise(resolve=>server.close(resolve)); }
  rmSync(temp,{recursive:true,force:true,maxRetries:5,retryDelay:100});
}
