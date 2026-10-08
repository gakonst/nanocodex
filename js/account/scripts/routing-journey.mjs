// Route journey against the production vite client bundle (dist/client). Only
// HTTP boundaries are synthetic: documents follow the worker contract (legacy
// 302 via legacyRedirectPath, SPA shell otherwise) and /v1 is a signed-in fixture.
// Run: node --experimental-strip-types scripts/routing-journey.mjs [distClientDir]
import http from 'node:http';
import assert from 'node:assert/strict';
import {readFile,writeFile,mkdir,stat} from 'node:fs/promises';
import {createRequire} from 'node:module';
import path from 'node:path';
import {legacyRedirectPath} from '../src/navigation.ts';
const here=path.dirname(new URL(import.meta.url).pathname);
const dist=path.resolve(process.argv[2]||path.join(here,'../dist/client'));
const out=path.resolve(here,'../../../output/routing-journey');await mkdir(out,{recursive:true});
const log=[];const step=async t=>{log.push(`${new Date().toISOString()} ${t}`);await writeFile(`${out}/progress.md`,log.join('\n')+'\n');};
const require=createRequire(path.join(here,'../package.json'));const {chromium}=require('playwright-core');
const wallet='0x1111111111111111111111111111111111111111';
const user={id:'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',persistent:true,address:wallet};
const types={'.js':'text/javascript','.css':'text/css','.svg':'image/svg+xml','.png':'image/png','.woff2':'font/woff2','.json':'application/json','.wasm':'application/wasm'};
const documents=[];
const server=http.createServer(async(req,res)=>{const url=new URL(req.url,'http://localhost');
 if(url.pathname.startsWith('/v1/')){res.setHeader('content-type','application/json');const reply=(b,s=200)=>{res.statusCode=s;res.end(JSON.stringify(b));};
  if(url.pathname==='/v1/me')return reply({user});
  if(url.pathname==='/v1/credentials')return reply({ready:true,active:'openai',openai:{connected:true},chatgpt:{connected:false,accounts:[]},claude:{connected:false},ssh:[],vault:[]});
  if(url.pathname==='/v1/connectors')return reply({connectors:{github:{connected:true,connections:[{id:'a'.repeat(43),label:'Example developer',account_id:'example',capabilities:['github']}]}}});
  if(url.pathname==='/v1/connectors/mcp-connections')return reply({mcp_connections:[]});
  if(url.pathname==='/v1/account/communication')return reply({email:null,phone:null});
  if(url.pathname==='/v1/account/admin')return reply({admin:false});
  if(url.pathname==='/v1/wallet')return reply({address:wallet,original_address:wallet,mode:'internal'});
  if(req.method!=='GET')return reply({error:'fixture is read-only'},405);
  return reply({connected:false,connections:[],data:[],agents:[],threads:[],items:[]});}
 const file=path.join(dist,path.normalize(url.pathname));
 if(url.pathname!=='/'&&file.startsWith(dist)&&await stat(file).then(s=>s.isFile(),()=>false)){res.setHeader('content-type',types[path.extname(file)]||'application/octet-stream');res.end(await readFile(file));return;}
 documents.push(url.pathname+url.search);
 const legacy=legacyRedirectPath(url);if(legacy){res.writeHead(302,{location:legacy,'cache-control':'no-store'});res.end();return;}
 res.setHeader('content-type','text/html');res.end(await readFile(path.join(dist,'index.html')));});
await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin=`http://127.0.0.1:${server.address().port}`;
const browser=await chromium.launch({executablePath:process.env.CHROME_PATH||chromium.executablePath(),headless:true});
const errors=[];const at=p=>new URL(p.url()).pathname;
try{
 const context=await browser.newContext({viewport:{width:1280,height:900}});
 await context.route('**/*',r=>r.request().url().startsWith(origin)?r.continue():r.abort());
 const page=await context.newPage();page.on('pageerror',e=>errors.push(e.message));
 // 1. Signed-in homepage stays the homepage (direct + refresh).
 await page.goto(origin+'/');await page.getByRole('heading',{name:'You’re signed in'}).waitFor({timeout:30000});
 assert.equal(at(page),'/');await page.getByRole('link',{name:'Open your agents',exact:true}).waitFor();
 await page.screenshot({path:`${out}/home-signed-in.png`});
 await page.reload();await page.getByRole('heading',{name:'You’re signed in'}).waitFor();assert.equal(at(page),'/');
 await step('PASS home signed-in direct+refresh');
 // 2. Home -> /account via link, refresh, back.
 await page.getByRole('link',{name:'Manage connections',exact:true}).click();await page.waitForURL(origin+'/account');
 await page.getByRole('heading',{level:1,name:'Connections',exact:true}).waitFor({timeout:30000});
 await page.screenshot({path:`${out}/account.png`});
 await page.reload();await page.getByRole('heading',{level:1,name:'Connections',exact:true}).waitFor({timeout:30000});assert.equal(at(page),'/account');
 await page.goBack();await page.waitForURL(origin+'/');await page.getByRole('heading',{name:'You’re signed in'}).waitFor();
 await step('PASS /account via link, refresh, back to /');
 // 3. Home -> /agents via link, refresh, back.
 await page.getByRole('link',{name:'Open your agents',exact:true}).click();await page.waitForURL(origin+'/agents');
 await page.locator('.chat-workspace #agent-navigation').waitFor({timeout:30000});
 await page.screenshot({path:`${out}/agents.png`});
 await page.reload();await page.locator('.chat-workspace #agent-navigation').waitFor({timeout:30000});assert.equal(at(page),'/agents');
 await page.goBack();await page.waitForURL(origin+'/');await page.getByRole('heading',{name:'You’re signed in'}).waitFor();
 await page.goForward();await page.waitForURL(origin+'/agents');await page.locator('.chat-workspace #agent-navigation').waitFor({timeout:30000});
 await step('PASS /agents via link, refresh, back, forward');
 // 4. Direct entry of each canonical route.
 for(const [p,check] of [['/account',()=>page.getByRole('heading',{level:1,name:'Connections',exact:true})],['/account/vault',()=>page.getByRole('heading',{level:1,name:'Vault',exact:true})],['/agents',()=>page.locator('.chat-workspace #agent-navigation')]]){
  await page.goto(origin+p);await check().waitFor({timeout:30000});assert.equal(at(page),p);}
 await step('PASS direct /account, /account/vault, /agents');
 // 5. Legacy links land on canonical routes, preserving query; back does not bounce into a redirect loop.
 await page.goto(origin+'/');await page.getByRole('heading',{name:'You’re signed in'}).waitFor();
 await page.goto(origin+'/connect?connect=github');await page.waitForURL(origin+'/account?connect=github');await page.getByRole('heading',{level:1,name:'Connections',exact:true}).waitFor({timeout:30000});
 await page.goto(origin+'/agent');await page.waitForURL(origin+'/agents');await page.locator('.chat-workspace #agent-navigation').waitFor({timeout:30000});
 await page.goBack();await page.waitForURL(origin+'/account?connect=github');
 await page.goBack();await page.waitForURL(origin+'/');await page.getByRole('heading',{name:'You’re signed in'}).waitFor();
 await step('PASS legacy /connect, /agent redirects + back history');
 assert.deepEqual(errors,[],'page errors');
 await writeFile(`${out}/result.json`,JSON.stringify({documents,errors},null,2));
 await step('PASS routing journey');console.log('PASS routing journey');
}catch(e){await step('FAIL '+(e?.stack||e));throw e;}finally{await browser.close();await new Promise(r=>server.close(r));}
