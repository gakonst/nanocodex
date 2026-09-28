// Behavioral browser journey: owner creates/revokes a link, guest reads and contributes without cookies.
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
const comments = []; let ambiguousWrite=true; let staleRead=false; const writeIds=[];
const server = createServer(async(req,res) => {
  requests.push({url:req.url, authorization:req.headers.authorization, cookie:req.headers.cookie});
  res.setHeader('Content-Type','application/json');
  const body = await new Promise(resolve => {let b='';req.on('data',x=>b+=x);req.on('end',()=>resolve(b));});
  const json = body ? JSON.parse(body) : {};
  const owner = `/v1/agents/${agentId}/share-links`;
  const shared = `/v1/shared/${agentId}`;
  if (req.url===`/v1/agents/${agentId}/share-comments` && req.method==='GET') return res.end(JSON.stringify({data:comments}));
  if (req.url===owner && req.method==='GET') return res.end(JSON.stringify({data:links}));
  if (req.url===owner && req.method==='POST') {
    const item={id:'link-1',permission:json.permission,created_at:Date.now()};
    links.push(item); return res.end(JSON.stringify({...item,url:`http://127.0.0.1:${server.address().port}/share/${agentId}#token=${token}`}));
  }
  if (req.url===`${owner}/link-1` && req.method==='DELETE') {revoked=true;links.splice(0);res.statusCode=204;return res.end();}
  if (req.url?.startsWith(shared)) {
    if (req.headers.authorization!==`Bearer ${token}` || revoked) {res.statusCode=403;return res.end(JSON.stringify({error:'invalid_share_link'}));}
    if (req.url===shared) return res.end(JSON.stringify({agent_id:agentId,permission:links[0]?.permission??'read',title:'Project handoff',latest_event_cursor:'2'}));
    if (req.url.startsWith(`${shared}/events/history`)) {
      const before=new URL(req.url,'http://localhost').searchParams.get('before');
      const page=before==='3' ? {data:[],has_more:true,next_cursor:'2'} : before==='2' ? {data:[events[0]],has_more:false,next_cursor:null} : {data:[events[1]],has_more:true,next_cursor:'3'};
      return res.end(JSON.stringify({...page,latest_cursor:'3'}));
    }
    if (req.url===`${shared}/comments` && req.method==='GET') {const data=staleRead ? [] : comments;staleRead=false;return res.end(JSON.stringify({data}));}
    if (req.url===`${shared}/comments` && req.method==='POST' && links[0]?.permission==='write') {writeIds.push(json.id);let comment=comments.find(c=>c.id===json.id);if (comment) return res.end(JSON.stringify(comment));comment={id:json.id,input:json.input,created_at:Date.now(),author:'guest'};comments.push(comment);if (ambiguousWrite){ambiguousWrite=false;staleRead=true;res.statusCode=503;return res.end(JSON.stringify({error:'unknown_outcome'}));}res.statusCode=201;return res.end(JSON.stringify(comment));}
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
`,resolveDir:new URL('..',import.meta.url).pathname,loader:'tsx'},bundle:true,write:false,outfile:'app.js',jsx:'automatic' });
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
 await visitor.getByRole('button',{name:'Load earlier messages'}).click();
 await visitor.getByRole('button',{name:'Load earlier messages'}).click();
 await visitor.getByText('What changed?').waitFor();
 assert.equal(await visitor.getByRole('textbox',{name:'Comment on this thread'}).count(),0);
 await visitor.screenshot({path:new URL('read.png',output).pathname});
 await visitor.reload(); await visitor.getByText('The release is ready.').waitFor();
 await owner.getByRole('button',{name:'Revoke link'}).click();assert.equal(revoked,true);
 await visitor.reload();await visitor.getByRole('alert').waitFor();
 // A new link with write access exercises contribution using the same guest UI and bearer boundary.
 revoked=false;links.push({id:'link-2',permission:'write',created_at:new Date().toISOString()});
 const writer=await browser.newPage();await writer.goto(link);
 await writer.getByRole('textbox',{name:'Comment on this thread'}).fill('Can you follow up?');
 await writer.getByRole('button',{name:'Post comment'}).click();
 await writer.getByRole('alert').getByText(/Couldn’t confirm your comment/).waitFor();
 await writer.getByRole('button',{name:'Post comment'}).click();
 await writer.getByText('Can you follow up?').waitFor();
 assert.deepEqual(writeIds,[writeIds[0],writeIds[0]],'retry must retain the same comment ID');
 assert.equal(comments.length,1,'uncertain outcome must not duplicate a comment');
 await owner.getByRole('button',{name:'Refresh'}).click();
 await owner.getByText('Can you follow up?').waitFor();
 assert.equal(requests.some(r=>r.url.includes('/turns')),false,'sharing must never start an owner AI turn');
 assert.ok(requests.filter(r=>r.url.startsWith('/v1/shared/')).every(r=>r.authorization===`Bearer ${token}` && !r.cookie && !r.url.includes(token)));
 await writer.screenshot({path:new URL('write.png',output).pathname});
 const mobile=await browser.newPage({viewport:{width:390,height:844},isMobile:true,hasTouch:true});
 await mobile.goto(link);
 await mobile.getByText('The release is ready.').waitFor();
 assert.ok(await mobile.evaluate(()=>document.documentElement.scrollWidth <= window.innerWidth));
 await mobile.screenshot({path:new URL('mobile.png',output).pathname});
 await mobile.close();
 console.log('Share-link journey passed; screenshots: output/thread-share/{owner,read,write,mobile}.png');
 await owner.close();await visitor.close();await writer.close();
} finally {await browser.close();server.close();}
