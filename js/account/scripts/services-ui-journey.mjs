// Production React -> HTTP -> authenticated managed routes -> real workerd Vault/phone DOs.
// Only SMS sign-in enrollment and the external carrier are synthetic.
import assert from 'node:assert/strict';
import http from 'node:http';
import { createHmac } from 'node:crypto';
import { createRequire } from 'node:module';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { routeLinkPreview } from '../worker/linkPreview.ts';
const root = fileURLToPath(new URL('../../../', import.meta.url));
process.chdir(root);
const require = createRequire(path.join(root, 'js/account/package.json'));
const egressRequire = createRequire(path.join(root, 'js/egress/package.json'));
const {build} = require('esbuild');
const {chromium} = require('playwright-core');
const {Miniflare, convertV4MiniflareOptions, Log, LogLevel} = createRequire(egressRequire.resolve('wrangler/package.json'))('miniflare');
const output = path.join(root, 'output/services-ui');
await mkdir(output, {recursive:true});
const seed = 'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ';
const owner = '11111111-1111-4111-8111-111111111111';
const sid = 'AC' + '1'.repeat(32), token = 'synthetic-phone-auth-token';
const webhook = 'https://account.test/v1/services/phone/webhook';
let purchases=0, releases=0, failPurchase=false;
const trace=[], logs=[];
class CapturedLog extends Log { logWithLevel(level,message) { logs.push(String(message)); } }
async function bundle(source, resolveDir, wasm=false) {
  const compiled = await build({stdin:{contents:source,resolveDir}, bundle:true,write:false,format:'esm',platform:'node',conditions:['workerd'],external:['cloudflare:*','node:*'],alias:{'node-rsa':path.join(root,'js/nanocodex/tools/browser/unsupportedNodeRsa.mjs')},plugins:[{name:'transports',setup(b){
    b.onResolve({filter:/^nanocodex\/wasm$/},()=>({path:'./nanocodex.wasm',external:true}));
    b.onResolve({filter:/^\.\/whatsapp-runtime$/},()=>({path:path.join(root,'js/egress/test/whatsapp/runtime.fixture.ts')}));
  }}]});
  return [{type:'ESModule',path:'worker.js',contents:compiled.outputFiles[0].text},...(wasm?[{type:'CompiledWasm',path:'nanocodex.wasm',contents:await readFile(path.join(root,'js/nanocodex/pkg-web/nanocodex_bg.wasm'))}]:[])];
}
const accountSource = `
import {routeCredentialRequest} from './src/credentials.ts';
import {routeServicesRequest} from './src/services-http.ts';
import {authenticate,routeAccountRequest,UserAccount,Organization,ApiKeyRecord,NonceStorage,ensureAccount} from './src/account-auth.ts';
import {Kv} from 'accounts/server';
export {UserAccount,Organization,ApiKeyRecord,NonceStorage};
export default {async fetch(request,env){
 const url=new URL(request.url);
 if(url.pathname==='/__enroll') {
   await ensureAccount(env,'${owner}',true);
   const token='s_'+crypto.randomUUID().replaceAll('-','')+'A'.repeat(11);
   await Kv.durableObject(env.NANOCODEX_AUTH,{name:'account'}).set('session:'+token,{userId:'${owner}',authentication:'sms_otp',issuedAt:Date.now()/1000,expiresAt:Date.now()/1000+3600});
   return Response.json({cookie:'nanocodex_account='+token});
 }
 const credential=await routeCredentialRequest(request,env,url);if(credential)return credential;
 if(url.pathname.startsWith('/v1/services'))return routeServicesRequest(request,env.NANOCODEX,await authenticate(request,env,url));
 const account=await routeAccountRequest(request,env,url);if(account)return account;
 return new Response(null,{status:404});
}};`;
const common={compatibilityDate:'2026-07-29',compatibilityFlags:['nodejs_compat'],bindings:{ENVIRONMENT:'test'}};
const mf = new Miniflare(convertV4MiniflareOptions({log:new CapturedLog(LogLevel.WARN),handleStructuredLogs:entry=>logs.push(JSON.stringify(entry)),workers:[
 {...common,name:'account',modules:await bundle(accountSource,path.join(root,'js/managed')),serviceBindings:{NANOCODEX:'egress'},durableObjects:{NANOCODEX_USERS:{className:'UserAccount',useSQLite:true},NANOCODEX_ORGANIZATIONS:{className:'Organization',useSQLite:true},NANOCODEX_API_KEYS:{className:'ApiKeyRecord',useSQLite:true},NANOCODEX_AUTH:{className:'NonceStorage',useSQLite:true}}},
 {...common,name:'egress',modules:await bundle("export {default} from './src/egress.ts';export * from './src/egress.ts';",path.join(root,'js/egress'),true),bindings:{...common.bindings,CREDENTIAL_ENCRYPTION_KEY:'MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY',TWILIO_ACCOUNT_SID:sid,TWILIO_AUTH_TOKEN:token,PHONE_WEBHOOK_URL:webhook,PHONE_PROVISIONING_ENABLED:'true',PHONE_MAX_MONTHLY_PRICE:'2',PHONE_MAX_INBOUND_SMS_PRICE:'0.02',PHONE_MAX_NUMBERS_PER_OWNER:'3'},durableObjects:{USER_CREDENTIALS:{className:'UserCredentialBroker',useSQLite:true},AGENT_SUBJECTS:{className:'AgentSubjectDirectory',useSQLite:true},PHONE_SERVICE_ACCOUNTS:{className:'PhoneServiceAccount',useSQLite:true}},outboundService:async request=>{
   const url=new URL(request.url);
   assert.equal(request.headers.get('authorization'),'Basic '+Buffer.from(`${sid}:${token}`).toString('base64'));
   if(url.hostname==='pricing.twilio.com')return Response.json(url.pathname.includes('PhoneNumbers')?{price_unit:'USD',phone_number_prices:[{number_type:'local',current_price:'1.15'}]}:{price_unit:'USD',inbound_sms_prices:[{number_type:'local',current_price:'0.0083'}]});
   assert.equal(url.hostname,'api.twilio.com');
   if(url.pathname.includes('AvailablePhoneNumbers'))return Response.json({available_phone_numbers:(url.searchParams.has('Contains')?[url.searchParams.get('Contains')]:['+14155550101','+14155550102']).map(phone_number=>({phone_number,iso_country:'US',capabilities:{SMS:true}}))});
   if(request.method==='POST'&&url.pathname.endsWith('/IncomingPhoneNumbers.json')){
     purchases++;const fields=new URLSearchParams(await request.text());assert.equal(fields.get('SmsUrl'),webhook);
     if(failPurchase){failPurchase=false;return new Response(null,{status:503});}
     return Response.json({sid:'PN'+String(purchases).padStart(32,'0'),account_sid:sid,phone_number:fields.get('PhoneNumber'),sms_url:webhook,sms_method:'POST'},{status:201});
   }
   if(request.method==='DELETE'){releases++;return new Response(null,{status:204});}
   throw new Error('Unexpected external request '+url.hostname+url.pathname);
 }},
]}));
await mf.ready;
await build({stdin:{contents:`
import './src/index.css';import React from 'react';import {createRoot} from 'react-dom/client';
import {QueryClient,QueryClientProvider} from '@tanstack/react-query';
import {AccountSessionProvider} from './src/AccountSession';import {BrowserRouter} from 'react-router';
import {Vault} from './src/Vault';import {PhoneService} from './src/PhoneService';
createRoot(document.getElementById('root')).render(<QueryClientProvider client={new QueryClient()}><AccountSessionProvider><BrowserRouter>{location.pathname==='/services/phone'?<PhoneService/>:<Vault/>}</BrowserRouter></AccountSessionProvider></QueryClientProvider>);
`,resolveDir:path.join(root,'js/account'),sourcefile:'services-journey.tsx',loader:'tsx'},bundle:true,format:'esm',jsx:'automatic',external:['/paradigm-mark.svg'],outfile:path.join(output,'journey.js')});
// Serve the shipped asset policy through the production document response router.
const assetHeaders=new Headers({'content-type':'text/html'});
let globalHeaders=false;
for(const line of (await readFile(path.join(root,'js/account/public/_headers'),'utf8')).split('\n')) {
  if(line==='/*'){globalHeaders=true;continue;}
  if(globalHeaders&&line&&!/^\s/.test(line))break;
  const header=globalHeaders&&line.match(/^\s+([^:]+):\s*(.*)$/);
  if(header)assetHeaders.set(header[1],header[2]);
}
let origin;
const server=http.createServer(async(req,res)=>{
  try {
    const url=new URL(req.url,origin);
    if(url.pathname.startsWith('/v1/')){
      const chunks=[];for await(const chunk of req)chunks.push(chunk);
      const body=Buffer.concat(chunks);const input=body.length?JSON.parse(body.toString()):undefined;
      const response=await mf.dispatchFetch(url.href,{method:req.method,headers:req.headers,...(body.length?{body}: {})});
      trace.push({path:url.pathname,method:req.method,status:response.status,...(input?{fields:Object.keys(input),...(input.operation_id?{operation_id:input.operation_id}:{})}: {})});
      res.statusCode=response.status;response.headers.forEach((value,key)=>res.setHeader(key,value));res.end(Buffer.from(await response.arrayBuffer()));return;
    }
    if(url.pathname==='/journey.js'||url.pathname==='/journey.css'){res.setHeader('content-type',url.pathname.endsWith('.js')?'text/javascript':'text/css');res.end(await readFile(path.join(output,url.pathname.slice(1))));return;}
    const asset=()=>new Response('<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1"><link rel="stylesheet" href="/journey.css"></head><body><div id="root"></div><script type="module" src="/journey.js"></script></body></html>',{headers:assetHeaders});
    const response=await routeLinkPreview(new Request(url,{method:req.method,headers:req.headers}),{ASSETS:{fetch:async()=>asset()}},url)??asset();
    const callbackPage=['/vault','/services/phone'].includes(url.pathname)&&url.searchParams.has('enrollment_origin')&&url.searchParams.has('state')&&req.headers['sec-fetch-dest']!=='iframe';
    assert.equal(response.headers.get('cross-origin-opener-policy'),callbackPage?'unsafe-none':'same-origin-allow-popups');
    assert.equal(response.headers.get('x-frame-options'),'SAMEORIGIN');
    assert.ok(response.headers.get('content-security-policy').includes("frame-ancestors 'self'"));
    if(callbackPage)assert.equal(response.headers.get('cache-control'),'no-store');
    trace.push({verified:'production routeLinkPreview document headers',path:url.pathname,callbackPage,coop:response.headers.get('cross-origin-opener-policy'),csp:response.headers.get('content-security-policy'),xfo:response.headers.get('x-frame-options'),cache:response.headers.get('cache-control')});
    res.statusCode=response.status;response.headers.forEach((value,key)=>res.setHeader(key,value));res.end(Buffer.from(await response.arrayBuffer()));
  }catch(error){console.error(error);res.statusCode=500;res.end('fixture transport error');}
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));origin=`http://127.0.0.1:${server.address().port}`;
const parent=http.createServer((req,res)=>{res.setHeader('content-type','text/html');res.end('<!doctype html><button id="open">Enroll authenticator</button><script>window.receipts=[];addEventListener("message",e=>receipts.push({origin:e.origin,value:e.data}));document.getElementById("open").onclick=()=>window.open('+JSON.stringify(origin)+'+(new URLSearchParams(location.search).get("service")=="phone"?"/services/phone?operation_id="+new URLSearchParams(location.search).get("operation_id")+"&":"/vault?")+(new URLSearchParams(location.search).get("service")==="general"?"":"service="+(new URLSearchParams(location.search).get("service")||"totp")+"&"+(new URLSearchParams(location.search).has("kind")?"kind="+new URLSearchParams(location.search).get("kind")+"&":""))+"enrollment_origin="+encodeURIComponent(location.origin)+"&state=synthetic_enrollment_state_123","enrollment");</script>');});
await new Promise(resolve=>parent.listen(0,'127.0.0.1',resolve));
const browser=await chromium.launch({executablePath:process.env.CHROME_PATH||chromium.executablePath(),headless:true,args:['--no-sandbox']});
const context=await browser.newContext({viewport:{width:390,height:844}}), page=await context.newPage();
const errors=[];context.on('page',p=>p.on('pageerror',error=>errors.push(error.message)));page.on('pageerror',error=>errors.push(error.message));
async function see(text,p=page){await p.getByText(text,{exact:true}).waitFor();}
async function click(name,p=page){await p.getByRole('button',{name,exact:true}).click();}
async function addItem(label,p=page){await click('Add item',p);await p.getByRole('menuitem',{name:label,exact:true}).click();}
async function seedForm(p=page){if(!new URL(p.url()).searchParams.has('add'))await addItem('Add authenticator',p);await p.getByLabel('Enrollment format').selectOption('seed');await p.getByLabel('Name',{exact:true}).fill('Synthetic authenticator');await p.getByLabel('Website origin').fill('https://login.example.test');await p.getByLabel('Setup key',{exact:true}).fill(seed);await p.getByLabel('Issuer',{exact:true}).fill('Example');await p.getByLabel('Account label').fill('synthetic@example.test');}
async function approveQuote(){assert.equal(await page.getByRole('button',{name:'Approve purchase',exact:true}).isEnabled(),false);await page.getByRole('checkbox').check();await click('Approve purchase');}
try {
  await page.goto(origin+'/vault?service=totp');await page.getByRole('textbox',{name:'Mobile number'}).waitFor();
  const enrolled=await(await mf.dispatchFetch(origin+'/__enroll')).json();
  const [cookieName,...cookieValue]=enrolled.cookie.split('=');await context.addCookies([{name:cookieName,value:cookieValue.join('='),url:origin}]);
  await page.goto(origin+'/vault?service=totp');await seedForm();
  await page.getByLabel('Website origin').fill('https://login.example.test/path');const before=trace.filter(r=>r.method==='POST').length;await click('Save');await page.getByRole('alert').filter({hasText:'Enter an exact HTTPS origin'}).waitFor();assert.equal(trace.filter(r=>r.method==='POST').length,before);
  await page.getByLabel('Website origin').fill('https://login.example.test');await click('Save');await see('Authenticator saved to Vault.');await see('Synthetic authenticator');assert.equal(await page.getByRole('dialog').count(),0);
  assert.equal((await page.locator('body').textContent()).includes(seed),false);
  await addItem('Add authenticator');await page.getByLabel('Name',{exact:true}).fill('URI authenticator');await page.getByLabel('Website origin').fill('https://login.example.test');await page.getByLabel('Authenticator URI',{exact:true}).fill('otpauth://totp/Example:test?secret=BAD');await click('Save');await page.getByRole('alert').filter({hasText:'Couldn’t save the item'}).waitFor();assert.equal(await page.getByLabel('Authenticator URI',{exact:true}).inputValue(),'');
  await page.getByLabel('Authenticator URI',{exact:true}).fill(`otpauth://totp/Example:test?secret=${seed}&issuer=Example`);await click('Save');await see('URI authenticator');
  await page.screenshot({path:path.join(output,'vault-saved.png'),fullPage:true});
  const parentPage=await context.newPage();
  const parentOrigin=`http://127.0.0.1:${parent.address().port}`;
  async function openVault(service,operationId='',kind='') {
    await parentPage.goto(parentOrigin+'?service='+service+'&operation_id='+operationId+(kind?'&kind='+kind:''));
    const opened=parentPage.waitForEvent('popup');await click('Enroll authenticator',parentPage);
    const popup=await opened;await popup.waitForLoadState();
    assert.equal(await popup.evaluate(()=>window.opener!==null),true);
    return popup;
  }
  let popup=await openVault('totp');await seedForm(popup);
  assert.equal(await popup.getByRole('button',{name:'Save',exact:true}).isEnabled(),false);
  assert.equal(await parentPage.evaluate(()=>window.receipts.length),0);
  await popup.getByRole('checkbox',{name:/Share completion with/}).check();
  await popup.screenshot({path:path.join(output,'vault-recipient-consent.png'),fullPage:true});
  await click('Save',popup);await parentPage.waitForFunction(()=>window.receipts.length===1,undefined,{timeout:5000});
  const callback=await parentPage.evaluate(()=>window.receipts[0]);assert.equal(callback.origin,origin);assert.deepEqual(Object.keys(callback.value).sort(),['type','service','state','vault_id','kind','name','origin'].sort());assert.equal(callback.value.origin,'https://login.example.test');assert.equal(callback.value.state,'synthetic_enrollment_state_123');assert.equal(JSON.stringify(callback).includes(seed),false);trace.push({verified:'explicit recipient consent metadata-only enrollment callback',callback});await popup.close();
  const privateInputs = [seed,'synthetic-login-password-981','synthetic-enrollment-key-982','4242424242424242','731','219 Synthetic Private Lane','+14155550987','synthetic-picker-private-key'];
  async function assertNoPrivateSurface(p) {
    const surface = await p.locator('body').evaluate(body => body.innerText + body.outerHTML + [...body.querySelectorAll('input,textarea')].map(input => input.value).join(' '));
    const receipts = JSON.stringify(await parentPage.evaluate(()=>window.receipts));
    for (const secret of [privateInputs[0], privateInputs[1], privateInputs[2], privateInputs[3], privateInputs[7]]) {
      assert.equal(surface.includes(secret),false,'secret retained in completed/cancelled DOM');
      assert.equal(receipts.includes(secret),false,'secret disclosed in callback');
      assert.equal(JSON.stringify(trace).includes(secret),false,'secret disclosed in trace');
      assert.equal(logs.join('\n').includes(secret),false,'secret disclosed in worker log');
    }
  }
  const enrollmentCases = [
    {kind:'login',label:'Add login',fields:{'Website (optional)':'https://login.example.test','Username':'synthetic-user','Password':privateInputs[1]},origin:'https://login.example.test'},
    {kind:'api_key',label:'Add API key',fields:{'API key':privateInputs[2]}},
    {kind:'card',label:'Add card',fields:{'Card number':privateInputs[3],'Expiry month':'12','Expiry year':'2035','CVV':privateInputs[4],'Billing ZIP':'94105'}},
    {kind:'address',label:'Add address',fields:{'Address line 1':privateInputs[5],'City':'San Francisco','State':'CA','ZIP':'94105','Country':'US'}},
    {kind:'phone',label:'Add phone',fields:{'Phone number':privateInputs[6]}},
    {kind:'totp',label:'Add authenticator',fields:{}}
  ];
  const writeCount=()=>trace.filter(r=>['POST','PUT','PATCH','DELETE'].includes(r.method)).length;
  for (const specimen of enrollmentCases) {
    const beforeOpen=writeCount();
    popup=await openVault('enroll','',specimen.kind);
    await popup.getByRole('heading',{name:'Vault',exact:true}).waitFor();
    if(specimen.kind==='totp') await seedForm(popup);
    else {
      if(!new URL(popup.url()).searchParams.has('add'))await addItem(specimen.label,popup);
      await popup.getByLabel('Name',{exact:true}).fill('Hosted '+specimen.kind);
      for(const [label,value] of Object.entries(specimen.fields))await popup.getByLabel(label,{exact:true}).fill(value);
    }
    assert.equal(writeCount(),beforeOpen,'opening/filling never writes');
    assert.deepEqual(await parentPage.evaluate(()=>window.receipts),[]);
    assert.equal(await popup.getByRole('button',{name:'Save',exact:true}).isEnabled(),false);
    await click('Cancel',popup);
    await popup.getByRole('button',{name:'Add item',exact:true}).waitFor();
    assert.equal(writeCount(),beforeOpen,'cancel never writes');
    assert.deepEqual(await parentPage.evaluate(()=>window.receipts),[]);
    await assertNoPrivateSurface(popup);
    if(specimen.kind==='totp')await seedForm(popup);
    else {
      await addItem(specimen.label,popup);
      for(const [label,value] of Object.entries(specimen.fields))await popup.getByLabel(label,{exact:true}).fill(value);
    }
    await popup.getByLabel('Name',{exact:true}).fill('Hosted '+specimen.kind);
    await popup.getByRole('checkbox',{name:'Share completion with '+parentOrigin+'. This includes the saved name, kind, and Vault ID.',exact:true}).check();
    await click('Save',popup);
    await parentPage.waitForFunction(()=>window.receipts.length===1,undefined,{timeout:5000});
    await popup.getByRole('button',{name:'Add item',exact:true}).waitFor();
    const receipt=await parentPage.evaluate(()=>window.receipts[0]);
    const expectedOrigin=specimen.kind==='totp'?'https://login.example.test':specimen.origin;
    assert.equal(receipt.origin,origin);
    assert.deepEqual(Object.keys(receipt.value).sort(),['type','service','state','vault_id','kind','name',...(expectedOrigin?['origin']:[])].sort());
    assert.equal(receipt.value.type,'nanocodex:service-enrollment');assert.equal(receipt.value.service,'vault');
    assert.equal(receipt.value.state,'synthetic_enrollment_state_123');assert.equal(receipt.value.kind,specimen.kind);
    assert.equal(receipt.value.name,'Hosted '+specimen.kind);assert.ok(receipt.value.vault_id);
    assert.equal(receipt.value.origin,expectedOrigin);assert.equal(writeCount(),beforeOpen+1);
    await assertNoPrivateSurface(popup);
    trace.push({verified:'hosted '+specimen.kind+' explicit recipient consent; cancel/open no write; metadata only; private inputs cleared',callback:receipt});
    await popup.close();
  }
  console.log('PASS all six hosted Vault kinds: explicit recipient consent, cancellation without writes, metadata-only callback, secrets cleared');
  // Regression: a general Vault link carrying callback parameters must never publish new item metadata.
  popup=await openVault('general');await seedForm(popup);await popup.getByLabel('Name',{exact:true}).fill('General Vault private item');
  assert.equal(await popup.getByRole('checkbox',{name:/Share completion/}).count(),0);
  await click('Save',popup);await see('General Vault private item',popup);
  await popup.screenshot({path:path.join(output,'vault-general-no-callback.png'),fullPage:true});
  // A browser round-trip synchronizes after the completed response and reloaded metadata.
  await parentPage.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
  assert.deepEqual(await parentPage.evaluate(()=>window.receipts),[]);
  await addItem('Add API key',popup);await popup.getByLabel('Name',{exact:true}).fill('Existing API credential');await popup.getByLabel('API key',{exact:true}).fill('synthetic-picker-private-key');await click('Save',popup);await see('Existing API credential',popup);
  assert.deepEqual(await parentPage.evaluate(()=>window.receipts),[]);await popup.close();
  const postCount=trace.filter(r=>r.method==='POST').length;
  popup=await openVault('select');await see('Choose a Vault item',popup);
  await popup.getByText(parentOrigin,{exact:true}).waitFor();
  assert.equal(await popup.getByRole('button',{name:'Share selected item with '+parentOrigin,exact:true}).isEnabled(),false);
  assert.equal(await parentPage.evaluate(()=>window.receipts.length),0);
  const selectedRadio=popup.getByRole('radio',{name:/Existing API credential/});const selectedId=await selectedRadio.inputValue();await selectedRadio.check();
  assert.equal(await parentPage.evaluate(()=>window.receipts.length),0);
  await popup.screenshot({path:path.join(output,'vault-picker-consent.png'),fullPage:true});
  await click('Share selected item with '+parentOrigin,popup);
  await parentPage.waitForFunction(()=>window.receipts.length===1,undefined,{timeout:5000});
  assert.equal(await popup.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
  const selection=await parentPage.evaluate(()=>window.receipts[0]);
  assert.deepEqual(Object.keys(selection.value).sort(),['type','service','action','state','vault_id','kind','name'].sort());assert.equal(selection.value.action,'select');assert.equal(selection.value.name,'Existing API credential');assert.equal(selection.value.kind,'api_key');assert.equal(selection.value.vault_id,selectedId);
  assert.equal(trace.filter(r=>r.method==='POST').length,postCount);
  trace.push({verified:'general Vault parameters do not disclose metadata; explicit existing selection returns metadata only, no POST or grant',selection});
  await popup.close();
  await page.goto(origin+'/vault?service=select');await page.getByRole('alert').filter({hasText:'valid requesting origin and state'}).waitFor();assert.equal(await page.getByRole('radio').count(),0);
  // The hosted pages remain unavailable to cross-origin frames even though popups can communicate.
  const blockedFrame=parentPage.waitForEvent('console',{predicate:message=>message.text().includes('frame-ancestors'),timeout:5000});
  await parentPage.evaluate(url=>{const frame=document.createElement('iframe');frame.src=url;document.body.append(frame);},origin+'/vault?service=select');
  await blockedFrame;
  assert.equal(await parentPage.locator('iframe').contentFrame().getByRole('heading',{name:'Choose a Vault item'}).count(),0);
  console.log('PASS signed-in Vault seed/URI enrollment, exact-origin input, real broker validation, private metadata callback');
  await page.goto(origin+'/services/phone');await see('No dedicated numbers yet.');await page.getByLabel('Area code (optional)').fill('415');await click('Find available numbers');await click('Review quote for +14155550101');await see('Awaiting your approval');await see('USD 1.15 / month');await see('USD 0.0083 / message');assert.equal(purchases,0);await page.screenshot({path:path.join(output,'phone-quote.png'),fullPage:true});const stagedId=trace.findLast(r=>r.method==='POST'&&r.path==='/v1/services/phone/numbers').operation_id;
  popup=await openVault('phone',stagedId);await see('Awaiting your approval',popup);assert.equal(purchases,0);
  assert.equal(await parentPage.evaluate(()=>window.receipts.length),0);
  await popup.getByRole('checkbox',{name:/Share completion with/}).check();await popup.screenshot({path:path.join(output,'phone-popup-consent.png'),fullPage:true});
  await click('Deny request',popup);await parentPage.waitForFunction(()=>window.receipts.length===1,undefined,{timeout:5000});
  const phoneCallback=await parentPage.evaluate(()=>window.receipts[0]);assert.deepEqual(phoneCallback,{origin,value:{type:'nanocodex:service-enrollment',state:'synthetic_enrollment_state_123',service:'phone',operation_id:stagedId,status:'denied'}});
  trace.push({verified:'hosted phone opens existing approval request, explicit recipient share, terminal result metadata only',phoneCallback});
  await popup.close();await parentPage.close();await click('Check status');await see('Request denied');assert.equal(purchases,0);await click('Close request');
  await click('Review quote for +14155550101');await see('Awaiting your approval');await approveQuote();await see('Number activated');assert.equal(purchases,1);await page.reload();await page.getByRole('button',{name:'Read messages for +14155550101',exact:true}).waitFor();assert.equal(purchases,1);
  const form=new URLSearchParams({AccountSid:sid,To:'+14155550101',From:'+14155550999',MessageSid:'SM'+'2'.repeat(32),Body:'Synthetic verification code 481927 <script>unsafe()</script>',NumMedia:'0'});
  const signature=createHmac('sha1',token).update(webhook+[...form.keys()].sort().map(key=>key+form.get(key)).join('')).digest('base64');
  const egress=await mf.getWorker('egress');assert.equal((await egress.fetch('https://phone-service.internal/v1/phone/webhook',{method:'POST',headers:{'content-type':'application/x-www-form-urlencoded','x-twilio-signature':signature},body:form.toString()})).status,200);
  await click('Read messages for +14155550101');await see('Synthetic verification code 481927 <script>unsafe()</script>');await page.screenshot({path:path.join(output,'phone-inbox.png'),fullPage:true});
  await click('Release +14155550101');await see('Awaiting your approval');await page.getByText(/You may lose access to accounts/).waitFor();assert.equal(await page.getByRole('button',{name:'Confirm release',exact:true}).isEnabled(),false);assert.equal(releases,0);await page.getByRole('checkbox').check();await click('Confirm release');await see('Number released');assert.equal(releases,1);await click('Check status');assert.equal(releases,1);await click('Close request');
  failPurchase=true;await click('Find available numbers');await click('Review quote for +14155550102');await see('Awaiting your approval');await approveQuote();await see('Outcome unknown');assert.equal(purchases,2);assert.equal(await page.getByRole('button',{name:'Approve purchase',exact:true}).count(),0);await page.reload();await see('Outcome unknown');await click('Check status');await see('Outcome unknown');assert.equal(purchases,2);assert.equal(await page.getByRole('button',{name:'Find available numbers',exact:true}).isEnabled(),false);await page.screenshot({path:path.join(output,'phone-unknown.png'),fullPage:true});
  assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true);
  assert.deepEqual(errors,[]);assert.equal(logs.join('\n').includes(seed),false);assert.equal(JSON.stringify(trace).includes(seed),false);
  trace.push({verified:'real browser + real account/egress runtime, synthetic carrier only',purchases,releases,errors});
  console.log('PASS browse/quote/deny/approve, incoming SMS, release confirmation, reload and unknown reconciliation; 2 carrier purchase calls and 1 release, no duplicate');
} catch(error) {
  await page.screenshot({path:path.join(output,'failure.png'),fullPage:true});
  await writeFile(path.join(output,'failure.txt'),String(error)+'\n'+await page.locator('body').innerText());
  throw error;
} finally {
  await writeFile(path.join(output,'http-journey.json'),JSON.stringify(trace,null,2));await writeFile(path.join(output,'worker.log'),logs.join('\n'));
  await browser.close();await new Promise(resolve=>server.close(resolve));await new Promise(resolve=>parent.close(resolve));await mf.dispose();
}
