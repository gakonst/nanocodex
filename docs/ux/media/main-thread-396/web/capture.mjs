import { chromium } from '../../../../../node_modules/.pnpm/playwright@1.62.1/node_modules/playwright/index.mjs';
import fs from 'node:fs/promises';
import os from 'node:os';
const out = new URL('./',import.meta.url).pathname;
const browser=await chromium.launch({headless:true,executablePath:process.env.CHROMIUM_PATH || os.homedir()+'/Library/Caches/ms-playwright/chromium-1208/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing'});
const context=await browser.newContext({viewport:{width:1440,height:1000},recordVideo:{dir:out+'video',size:{width:1440,height:1000}}});
const page=await context.newPage(); const requests=[];const errors=[];
page.on('pageerror',e=>errors.push(String(e)));page.on('console',m=>{if(m.type()==='error') errors.push(m.text())});
const main='11111111-1111-4111-8111-111111111111',project='22222222-2222-4222-8222-222222222222';let opened=false;
await context.route('**/*',async route=>{const u=new URL(route.request().url()),p=u.pathname;
 if(u.origin!=='http://127.0.0.1:5197') return route.abort();
 if(!p.startsWith('/v1/')&&!p.startsWith('/api/')) return route.continue();
 requests.push({method:route.request().method(),path:p});let body={};let status=200;
 if(p==='/v1/me')body={user:{id:'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',persistent:true}};
 else if(p==='/api/health')body={agent_configured:true,credential_source:'brokered',deployment_sha:'f517a439',voice_enabled:false};
 else if(p==='/v1/main-thread'){if(route.request().method()==='PUT')opened=true;body={agent_id:opened?main:null};}
 else if(p==='/v1/projects')body={data:[{id:'demo-website',name:'Demo Website',coordinator_agent_id:project},{id:'demo-research',name:'Research Notes',coordinator_agent_id:null}]};
 else if(p==='/v1/agents')body={data:[main,project],summaries:{}};
 else if(p.endsWith('/events/history')) {
   const isMain=p.includes(main), turn='fixture-review';
   body={data:[
    {type:'turn_accepted',cursor:'1',created_at:1789680000000,turn_id:turn,id:turn,input:isMain?'Ask Demo Website to review its release checklist.':'Review the release checklist.'},
    {type:'turn_completed',cursor:'2',created_at:1789680001000,turn_id:turn,id:turn,final_message:isMain?'Fixture result: Demo Website reported its release checklist is ready. This seeded transcript demonstrates result presentation only; no agent delegation or model run occurred.':'Synthetic project coordinator transcript: the release checklist is ready for review. No live task was executed.',usage:null,citations:[]}
   ],has_more:false,latest_cursor:'2'};
 }
 else if(p.endsWith('/phone/calls'))body={calls:[]};
 else if(p==='/v1/account/hands/screens')body={surfaces:[]};
 else if(p.endsWith('/prepare'))body={};
 else if(p.endsWith('/events'))return route.fulfill({contentType:'text/event-stream',body:': fixture idle\n\n'});
 else if(/^\/v1\/agents\/[^/]+$/.test(p))body={agent_id:p.split('/').pop(),status:'idle',title:p.includes(main)?'Main Thread':'Demo Website',turns:[],settings:{model:'gpt-6-astra',thinking:'low',reasoningMode:'standard',fastMode:false}};
 else {status=404;body={error:'fixture_not_provided'};}
 return route.fulfill({status,contentType:'application/json',body:JSON.stringify(body)});
});
await page.goto('http://127.0.0.1:5197/agent');await page.waitForTimeout(5000);
console.log((await page.locator('body').innerText()).slice(0,5000));console.log(errors);
await page.screenshot({path:out+'01-entry.png',fullPage:true});
await page.getByRole('button',{name:'Main Thread',exact:true}).click();await page.waitForTimeout(2500);await page.screenshot({path:out+'02-main-thread.png',fullPage:true});
await page.getByRole('button',{name:'Demo Website',exact:true}).click();await page.waitForTimeout(2000);await page.screenshot({path:out+'03-project.png',fullPage:true});
await page.getByRole('button',{name:'Main Thread',exact:true}).click();await page.waitForTimeout(2000);await page.screenshot({path:out+'04-reuse.png',fullPage:true});
await fs.writeFile(out+'capture-log.json',JSON.stringify({requests,errors,finalUrl:page.url()},null,2));const video=page.video();await context.close();await video.saveAs(out+'navigation.webm');await browser.close();
