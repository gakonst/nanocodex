// Production components and CSS; only HTTP boundaries are synthetic. No real mutations.
import http from 'node:http';
import assert from 'node:assert/strict';
import {readFile,writeFile,mkdir} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {createRequire} from 'node:module';
import path from 'node:path';
const root=fileURLToPath(new URL('../../../',import.meta.url));process.chdir(root);
const out='output/account-workspace';await mkdir(out,{recursive:true});
const checkpoint=async text=>writeFile(`${out}/browser-progress.md`,`${new Date().toISOString()}\n${text}\n`);
await checkpoint('Draft saved; building production bundle.');
const require=createRequire(path.resolve('js/account/package.json'));
const {build}=require('esbuild');const {chromium}=require('playwright-core');
await build({stdin:{contents:`import React from 'react';import {createRoot} from 'react-dom/client';import {QueryClient,QueryClientProvider} from '@tanstack/react-query';import {BrowserRouter} from 'react-router';import {AccountSessionProvider} from './src/AccountSession';import {DeviceConnect} from './src/DeviceConnect';import './src/index.css';createRoot(document.getElementById('root')).render(<QueryClientProvider client={new QueryClient({defaultOptions:{queries:{retry:false}}})}><BrowserRouter><AccountSessionProvider><DeviceConnect/></AccountSessionProvider></BrowserRouter></QueryClientProvider>);`,resolveDir:path.resolve('js/account'),sourcefile:'account-journey.tsx',loader:'tsx'},bundle:true,external:['/paradigm-mark.svg'],format:'esm',jsx:'automatic',outfile:`${out}/journey.js`,loader:{'.woff2':'dataurl','.png':'dataurl','.svg':'dataurl'}});
const requests=[],errors=[],shots=[];let signedOut=false, credentialsFailure=false, pendingChatGpt=false, cloudflareConnected=false, cloudflareLostReply=false;
const wallet='0x1111111111111111111111111111111111111111';
const user={id:'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',persistent:true,address:wallet};
let entries=[['login','Example account'],['api_key','Development key'],['card','Travel card'],['address','Home address']].map(([kind,name],i)=>({id:String(i+1).repeat(22),kind,name,created_at:1}));
const server=http.createServer(async(req,res)=>{const url=new URL(req.url,'http://localhost');
if(url.pathname.startsWith('/v1/')){
 requests.push({method:req.method,path:url.pathname});res.setHeader('content-type','application/json');
 const reply=(body,status=200)=>{res.statusCode=status;res.end(JSON.stringify(body));};
 if(url.pathname==='/v1/auth/sms/start'&&req.method==='POST')return reply({challenge_id:'synthetic-challenge',expires_in:300});
 if(url.pathname==='/v1/auth/sms/verify'&&req.method==='POST'){signedOut=false;return reply({user});}
 if(url.pathname.startsWith('/v1/credentials/vault/')){
  if(signedOut)return reply({error:'unauthorized'},401);
  if(req.method==='POST'){let raw='';for await(const chunk of req)raw+=chunk;const values=JSON.parse(raw);const entry={id:'z'.repeat(22),kind:url.pathname.split('/').at(-1),name:values.name,created_at:2};entries.push(entry);return reply(entry);}
  if(req.method==='DELETE'){entries=entries.filter(e=>e.id!==url.pathname.split('/').at(-1));return reply({deleted:true});}
 }
 if(url.pathname==='/v1/connectors/cloudflare'&&req.method==='POST'){
  let raw='';for await(const chunk of req)raw+=chunk;const input=JSON.parse(raw);
  assert.deepEqual(input,{vault_id:'2'.repeat(22),account_id:'a'.repeat(32)});
  cloudflareConnected=true;if(cloudflareLostReply)return reply({error:'synthetic unconfirmed update'},503);
  return reply({connected:true});
 }
 if(url.pathname==='/v1/connectors/cloudflare/connections/'+ 'f'.repeat(43)&&req.method==='DELETE'){cloudflareConnected=false;return reply({disconnected:true});}
 if(req.method!=='GET')return reply({error:'Unexpected fixture mutation'},405);
 if(url.pathname==='/v1/me')return reply(signedOut?{error:'unauthorized'}:{user},signedOut?401:200);
 if(url.pathname==='/v1/credentials'){
  if(credentialsFailure)return reply({error:'synthetic outage'},503);
  return reply({ready:true,active:'openai',openai:{connected:true},chatgpt:{connected:false,accounts:[],...(pendingChatGpt?{login:{state:'pending',verification_url:'https://auth.example/device',user_code:'SYNTHETIC',expires_at:Date.now()+60000,poll_after_ms:30000}}:{})},claude:{connected:false},ssh:[],vault:entries});
 }
 if(url.pathname==='/v1/credentials/chatgpt/login')return reply({state:'pending',verification_url:'https://auth.example/device',user_code:'SYNTHETIC',expires_at:Date.now()+60000,poll_after_ms:30000});
 if(url.pathname==='/v1/wallet')return reply({address:wallet,original_address:wallet,mode:'internal'});
 if(url.pathname==='/v1/wallet/balance')return reply({account:wallet,balance:'5000000',decimals:6,symbol:'MACH',token:'0x20c000000000000000000000f37de3740adec032'});
 if(url.pathname==='/v1/machine-usd/config')return reply({min_usd_amount_cents:500,max_usd_amount_cents:10000,onramp_enabled:true,chain_id:4217,token_address:'0x20c000000000000000000000f37de3740adec032',stripe_publishable_key:'pk_test_fixture'});
 if(url.pathname==='/v1/connectors')return reply({connectors:{cloudflare:cloudflareConnected?{connected:true,connections:[{id:'f'.repeat(43),label:'Synthetic Cloudflare',account_id:'a'.repeat(32)}]}:{connected:false},github:{connected:true,connections:[{id:'a'.repeat(43),label:'Example developer',account_id:'example',capabilities:['github']}]}}});
 if(url.pathname==='/v1/connectors/mcp-connections')return reply({mcp_connections:[]});
 if(url.pathname==='/v1/account/communication')return reply({email:null,phone:null});
 if(url.pathname==='/v1/account/admin')return reply({admin:false});
 if(url.pathname==='/v1/api-keys')return reply({data:[{id:'abcdefghijkl',label:'Terminal',prefix:'nc_example',createdAt:1}]});
 return reply({connected:false,connections:[],data:[]});
}
if(['/journey.js','/journey.css'].includes(url.pathname)){res.setHeader('content-type',url.pathname.endsWith('.js')?'text/javascript':'text/css');res.end(await readFile(out+url.pathname));return;}
res.setHeader('content-type','text/html');res.end('<!doctype html><html><head><meta name="viewport" content="width=device-width, initial-scale=1"><link rel="stylesheet" href="/journey.css"></head><body><div id="root"></div><script type="module" src="/journey.js"></script></body></html>');});
await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin=`http://127.0.0.1:${server.address().port}`;
const browser=await chromium.launch({executablePath:process.env.CHROME_PATH||chromium.executablePath(),headless:true});
const contextFor=async(viewport,theme='light')=>{
 const context=await browser.newContext({viewport,colorScheme:theme});
 await context.route('**/*',route=>route.request().url().startsWith(origin)?route.continue():route.abort());
 await context.addInitScript(t=>{document.addEventListener('DOMContentLoaded',()=>{document.documentElement.dataset.theme=t;});localStorage.setItem('nanocodex-theme',t);},theme);
 return context;
};
const screenshot=async(page,name)=>{
 assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),true,`${name}: horizontal overflow`);
 await page.screenshot({path:`${out}/${name}.png`,fullPage:true});shots.push(name);
};
const writes=()=>requests.filter(r=>r.method==='POST'&&r.path.startsWith('/v1/credentials/vault/')).length;
try{
 for(const [device,viewport] of Object.entries({desktop:{width:1280,height:900},mobile:{width:360,height:800}}))for(const theme of ['light','dark']){
  const context=await contextFor(viewport,theme);const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));
  for(const section of ['connections','vault','wallet','access']){
   await page.goto(origin+'/connect'+(section==='connections'?'':'/'+section));await page.waitForLoadState('networkidle');
   await page.getByRole('heading',{level:1,name:section==='access'?'API access':section[0].toUpperCase()+section.slice(1),exact:true}).waitFor();
   assert.equal(await page.locator('[role="alert"]:visible').count(),0,section+' has no error: '+await page.locator('[role="alert"]:visible').allTextContents());
   if(section==='vault')await page.getByText('Example account',{exact:true}).waitFor();
   if(section==='wallet')await page.getByText('Balance: $5.00',{exact:true}).waitFor();
   await screenshot(page,`${device}-${theme}-${section}`);
  }
  await page.goto(origin+'/connect/vault?add=login');await page.getByLabel('Password',{exact:true}).waitFor();await screenshot(page,`${device}-${theme}-add-login`);
  await context.close();
 }
 const context=await contextFor({width:390,height:844});const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));
 signedOut=true;
 await page.goto(origin+'/connect/vault?add=login');await page.locator('input[type="tel"]').waitFor();await screenshot(page,'mobile-sign-in');
 await page.locator('input[type="tel"]').fill('+15555550123');await page.getByRole('button',{name:'Text me a code',exact:true}).click();
 await page.getByLabel('6-digit code',{exact:true}).fill('123456');await page.getByRole('button',{name:'Continue',exact:true}).click();
 await page.getByLabel('Password',{exact:true}).waitFor();assert.equal(new URL(page.url()).search,'?add=login');assert.equal(writes(),0);
 await page.getByLabel('Name',{exact:true}).fill('Journey login');await page.getByLabel('Username',{exact:true}).fill('synthetic-user');await page.getByLabel('Password',{exact:true}).fill('synthetic-password');
 await page.getByRole('button',{name:'Save',exact:true}).click();await page.getByText('Journey login',{exact:true}).waitFor();assert.equal(writes(),1);assert.equal(new URL(page.url()).search,'');
 assert.equal(await page.locator('input[type="password"]').count(),0);assert.equal((await page.locator('body').innerText()).includes('synthetic-password'),false);
 await page.getByLabel('Search vault',{exact:true}).fill('Journey');assert.equal(await page.locator('.vault-items > li').count(),1);
 await page.getByRole('button',{name:'Delete Journey login',exact:true}).click();assert.equal(requests.filter(r=>r.method==='DELETE').length,0);
 await page.getByRole('button',{name:'Cancel',exact:true}).click();assert.equal(requests.filter(r=>r.method==='DELETE').length,0);
 await page.getByRole('button',{name:'Delete Journey login',exact:true}).click();await page.getByRole('button',{name:'Delete',exact:true}).click();await page.getByText('No matching items',{exact:true}).waitFor();assert.equal(requests.filter(r=>r.method==='DELETE').length,1);
 await page.getByLabel('Search vault',{exact:true}).fill('');await page.getByRole('button',{name:'Cards',exact:true}).click();await page.getByText('Travel card',{exact:true}).waitFor();assert.equal(await page.locator('.vault-items > li').count(),1);
 await page.goto(origin+'/connect/vault?add=api_key');await page.getByLabel('API key',{exact:true}).fill('synthetic-key');await page.getByRole('button',{name:'Cancel',exact:true}).click();assert.equal(writes(),1);await page.locator('.vault-inline-entry').waitFor({state:'detached'});assert.equal(await page.locator('input[type="password"]').count(),0);
 for(const [path,title] of [['/connect','Connections'],['/connect/wallet','Wallet'],['/connect/access','API access'],['/connect/vault','Vault']]){
  await page.getByRole('navigation',{name:'Account navigation'}).getByRole('link',{name:title,exact:true}).click();await page.waitForURL(origin+path);assert.equal(new URL(page.url()).pathname,path);await page.reload();await page.getByRole('heading',{level:1,name:title,exact:true}).waitFor();
 }
 await page.goto(origin+'/connect?connect=constructor');await page.getByRole('heading',{level:1,name:'Connections',exact:true}).waitFor();
 await page.goto(origin+'/connect?connect=github');await page.locator('[data-provider="github"] button').first().waitFor();assert.equal(await page.locator('[data-provider="github"] button').first().evaluate(el=>el===document.activeElement),true);
 await page.goto(origin+'/connect?connect=cloudflare');
 const cf=page.locator('[data-provider="cloudflare"]');await cf.getByLabel('Cloudflare Vault API key').waitFor();
 assert.equal(requests.filter(r=>r.method==='POST'&&r.path==='/v1/connectors/cloudflare').length,0);
 assert.equal(await cf.getByRole('button',{name:'Connect Cloudflare',exact:true}).isEnabled(),false);
 await cf.getByLabel('Cloudflare Vault API key').selectOption('2'.repeat(22));await cf.getByLabel('Cloudflare account ID').fill('a'.repeat(32));
 await screenshot(page,'mobile-cloudflare-from-vault');cloudflareLostReply=true;
 await cf.getByRole('button',{name:'Connect Cloudflare',exact:true}).click();await cf.getByRole('alert').waitFor();
 assert.equal(await cf.getByRole('button',{name:'Connect Cloudflare',exact:true}).isEnabled(),false);
 await cf.getByRole('button',{name:'Check status',exact:true}).click();await cf.getByText('Synthetic Cloudflare',{exact:true}).waitFor();
 assert.equal(requests.filter(r=>r.method==='POST'&&r.path==='/v1/connectors/cloudflare').length,1);
 await cf.getByRole('button',{name:'Revoke Synthetic Cloudflare',exact:true}).click();await cf.getByText('Synthetic Cloudflare',{exact:true}).waitFor({state:'detached'});
 assert.equal(requests.filter(r=>r.method==='DELETE'&&r.path==='/v1/connectors/cloudflare/connections/'+'f'.repeat(43)).length,1);
 await page.goto(origin+'/connect?connector=github&connector_result=failed');await page.getByText('GitHub couldn’t be connected. Try again.',{exact:true}).waitFor();
 pendingChatGpt=true;await page.goto(origin+'/connect');await page.getByText('SYNTHETIC',{exact:true}).waitFor();await page.reload();await page.getByText('SYNTHETIC',{exact:true}).waitFor();pendingChatGpt=false;
 credentialsFailure=true;await page.goto(origin+'/connect/vault');await page.getByRole('alert').waitFor();credentialsFailure=false;await page.getByRole('button',{name:'Retry',exact:true}).click();await page.getByText('Example account',{exact:true}).waitFor();
 await page.goto(origin+'/connect/vault?add=login');await page.getByLabel('Password',{exact:true}).fill('discard-on-expiry');signedOut=true;await page.getByLabel('Name',{exact:true}).fill('Expired');await page.getByLabel('Username',{exact:true}).fill('fixture');await page.getByRole('button',{name:'Save',exact:true}).click();await page.locator('input[type="tel"]').waitFor();assert.equal(await page.locator('input[type="password"]').count(),0);
 assert.deepEqual(errors,[]);await context.close();
 await writeFile(`${out}/requests.json`,JSON.stringify({requests,errors,shots},null,2));await checkpoint(`PASS: desktop/mobile light/dark; all routes; sign-in deep link; single save; cancel; search/filter; delete confirmation; callback error; pending login reload; retry; expired session clears secret. ${shots.length} screenshots. No page errors.`);
 console.log('PASS account workspace browser journey ('+shots.length+' screenshots)');
}finally{await browser.close();await new Promise(r=>server.close(r));}
