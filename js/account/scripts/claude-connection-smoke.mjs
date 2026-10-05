// Actual React account variants and model picker over a synthetic local account HTTP boundary.
// No provider authorization page is loaded and no live account or secret is used.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { build } from 'esbuild';
import { chromium } from 'playwright-core';
const output = process.env.NANOCODEX_CLAUDE_UI_EVIDENCE_DIR
 ? pathToFileURL(resolve(process.env.NANOCODEX_CLAUDE_UI_EVIDENCE_DIR) + '/')
 : new URL('../../../output/claude-connection/', import.meta.url);
mkdirSync(output, {recursive:true});
console.log("Bundling real account components");
const bundle = await build({ loader: { ".png": "dataurl" }, stdin: { contents: `
import React from 'react'; import {createRoot} from 'react-dom/client'; import {MemoryRouter} from 'react-router';
import {QueryClientProvider} from '@tanstack/react-query'; import {appQueryClient} from './src/queryClient';
import {AccountSessionProvider} from './src/AccountSession'; import {AccountMenu} from './src/AccountMenu';
import {AgentModelMenu} from './src/AgentModelMenu'; import {createManagedConversation} from './src/managedAgentRuntime';
import './src/index.css'; import 'nanocodex-connect-ui/styles.css'; import './src/DeviceConnect.css'; import './src/Home.css'; import './src/AgentTerminal.css';
function Journey(){const [settings,setSettings]=React.useState({model:'gpt-6-astra',thinking:'max',reasoningMode:'pro',fastMode:true});
 const [receipt,setReceipt]=React.useState(''); const [locked,setLocked]=React.useState(false);
 const update=(model,normalized)=>{setSettings({model,...normalized});return Promise.resolve()};
 return <><AccountMenu inline={location.search.includes('inline')}/>
 <section style={{position:'fixed',bottom:0,left:0,right:0,zIndex:10,background:'var(--surface)',padding:12}}>
 <AgentModelMenu agentReady={true} modelLocked={locked} settings={settings} onModel={update} onThinking={thinking=>{setSettings({...settings,thinking});return Promise.resolve()}} onFastMode={fastMode=>{setSettings({...settings,fastMode});return Promise.resolve()}}/>
 <button onClick={()=>setLocked(true)}>Mark first turn accepted</button>
 <output aria-label="selected-settings">{JSON.stringify(settings)}</output>
 <button onClick={()=>{createManagedConversation('018f0000-0000-4000-8000-000000000001').then(agent=>setReceipt(agent.id)).catch(error=>setReceipt(error.message))}}>Create managed chat</button><output aria-label="create-receipt">{receipt}</output></section></>}
createRoot(document.getElementById('root')).render(<MemoryRouter><QueryClientProvider client={appQueryClient}><AccountSessionProvider><Journey/></AccountSessionProvider></QueryClientProvider></MemoryRouter>);
`, resolveDir:new URL('..',import.meta.url).pathname, loader:'tsx'}, tsconfigRaw:{compilerOptions:{jsx:'react-jsx'}}, bundle:true, write:false, outfile:'app.js', jsx:'automatic', external:['/paradigm-mark.svg'] });
const model = {id:'claude-sonnet-4-6',name:'Claude Sonnet 4.6',provider:'claude',thinking:['low','medium','high'],fast_mode:false,reasoning_modes:['standard']};
let connected=false, started=false, failure=false, catalogFailure=false, claudeUnavailable=false;
const trace=[];
const server=createServer(async(req,res)=>{
 const url=new URL(req.url,'http://fixture.test'); let body='';for await(const chunk of req)body+=chunk;
 const json=(value,status=200)=>{res.writeHead(status,{'content-type':'application/json','cache-control':'no-store'});res.end(JSON.stringify(value))};
 if(url.pathname.startsWith('/v1')||url.pathname==='/api/health') trace.push({method:req.method,path:url.pathname,privateBodyPresent:Boolean(body),originPresent:Boolean(req.headers.origin)});
 if(url.pathname==='/v1/me')return json({user:{id:'018f0000-0000-4000-8000-000000000001',persistent:true}});
 if(url.pathname==='/v1/credentials')return json({ready:connected,active:null,openai:{connected:false},chatgpt:{connected:false,accounts:[]},claude:{connected,state:started?'pending':connected?'authenticated':'signed_out'}});
 if(url.pathname==='/v1/credentials/claude/login'&&req.method==='POST'){
  assert.equal(req.headers.origin,`http://127.0.0.1:${server.address().port}`);started=true;
  return json({authorization_url:'https://claude.com/cai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&scope=org%3Acreate_api_key+user%3Aprofile+user%3Ainference+user%3Asessions%3Aclaude_code+user%3Amcp_servers+user%3Afile_upload+user%3Aplugins&response_type=code&redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback&state=sssssssssssssssssssssssssssssssssssssssssss&code_challenge=ccccccccccccccccccccccccccccccccccccccccccc&code_challenge_method=S256',expires_at:Date.now()+600000});
 }
 if(url.pathname==='/v1/credentials/claude/login/complete'){
  assert.equal(req.method,'POST'); assert.equal(req.headers.origin,`http://127.0.0.1:${server.address().port}`);
  assert.deepEqual(Object.keys(JSON.parse(body)),['code']);assert.equal(JSON.parse(body).code,'synthetic-code#sssssssssssssssssssssssssssssssssssssssssss');
  if(failure)return json({error:'private-provider-diagnostic-must-not-render'},409);
  connected=true;started=false;return json({state:'authenticated'});
 }
 if(url.pathname==='/v1/credentials/claude'&&req.method==='DELETE'){connected=false;started=false;return json({connected:false,state:'signed_out'})}
 if(url.pathname==='/v1/models')return catalogFailure?json({error:'unavailable'},503):json({object:'list',data:connected&&!claudeUnavailable?[model]:[],default_model:connected&&!claudeUnavailable?model.id:null,partial:claudeUnavailable,availability:{claude:{connected,available:connected&&!claudeUnavailable,...(claudeUnavailable?{error:"claude_models_unavailable"}:{})}}});
 if(url.pathname==='/v1/agents'&&req.method==='POST'){
  const settings=JSON.parse(body).settings;assert.equal(settings.model,model.id);assert.equal(settings.thinking,'low');assert.equal(settings.reasoning_mode,'standard');assert.equal(settings.fast_mode,false);
  return json({agent_id:'018f0000-0000-7000-8000-000000000002'},201);
 }
 if(url.pathname==='/v1/api-keys')return json({data:[]});
 if(url.pathname==='/api/health')return json({agent_configured:connected,credential_source:connected?'brokered':null});
 if(url.pathname.startsWith('/v1/'))return json({error:'fixture_unavailable'},404);
 res.setHeader('content-type','text/html');res.end(`<meta name="viewport" content="width=device-width,initial-scale=1"><div id="root"></div><style>${bundle.outputFiles.find(f=>f.path.endsWith('.css'))?.text??''}</style><script>${bundle.outputFiles.find(f=>f.path.endsWith('.js')).text}</script>`);
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
console.log("Starting isolated browser");
const browser=await chromium.launch({headless:true,...(process.env.CLAUDE_BROWSER_CHANNEL?{channel:process.env.CLAUDE_BROWSER_CHANNEL}:{})});
try{
 for(const inline of [true,false]){
  connected=false;started=false;failure=false;catalogFailure=false;
  const context=await browser.newContext({viewport:{width:inline?390:1200,height:1000}});const page=await context.newPage();page.setDefaultTimeout(15000);const errors=[];
  page.on('pageerror',error=>{errors.push(error.message);console.error('pageerror:',error.message)});
  await context.route('https://claude.com/**',route=>route.fulfill({status:200,contentType:'text/html',body:'Synthetic provider approval page. No OAuth performed.'}));
  console.log("Opening", inline?"inline":"popover");
  await page.goto(`http://127.0.0.1:${server.address().port}/${inline?'?inline':''}`);
  if(!inline)await page.locator('.account-menu-trigger').click();
  console.log("Account opened");
  const card=page.locator('#claude-connection');await card.getByRole('button',{name:/Claude.*Connect/}).waitFor();
  console.log("Starting Claude popup");
  const popupPromise=page.waitForEvent('popup');await card.getByRole('button',{name:/Claude.*Connect/}).click();const popup=await popupPromise;
  console.log("Popup received");
  await popup.waitForURL('https://claude.com/**');assert.equal(await popup.evaluate(()=>window.opener===null),true);
  await card.getByLabel('Claude authorization code (code#state)').fill('synthetic-code#sssssssssssssssssssssssssssssssssssssssssss');
  await card.getByRole('button',{name:'Complete Claude sign-in'}).click();await card.getByRole('button',{name:/Claude.*Disconnect/}).waitFor();
  assert.equal(await page.locator('#claude-private-code').count(),0);assert.equal((await page.locator('body').textContent()).includes('synthetic-code'),false);
  console.log("Claude connected, opening model menu");
  await page.getByRole('button',{name:/Model settings:/}).click();await page.getByText('Choose a model',{exact:true}).hover();
  await page.getByRole('menuitemradio',{name:'Claude Sonnet 4.6',exact:true}).click();
  assert.deepEqual(JSON.parse(await page.getByLabel('selected-settings').textContent()),{model:model.id,thinking:'low',reasoningMode:'standard',fastMode:false});
  await page.getByRole('button',{name:'Create managed chat'}).click();await page.getByLabel('create-receipt').filter({hasText:'018f0000-0000-7000-8000-000000000002'}).waitFor();
  if(!inline && await card.count()===0) await page.locator('.account-menu-trigger').click();
  const layout=await card.evaluate(element=>{
   const button=element.querySelector('.connection-card'),copy=element.querySelector('.connection-card-copy');
   const title=copy.querySelector('strong').getBoundingClientRect(),detail=copy.querySelector('span').getBoundingClientRect(),action=element.querySelector('.connection-card-action').getBoundingClientRect();
   return {buttonDisplay:getComputedStyle(button).display,copyDisplay:getComputedStyle(copy).display,separated:detail.top>=title.bottom&&action.top>=detail.bottom,noOverflow:element.scrollWidth<=element.clientWidth};
  });
  assert.deepEqual(layout,{buttonDisplay:'grid',copyDisplay:'grid',separated:true,noOverflow:true});
  await card.screenshot({path:new URL(inline?'inline-connected.png':'popover-connected.png',output).pathname});
  if(!inline) await page.getByRole('button',{name:'Close account panel'}).click();
  await page.getByRole('button',{name:'Mark first turn accepted'}).click();
  await page.getByRole('button',{name:/Model settings:/}).click();
  await page.getByText('Thinking',{exact:true}).hover();
  await page.getByText('Thinking fixed for this Claude conversation',{exact:true}).waitFor();
  for(const effort of ['Low','Medium','High'])assert.equal(await page.getByRole('menuitemradio',{name:effort,exact:true}).getAttribute('aria-disabled'),'true');
  await page.keyboard.press('Escape');await page.keyboard.press('Escape');
  claudeUnavailable=true;
  await page.getByRole('button',{name:/Model settings:/}).click();
  await page.getByText('Couldn’t load Claude models. Reopen to retry.',{exact:true}).waitFor();
  assert.equal(await page.getByRole('menuitem',{name:'Manage Claude connection',exact:true}).getAttribute('href'),'/connect#claude-connection');
  await page.keyboard.press('Escape');
  claudeUnavailable=false;
  catalogFailure=true;
  await page.getByRole('button',{name:/Model settings:/}).click();
  await page.locator('.agent-model-error').filter({hasText:'Couldn’t load available models'}).waitFor();
  await page.getByText('Start a new chat to change models',{exact:true}).hover();
  assert.equal(await page.getByRole('menuitemradio',{name:'Claude Sonnet 4.6',exact:true}).count(),0);
  await page.keyboard.press('Escape');await page.keyboard.press('Escape');
  await page.getByRole('button',{name:'Create managed chat'}).click();
  await page.getByLabel('create-receipt').filter({hasText:'Couldn’t check available models'}).waitFor();
  catalogFailure=false;
  if(!inline && await card.count()===0) await page.locator('.account-menu-trigger').click();
  await card.getByRole('button',{name:/Claude.*Disconnect/}).click();await card.getByRole('button',{name:/Claude.*Connect/}).waitFor();
  if(!inline && await card.count()!==0) await page.getByRole('button',{name:'Close account panel'}).click();
  await page.getByRole('button',{name:/Model settings:/}).click();
  assert.equal(await page.getByRole('menuitem',{name:'Connect Claude',exact:true}).getAttribute('href'),'/connect#claude-connection');
  await page.keyboard.press('Escape');
  console.log('Disconnected, checking create denial');
  await page.getByRole('button',{name:'Create managed chat'}).click();await page.getByLabel('create-receipt').filter({hasText:'Connect a model subscription'}).waitFor();
  if(!inline && await card.count()===0) await page.locator('.account-menu-trigger').click();
  failure=true;const nextPopup=page.waitForEvent('popup');await card.getByRole('button',{name:/Claude.*Connect/}).click();await nextPopup;
  await card.getByLabel('Claude authorization code (code#state)').fill('synthetic-code#sssssssssssssssssssssssssssssssssssssssssss');await card.getByRole('button',{name:'Complete Claude sign-in'}).click();
  await card.getByRole('alert').waitFor();assert.equal(await card.getByLabel('Claude authorization code (code#state)').inputValue(),'');assert.equal((await page.locator('body').textContent()).includes('private-provider-diagnostic'),false);
  await card.screenshot({path:new URL(inline?'inline-failed.png':'popover-failed.png',output).pathname});assert.deepEqual(errors,[]);
  await context.close();console.log(`${inline?'inline/mobile':'popover/desktop'}: connect via isolated popup, private completion, Claude selection normalized, Claude-only create, catalog failure hides stale choices/denies create, disconnect denies create, private error cleared, accepted Claude effort pinned: PASS`);
 }
 writeFileSync(new URL('browser-api-trace.json',output),JSON.stringify(trace,null,2));
}catch(error){console.error(error);writeFileSync(new URL('browser-api-trace.json',output),JSON.stringify(trace,null,2));throw error;}finally{await browser.close();server.closeAllConnections();await new Promise(resolve=>server.close(resolve));}
