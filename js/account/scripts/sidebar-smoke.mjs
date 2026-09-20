// Synthetic sidebar rendering: no accounts, credentials, or network services.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readdirSync, mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
const require = createRequire(new URL('../package.json', import.meta.url));
const { build } = require('esbuild');
const packages = new URL('../../../node_modules/.pnpm/', import.meta.url);
const entry = readdirSync(packages).find(name => /^playwright-core@/.test(name));
const { chromium } = await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`, packages));
const bundle = await build({ stdin: { contents: `
import React from 'react'; import {createRoot} from 'react-dom/client'; import {MemoryRouter} from 'react-router';
import {AgentSidebar} from './src/AgentSidebar'; import './src/Home.css'; import './src/AgentTerminal.css';
const conversations = [
{id:'one',title:'Improve agent sidebar',presentation:{status:'running',activeTurnIds:['a'],activityTurnId:'a',activity:"I'm checking agent state"}},
{id:'two',title:'Review deployment changes',presentation:{status:'completed',activeTurnIds:[],activity:'Stale activity must be hidden'}},
{id:'three',title:'Repair failing build',presentation:{status:'failed',activeTurnIds:[]}},
{id:'four',title:'Analyze query performance',presentation:{status:'stopping',activeTurnIds:['d']}},
{id:'five',title:'Plan documentation',presentation:{status:'idle',activeTurnIds:[]}},
{id:'six',title:'Older conversation'}];
createRoot(document.getElementById('root')).render(<MemoryRouter><div className="nanocodex-demo chat-workspace"><div className="conversation-workspace"><AgentSidebar active conversations={conversations}
landing={false} collapsed={false} open={window.innerWidth<761} persistent pending={false} selectedId="one" triggerRef={{current:null}}
onClose={()=>{}} onCollapse={()=>{}} onCreate={()=>{}} onRetry={()=>{}} onSelect={()=>{}} onPrefetch={()=>{}} /></div></div></MemoryRouter>);
`, resolveDir: new URL('..', import.meta.url).pathname, loader:'tsx' }, bundle:true, write:false, outfile:'app.js', jsx:'automatic' });
const server = createServer((_req,res) => { res.setHeader('Content-Type','text/html'); res.end(`<meta name="viewport" content="width=device-width,initial-scale=1"><div id="root"></div><style>html,body,#root{height:100%;margin:0} .chat-workspace,.conversation-workspace{height:100%;display:grid} .chat-workspace button{font:inherit;border:0} ${bundle.outputFiles.find(f=>f.path.endsWith('.css')).text}</style><script>${bundle.outputFiles.find(f=>f.path.endsWith('.js')).text}</script>`); });
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const browser = await chromium.launch({headless:true, ...(process.env.SIDEBAR_BROWSER_CHANNEL ? {channel:process.env.SIDEBAR_BROWSER_CHANNEL} : {})});
const output = new URL('../../../.sidebar-ui/', import.meta.url); mkdirSync(output,{recursive:true});
try {
  for (const width of [1200,390]) for (const theme of ["dark","light"]) {
    const page = await browser.newPage({viewport:{width,height:850}});
    const errors=[]; page.on('pageerror', e=>errors.push(e.message));
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    await page.evaluate(theme => document.documentElement.dataset.theme = theme, theme);
    await page.getByText("I'm checking agent state", {exact:true}).waitFor();
    for (const status of ['Running','Ready','Failed','Stopping','Idle','Status unavailable']) assert.equal(await page.getByText(status,{exact:true}).count(),1);
    assert.equal(await page.getByText('Stale activity must be hidden').count(),0);
    assert.deepEqual(errors,[]);
    await page.screenshot({path:new URL(`${width}-${theme}.png`,output).pathname});
    await page.close();
  }
  console.log('Sidebar desktop and mobile checks passed');
} finally { await browser.close(); server.close(); }
