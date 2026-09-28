// Behavioral browser journey for managed thread discovery. Synthetic identities only.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdirSync, readdirSync } from 'node:fs';
import { createRequire } from 'node:module';
const require = createRequire(new URL('../package.json', import.meta.url));
const { build } = require('esbuild');
const packages = new URL('../../../node_modules/.pnpm/', import.meta.url);
const entry = readdirSync(packages).find(name => /^playwright-core@/.test(name));
assert.ok(entry, 'Install workspace dependencies including playwright-core');
const { chromium } = await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`, packages));
const bundle = await build({ stdin: { contents: `
import React, {useState,useCallback} from 'react'; import {createRoot} from 'react-dom/client'; import {MemoryRouter} from 'react-router';
import {AgentSidebar} from './src/AgentSidebar'; import './src/Home.css'; import './src/AgentTerminal.css';
const now = Date.now();
const conversations = [
  {id:'one',title:'Shipping notes',lastUserMessageAt:now-60000,presentation:{status:'running',lastUserPrompt:'Investigate rollout',activeTurnIds:[]}},
  {id:'two',title:'Budget review',lastUserMessageAt:now-12*86400000,presentation:{status:'completed',lastUserPrompt:'Compare numbers',activeTurnIds:[]}},
  {id:'three',title:'Conversation abcdef12',lastUserMessageAt:now-120*86400000,presentation:{status:'failed',activeTurnIds:[]}}
];
function App(){ const [open,setOpen]=useState(false); const close=useCallback(()=>setOpen(false),[]); return <MemoryRouter><div className="nanocodex-demo chat-workspace"><div className="conversation-workspace"><button className="mobile-open" onClick={()=>setOpen(true)}>Open sidebar</button><AgentSidebar active conversations={conversations}
landing={false} collapsed={false} open={open} persistent pending={false} selectedId="one" triggerRef={{current:null}}
onClose={close} onCollapse={()=>{}} onCreate={()=>{}} onRetry={()=>{}} onSelect={id=>{window.selectedThread=id;setOpen(false)}} onPrefetch={()=>{}} /></div></div></MemoryRouter>;}
createRoot(document.getElementById('root')).render(<App />);
`, resolveDir: new URL('..', import.meta.url).pathname, loader:'tsx' }, bundle:true, write:false, outfile:'app.js', jsx:'automatic' });
const server = createServer((_req,res) => { res.setHeader('Content-Type','text/html'); res.end(`<meta name="viewport" content="width=device-width,initial-scale=1"><div id="root"></div><style>html,body,#root{height:100%;margin:0} .chat-workspace,.conversation-workspace{height:100%;display:grid} .chat-workspace button{font:inherit;border:0}.mobile-open{display:none}@media(max-width:760px){.mobile-open{display:block;position:absolute;top:14px;left:14px}}  ${bundle.outputFiles.find(f=>f.path.endsWith('.css')).text}</style><script>${bundle.outputFiles.find(f=>f.path.endsWith('.js')).text}</script>`); });
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const output = new URL('../../../output/thread-browser/', import.meta.url); mkdirSync(output,{recursive:true});
const browser = await chromium.launch({headless:true, ...(process.env.SIDEBAR_BROWSER_CHANNEL ? {channel:process.env.SIDEBAR_BROWSER_CHANNEL} : {})});
try {
  const page = await browser.newPage({viewport:{width:1200,height:850}});
  const errors=[]; page.on('pageerror', e=>errors.push(e.message));
  await page.goto(`http://127.0.0.1:${server.address().port}`);
  await page.getByRole('button',{name:/Shipping notes/}).waitFor();
  assert.equal(await page.locator('.agent-navigation-thread').filter({hasText:'New agent'}).count(),1);
  await page.getByRole('button',{name:/Budget review/}).click();
  assert.equal(await page.evaluate(()=>window.selectedThread),'two');
  await page.getByRole('button',{name:'Search agents'}).click();
  await page.getByRole('searchbox',{name:'Search agents'}).fill('investigate rollout');
  await page.getByRole('dialog',{name:'Search agents'}).getByRole('button',{name:/Shipping notes/}).click();
  assert.equal(await page.evaluate(()=>window.selectedThread),'one','search should find a thread by its last prompt');
  assert.deepEqual(errors,[]);
  await page.screenshot({path:new URL('desktop.png',output).pathname});
  console.log('Thread browsing desktop passed');
  await page.close();
  const mobile=await browser.newPage({viewport:{width:390,height:844},isMobile:true,hasTouch:true});
  await mobile.goto(`http://127.0.0.1:${server.address().port}`);
  await mobile.getByRole('button',{name:'Open sidebar'}).click();
  await mobile.getByRole('dialog',{name:'Workspace navigation'}).getByRole('button',{name:/Budget review/}).click();
  assert.equal(await mobile.evaluate(()=>window.selectedThread),'two');
  await mobile.getByRole('button',{name:'Open sidebar'}).click();
  await mobile.screenshot({path:new URL('mobile.png',output).pathname});
  assert.ok(await mobile.evaluate(()=>document.documentElement.scrollWidth <= window.innerWidth));
  console.log('Thread browsing responsive passed; screenshots: output/thread-browser/{desktop,mobile}.png');
  await mobile.close();
} finally { await browser.close(); server.close(); }
