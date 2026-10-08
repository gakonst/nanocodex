// Behavioral browser journey: owner creates/revokes a link, guest submits and streams a turn through a synthetic API without cookies.
// Repro: SIDEBAR_BROWSER_CHANNEL=chrome node js/account/scripts/thread-share-smoke.mjs
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdirSync, readdirSync } from 'node:fs';
import { createRequire } from 'node:module';
const require = createRequire(new URL('../package.json', import.meta.url));
const { build } = require('esbuild');
const packages = new URL('../../../node_modules/.pnpm/', import.meta.url);
const entry = readdirSync(packages).find(name => /^playwright-core@/.test(name));
const { chromium } = await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`, packages));
const token = `nsl_${'a'.repeat(43)}`;
const agentId = 'synthetic-agent';
const links = [];
const requests = [];
const events = [{cursor:'1',created_at:Date.now(),type:'turn_accepted',id:'initial',input:'What changed?'}, {cursor:'3',created_at:Date.now(),type:'turn_completed',id:'initial',final_message:'The release is ready.'}];
let revoked = false;
const streams=new Set();
const emit=(event)=>{if(event.type!=='assistant_delta')events.push(event);const frame=`id: ${event.cursor}\nevent: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`;for(const stream of streams)stream.write(frame);};
const turns = []; let ambiguousWrite=true; const writeIds=[];
const server = createServer(async(req,res) => {
  requests.push({url:req.url, authorization:req.headers.authorization, cookie:req.headers.cookie});
  res.setHeader('Content-Type','application/json');
  const body = await new Promise(resolve => {let b='';req.on('data',x=>b+=x);req.on('end',()=>resolve(b));});
  const json = body ? JSON.parse(body) : {};
  const owner = `/v1/agents/${agentId}/share-links`;
  const shared = `/v1/shared/${agentId}`;
  if (req.url===owner && req.method==='GET') return res.end(JSON.stringify({data:links}));
  if (req.url===owner && req.method==='POST') {
    const item={id:'link-1',permission:json.permission,created_at:Date.now()};
    links.push(item); return res.end(JSON.stringify({...item,url:`http://127.0.0.1:${server.address().port}/share/${agentId}#token=${token}`}));
  }
  if (req.url===`${owner}/link-1` && req.method==='DELETE') {revoked=true;links.splice(0);for(const stream of streams)stream.end();streams.clear();res.statusCode=204;return res.end();}
  if (req.url?.startsWith(shared)) {
    if (req.headers.authorization!==`Bearer ${token}` || revoked) {res.statusCode=403;return res.end(JSON.stringify({error:'invalid_share_link'}));}
    if (req.url?.startsWith(`${shared}/events?`)) {
      res.setHeader('Content-Type','text/event-stream');res.setHeader('Cache-Control','no-store');
      const cursor=BigInt(new URL(req.url,'http://localhost').searchParams.get('after')??'0');
      res.write('retry: 1000\n\n');streams.add(res);req.on('close',()=>streams.delete(res));
      for(const event of events)if(BigInt(event.cursor)>cursor)res.write(`id: ${event.cursor}\nevent: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`);
      return;
    }
    if (req.url===shared) return res.end(JSON.stringify({agent_id:agentId,permission:links[0]?.permission??'read',title:'Project handoff',latest_event_cursor:'2'}));
    if (req.url.startsWith(`${shared}/events/history`)) {
      const before=new URL(req.url,'http://localhost').searchParams.get('before');
      const page=before==='3' ? {data:[],has_more:true,next_cursor:'2'} : before==='2' ? {data:[events[0]],has_more:false,next_cursor:null} : {data:events.slice(1),has_more:true,next_cursor:'3'};
      return res.end(JSON.stringify({...page,latest_cursor:'3'}));
    }
    if (req.url===`${shared}/turns` && req.method==='POST' && links[0]?.permission==='write') {
      writeIds.push(json.id);
      let turn=turns.find(t=>t.id===json.id);
      if (!turn) {
        turn={id:json.id,input:json.input};turns.push(turn);
        emit({cursor:String(7+turns.length*4),type:'turn_accepted',id:turn.id,turn_id:turn.id,input:turn.input,author:'guest'});
        emit({cursor:String(8+turns.length*4),type:'event',turn_id:turn.id,event:{request_id:'synthetic',seq:8+turns.length*4,type:'reasoning.summary.delta',payload:{turn_id:turn.id,text:'Checking the answer…'}}});
        emit({cursor:String(9+turns.length*4),type:'event',turn_id:turn.id,event:{request_id:'synthetic',seq:9+turns.length*4,type:'tool.call',payload:{turn_id:turn.id,tool:'search',call_id:'tool-1',arguments:{query:'status'}}}});
        emit({cursor:String(10+turns.length*4),type:'event',turn_id:turn.id,event:{request_id:'synthetic',seq:10+turns.length*4,type:'tool.result',payload:{turn_id:turn.id,call_id:'tool-1',result:{text:'complete'}}}});
        setTimeout(()=>{emit({cursor:String(11+turns.length*4),type:'event',turn_id:turn.id,event:{request_id:'synthetic',seq:11+turns.length*4,type:'assistant.delta',payload:{turn_id:turn.id,phase:'final_answer',text:'Follow-up '}}});emit({cursor:String(12+turns.length*4),type:'turn_completed',id:turn.id,turn_id:turn.id,final_message:'Follow-up complete.'});},180);
      }
      if (ambiguousWrite) {ambiguousWrite=false;res.statusCode=503;return res.end(JSON.stringify({error:'unknown_outcome'}));}
      res.statusCode=202;return res.end(JSON.stringify(turn));
    }
  }
  res.statusCode=404;res.end(JSON.stringify({error:'not_found'}));
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const bundle = await build({ stdin:{contents:`
import React from 'react'; import {createRoot} from 'react-dom/client';
import {ThreadShareDialog} from './src/ThreadShareDialog'; import {SharedThreadView} from './src/SharedThreadView';
import './src/ThreadSharing.css';
createRoot(document.getElementById('root')).render(location.pathname.startsWith('/share/')
 ? <SharedThreadView agentId="synthetic-agent" /> : <ThreadShareDialog agentId="synthetic-agent" onClose={()=>{}} />);
`,resolveDir:new URL('..',import.meta.url).pathname,loader:'tsx'},bundle:true,write:false,outfile:'app.js',jsx:'automatic',plugins:[{name:'single-react-for-browser-fixture',setup(build){build.onResolve({filter:/^react(?:\/jsx(?:-dev)?-runtime)?$/},args=>({path:require.resolve(args.path)}));}},{name:'disabled-voice-for-browser-fixture',setup(build){
  build.onResolve({filter:/^nanocodex-react$/},()=>({path:'disabled-voice',namespace:'fixture'}));
  build.onLoad({filter:/.*/,namespace:'fixture'},()=>({contents:`export const Voice={defaultVoice:'alloy',voices:['alloy']};
    export const createElevenLabsManager=()=>({});
    export const useVoice=()=>({transcripts:[],isActive:false,isConnecting:false,voice:undefined,status:'idle',statusText:'',start:async()=>{},stop:async()=>{},speak:async()=>{},error:undefined});`,loader:'js'}));
}}] });
const html=`<meta name="viewport" content="width=device-width,initial-scale=1"><div id="root"></div><style>html,body,#root{min-height:100%;margin:0} ${bundle.outputFiles.find(f=>f.path.endsWith('.css')).text}</style><script>${bundle.outputFiles.find(f=>f.path.endsWith('.js')).text}</script>`;
server.on('request',(req,res)=>{});
// Wrap the API server's request listener with an HTML response for SPA routes.
const api = server.listeners('request')[0]; server.removeListener('request',api);
server.on('request',(req,res)=>req.url==='/' || req.url?.startsWith('/share/') && !req.url.includes('/v1/') ? (res.setHeader('Content-Type','text/html'),res.end(html)) : api(req,res));
const browser = await chromium.launch({headless:true, ...(process.env.SIDEBAR_BROWSER_CHANNEL ? {channel:process.env.SIDEBAR_BROWSER_CHANNEL} : {})});
const output = new URL('../../../output/thread-share/',import.meta.url);mkdirSync(output,{recursive:true});
try {
 const origin=`http://127.0.0.1:${server.address().port}`;
 const owner=await browser.newPage();await owner.goto(origin);
 await owner.getByRole('button',{name:'Create view link'}).click();
 const link=await owner.getByRole('textbox',{name:'New share link'}).inputValue();
 assert.equal(link,`${origin}/share/${agentId}#token=${token}`);
 await owner.getByRole('button',{name:'Revoke link'}).waitFor();
 await owner.screenshot({path:new URL('owner.png',output).pathname});
 const visitor=await browser.newPage();await visitor.goto(link);
 await visitor.getByText('The release is ready.').waitFor();
 emit({cursor:'4',type:'turn_accepted',id:'live-turn',turn_id:'live-turn',input:'Any updates?'});
 const peer=await browser.newPage();await peer.goto(link);
 await peer.getByText('The release is ready.').waitFor();
 emit({cursor:'5',type:'event',turn_id:'live-turn',event:{type:'reasoning.summary.delta',payload:{text:'Checking shared progress…'}}});
 for (const page of [visitor,peer]) await page.getByText('Checking shared progress…').waitFor();
 emit({cursor:'6',type:'event',turn_id:'live-turn',event:{type:'tool.call',payload:{tool:'search',call_id:'live-search',arguments:{query:'progress'}}}});
 emit({cursor:'7',type:'event',turn_id:'live-turn',event:{type:'tool.result',payload:{call_id:'live-search',result:{text:'progress found'}}}});
 emit({cursor:'8',type:'event',turn_id:'live-turn',event:{type:'assistant.delta',payload:{phase:'final_answer',text:'Working on it…'}}});
 for (const page of [visitor,peer]) await page.getByText('Working on it…').waitFor();
 await Promise.all([visitor.waitForResponse(response=>response.url().endsWith('/events/history')),
   visitor.getByRole('button',{name:'Refresh shared thread'}).click()]);
 await visitor.getByText('Working on it…').waitFor();
 await visitor.screenshot({path:new URL('streaming.png',output).pathname});
 emit({cursor:'9',type:'turn_completed',id:'live-turn',turn_id:'live-turn',final_message:'All updates complete.'});
 for (const page of [visitor,peer]) await page.getByText('All updates complete.').waitFor();
 await peer.close();
 assert.equal(await visitor.getByText('Working on it…').count(),0,'final answer replaces its streamed delta');
 await visitor.getByRole('button',{name:'Load earlier messages'}).click();
 await visitor.getByRole('button',{name:'Load earlier messages'}).click();
 await visitor.getByText('What changed?').waitFor();
 assert.equal(await visitor.getByRole('textbox',{name:'Comment on this thread'}).count(),0);
 await visitor.screenshot({path:new URL('read.png',output).pathname});
 await visitor.reload(); await visitor.getByText('The release is ready.').waitFor();
 // A slow authorized history response must never resurrect the transcript after
 // an active stream is closed by revocation.
 let resolveBlocked; const blocked=new Promise(resolve=>{resolveBlocked=resolve;});
 let historyStarted; const started=new Promise(resolve=>{historyStarted=resolve;});
 await visitor.route('**/events/history',async route=>{
   historyStarted(); await blocked;
   await route.fulfill({status:200,contentType:'application/json',body:JSON.stringify({data:[events[1]],has_more:false})});
 });
 await visitor.getByRole('button',{name:'Refresh shared thread'}).click(); await started;
 await owner.getByRole('button',{name:'Revoke link'}).click();assert.equal(revoked,true);
 await visitor.getByRole('alert').waitFor(); resolveBlocked();
 await visitor.waitForTimeout(150);
 assert.equal(await visitor.getByText('The release is ready.').count(),0,'stale history cannot repopulate a revoked transcript');
 await visitor.reload();await visitor.getByRole('alert').waitFor();
 // A new write link submits through the guest chat surface to the fixture API.
 revoked=false;links.push({id:'link-2',permission:'write',created_at:new Date().toISOString()});
 const writer=await browser.newPage();await writer.goto(link);
 await writer.getByRole('textbox',{name:'Message Nanocodex'}).fill('Can you follow up?');
 await writer.getByRole('button',{name:'Send message'}).click();
 await writer.locator('.agent-terminal-user').filter({hasText:'Can you follow up?'}).waitFor();
 await writer.getByRole('alert').getByText(/Couldn’t confirm your message/).waitFor();
 await writer.getByRole('button',{name:'Send message'}).click();
 await writer.getByText('Checking the answer…').waitFor();
 await writer.getByText('Follow-up complete.').waitFor();
 assert.deepEqual(writeIds,[writeIds[0],writeIds[0]],'retry must retain the same turn ID');
 assert.equal(turns.length,1,'uncertain outcome must not duplicate a turn');
 assert.equal(await writer.locator('.agent-terminal-user').filter({hasText:'Can you follow up?'}).count(),1,'optimistic prompt reconciles with accepted event');
 assert.equal(await writer.getByText('Guest').count(),1,'shared turn is attributed to transferable Guest');
 links[0].permission='read';
 const reader=await browser.newPage();await reader.goto(link);
 await reader.getByText('Can you follow up?').waitFor();
 assert.equal(await reader.getByRole('textbox',{name:'Message Nanocodex'}).count(),0,'read link has no composer');
 assert.ok(requests.filter(r=>r.url.startsWith('/v1/shared/')).every(r=>r.authorization===`Bearer ${token}` && !r.cookie && !r.url.includes(token)));
 assert.equal(requests.some(r=>r.url?.includes('/comments')),false,'no comment endpoints');
 await writer.screenshot({path:new URL('write.png',output).pathname});
 const mobile=await browser.newPage({viewport:{width:390,height:844},isMobile:true,hasTouch:true});
 await mobile.goto(link);
 await mobile.getByText('The release is ready.').waitFor();
 assert.ok(await mobile.evaluate(()=>document.documentElement.scrollWidth <= window.innerWidth));
 await mobile.screenshot({path:new URL('mobile.png',output).pathname});
 await mobile.close();
 console.log('Share-link journey passed; screenshots: output/thread-share/{owner,read,write,mobile,streaming}.png');
 await owner.close();await visitor.close();await writer.close();await reader.close();
} finally {await browser.close();server.close();}
