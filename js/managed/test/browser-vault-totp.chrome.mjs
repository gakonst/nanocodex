// Real saved-login tool -> supported OTP -> account snapshot journey. Synthetic data only.
// Only Cloudflare allocation/storage and the private Vault resolver are local adapters.
import assert from 'node:assert/strict';
import https from 'node:https';
import { readFileSync, mkdtempSync, rmSync, readdirSync, mkdirSync, writeFileSync } from 'node:fs';
import { execFileSync, spawn } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { registerHooks } from 'node:module';
const adapter = `export const connectBrowser=()=>{throw Error("Unused public browser")};export const createBrowserSession=(b,o)=>b.create(o);export const deleteBrowserSession=(b,id)=>b.delete?.(id);
export class DurableBrowserSessionStore { constructor(storage){this.storage=storage} get(k){return this.storage.get(k)} set(k,v){return this.storage.put(k,v)} delete(k){return this.storage.delete(k)} async acquireLock(){return {release:async()=>{}}} }`;
registerHooks({resolve(specifier, context, next) {
  if (specifier === 'agents/browser') return {url:'data:text/javascript,'+encodeURIComponent(adapter),shortCircuit:true};
  if (specifier === 'agents/browser/ai') return {url:'data:text/javascript,export const createBrowserRuntime=()=>{throw Error("Use local allocation")};',shortCircuit:true};
  return next(specifier.startsWith('./browser-') && !specifier.endsWith('.ts') ? specifier+'.ts' : specifier, context);
}});
const {createManagedBrowserRuntime} = await import('../src/browser-runtime.ts');
const {default:WebSocket} = await import('ws');
const packages = new URL('../../../node_modules/.pnpm/',import.meta.url);
const entry = readdirSync(packages).find(name=>/^playwright-core@/.test(name));
const {chromium} = await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`,packages));
const temp=mkdtempSync(join(tmpdir(),'vault-totp-'));
const username='synthetic-totp-user',password='synthetic-totp-password',code='539217';
const loginId='l'.repeat(22),totpId='t'.repeat(22),foreignTotp='f'.repeat(22);
const trace=[],data=new Map(); let loseReceipt=false,sessionId='totp-session';
const storage={get:async k=>structuredClone(data.get(k)),put:async(k,v)=>{
  if(loseReceipt && k.startsWith('private-browser-operation:') && v.state==='done'){loseReceipt=false;throw Error('Synthetic receipt loss');}
  data.set(k,structuredClone(v));
},delete:async k=>data.delete(k),transaction:async f=>f(storage)};
let chrome,browser,runtime,server,page,changeDocument=false,resolutions=0,posts=0,authenticated=false;
try {
  execFileSync('openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',join(temp,'key'),'-out',join(temp,'cert'),'-days','1','-subj','/CN=localhost'],{stdio:'ignore'});
  const otp='<h1>Verification</h1><form method="post" action="/verify"><label>Verification code<input name="code" autocomplete="one-time-code" inputmode="numeric"></label><button>Verify</button></form>';
  server=https.createServer({key:readFileSync(join(temp,'key')),cert:readFileSync(join(temp,'cert'))},(req,res)=>{
    res.setHeader('content-type','text/html');
    if(req.method==='POST') {let body='';req.on('data',chunk=>body+=chunk);req.on('end',()=>{
      const fields=new URLSearchParams(body);
      if(req.url==='/login') {assert.equal(fields.get('user'),username);assert.equal(fields.get('password'),password);res.end(otp);}
      else {posts++;authenticated=fields.get('code')===code;res.end(`<h1>${authenticated?'Synthetic account access verified':'Verification failed'}</h1><p>Echo ${fields.get('code')}</p>`);}
    });return;}
    res.end(req.url==='/otp'?otp:'<form method="post" action="/login"><input name="user" autocomplete="username"><input name="password" type="password"><button>Sign in</button></form>');
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  const origin=`https://127.0.0.1:${server.address().port}`;
  chrome=spawn(process.env.CHROME_PATH||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',[
    ...(process.platform==='linux'?['--no-sandbox']:[]),'--headless','--disable-dev-shm-usage','--ignore-certificate-errors','--no-first-run','--no-default-browser-check','--remote-debugging-port=0',`--user-data-dir=${join(temp,'profile')}`,'about:blank'],{stdio:'ignore'});
  for(let i=0;i<100;i++){try{readFileSync(join(temp,'profile','DevToolsActivePort'));break;}catch{if(chrome.exitCode!==null)throw Error('Chrome exited');await new Promise(r=>setTimeout(r,100));}}
  const [port,endpoint]=readFileSync(join(temp,'profile','DevToolsActivePort'),'utf8').trim().split('\n');
  browser=await chromium.connectOverCDP(`http://127.0.0.1:${port}`);page=await browser.contexts()[0].newPage();await page.goto(origin);
  const observer=await page.context().newCDPSession(page),target=await observer.send('Target.getTargetInfo');
  const identity={vault_id:loginId,target_id:target.targetInfo.targetId,expected_origin:origin};
  const binding={fetch:async()=>{const socket=new WebSocket(`ws://127.0.0.1:${port}${endpoint}`);await new Promise((r,j)=>{socket.once('open',r);socket.once('error',j);});socket.accept=()=>{};return {webSocket:socket};}};
  const makeRuntime=()=>createManagedBrowserRuntime({ctx:{storage},env:{MANAGED_BROWSER_PROVIDER:'cloudflare',BROWSER:binding,LOADER:{}},sessionId:'totp-fixture',privateOnly:true,
    authorizeVaultAccess:ctx=>{if(ctx.sessionId!=='owner')throw Error('forbidden');},
    resolveVaultLogin:async(request,ctx)=>{if(ctx.sessionId!=='owner'||request.vault_id!==loginId)throw Error('denied');return {username,password};},
    resolveVaultTotp:async(request,ctx)=>{resolutions++;if(ctx.sessionId!=='owner'||request.totp_vault_id!==totpId||request.expected_origin!==origin)throw Error('denied');if(changeDocument){changeDocument=false;await page.reload();}return code;},
    createRuntime:()=>({connector:{sessionInfo:async()=>({sessionId}),closeSession:async()=>{}},tools:{},runtime:{expirePaused:async()=>{}}})});
  runtime=await makeRuntime();
  const context={sessionId:'owner',callId:'totp-fixture',signal:new AbortController().signal};
  const call=async(name,input,ctx=context)=>{try{const result=await runtime.tools.find(t=>t.name===name).handler(input,ctx);trace.push({tool:name,result});return result;}catch(error){trace.push({tool:name,error:String(error)});throw error;}};
  const fill=(extra={})=>({...identity,totp_vault_id:totpId,operation_id:crypto.randomUUID(),...extra});
  await assert.rejects(call('browser_vault_fill_totp',fill()),/selected private login/);
  const login=await call('browser_vault_fill',{...identity,username_selector:'input[name=user]',password_selector:'input[name=password]',submit:true,operation_id:crypto.randomUUID()});assert.equal(login.status,'submitted');
  await page.locator('input[autocomplete="one-time-code"]').waitFor();
  const status=await call('browser_vault_status',identity);assert.equal(status.status,'otp_form');
  await assert.rejects(call('browser_vault_fill_totp',fill(),{...context,sessionId:'other'}),/forbidden/);
  for(const extra of [{vault_id:'x'.repeat(22)},{target_id:'other-target'},{expected_origin:'https://different.example'}]) await assert.rejects(call('browser_vault_fill_totp',fill(extra)));
  assert.equal(resolutions,0);
  const wrong=fill({totp_vault_id:foreignTotp});assert.equal((await call('browser_vault_fill_totp',wrong)).status,'outcome_unknown');
  assert.equal((await call('browser_vault_fill_totp',wrong)).status,'outcome_unknown');assert.equal(resolutions,1);assert.equal(posts,0);
  const args=fill(),before=resolutions;
  assert.deepEqual(await call('browser_vault_fill_totp',args),{status:'submitted'});
  await page.waitForURL(origin+'/verify');assert.equal(authenticated,true);assert.equal(posts,1);
  assert.deepEqual(await call('browser_vault_fill_totp',Object.fromEntries(Object.entries(args).reverse())),{status:'submitted'});assert.equal(resolutions,before+1);assert.equal(posts,1);
  await assert.rejects(call('browser_vault_fill_totp',{...args,totp_vault_id:foreignTotp}),/different arguments/);
  const snapshot=await call('browser_vault_snapshot',identity);assert.ok(JSON.stringify(snapshot).includes('Synthetic account access verified'));assert.ok(!JSON.stringify(snapshot).includes(code));
  await page.goto(origin+'/otp');changeDocument=true;
  const stale=fill(),beforeStale=resolutions;assert.equal((await call('browser_vault_fill_totp',stale)).status,'outcome_unknown');
  assert.equal(await page.locator('input[name=code]').inputValue(),'');assert.equal(posts,1);
  assert.equal((await call('browser_vault_fill_totp',stale)).status,'outcome_unknown');assert.equal(resolutions,beforeStale+1);
  // A lost durable completion receipt follows actual submission. Replay never resubmits.
  const lost=fill();loseReceipt=true;assert.equal((await call('browser_vault_fill_totp',lost)).status,'outcome_unknown');
  await page.waitForURL(origin+'/verify');assert.equal(posts,2);
  const beforeReplay=resolutions;assert.equal((await call('browser_vault_fill_totp',lost)).status,'outcome_unknown');assert.equal(resolutions,beforeReplay);assert.equal(posts,2);
  await call('browser_vault_snapshot',identity);
  await runtime.close();runtime=await makeRuntime();
  await assert.rejects(call('browser_vault_snapshot',identity),/redaction state was lost/);
  await assert.rejects(call('browser_vault_fill_totp',lost),/redaction state was lost/);
  for(const secret of [username,password,code]) {assert.ok(!JSON.stringify(trace).includes(secret));assert.ok(!JSON.stringify([...data]).includes(secret));}
  const report={checks:['selected login/target/origin and owner enforced','saved-login fill -> supported OTP -> server-verified account snapshot','same operation replay generates and submits once','different replay arguments rejected','wrong TOTP item denied without submission','document changed during broker resolution rejected before injection','lost completion receipt remains unknown without retry','OTP echo redacted from snapshot','restart loses ephemeral redaction and fails closed','tool results and durable receipts contain no secrets'],trace};
  const output=new URL('../../../output/browser-vault-totp/',import.meta.url);mkdirSync(output,{recursive:true});writeFileSync(new URL('journey.json',output),JSON.stringify(report,null,2));
  console.log('PASS: saved Vault TOTP Chrome journey; 2 verified submissions; 10 security/recovery checks; secret-free trace in output/browser-vault-totp/journey.json');
} finally {
  await runtime?.close();await browser?.close();
  if(chrome && chrome.exitCode===null){chrome.kill();await new Promise(r=>chrome.once('exit',r));}
  if(server)await new Promise(r=>server.close(r));rmSync(temp,{recursive:true,force:true});
}
