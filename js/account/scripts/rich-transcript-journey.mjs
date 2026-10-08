// Real browser rendering and interaction against the shipped transcript component.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdirSync, readdirSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
const require = createRequire(new URL('../package.json', import.meta.url));
const { build } = require('esbuild');
const packages = new URL('../../../node_modules/.pnpm/', import.meta.url);
const entry = readdirSync(packages).find(name => /^playwright-core@/.test(name));
const { chromium } = await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`, packages));
const markdown = `## Release comparison

A readable response with **emphasis**, inline \`status\`, and [documentation](https://example.com/docs).

| Release | State | Owner | Region | Latency | Notes |
| --- | --- | --- | --- | --- | --- |
| Aurora | Ready | Morgan | Europe | 124 ms | Verified with a long explanatory note that wraps inside the cell. |
| Birch | Reviewing | Casey | North America | 168 ms | Awaiting review |

> Roll out gradually and observe the results.

1. Review the changes
2. Ship the update
   - Watch the dashboard
   - Record the outcome

- [x] Build completed
- [ ] Review pending

\`\`\`typescript
const status = { ready: true, region: "Europe", description: "A deliberately long code line that should scroll within the code block on narrow screens, without widening the entire transcript." };
console.log(status);
\`\`\`

\`\`\`mermaid
flowchart LR
  A[Review] --> B[Build]
  B --> C[Release]
\`\`\`

![Release illustration](/illustration.svg)

![Unavailable illustration](/missing.png)
`;
const bundle = await build({stdin:{contents:`
import React from 'react'; import {createRoot} from 'react-dom/client';
import {TerminalTranscriptSurface} from '../nanocodex-terminal/src/TerminalTranscriptSurface';
import '../nanocodex-terminal/styles.css';
const entries = [
{id:'user',kind:'user',text:'Compare these releases and show the preview.'},
{id:'tool',kind:'tool',tool:{callId:'preview',name:'preview',status:'completed',children:[],output:JSON.stringify({url:'https://preview.example.com/release',port:3000})}},
{id:'answer',kind:'assistant',text:${JSON.stringify(markdown)}},
{id:'failure',kind:'tool',tool:{callId:'failed',name:'exec_command',status:'failed',children:[],input:JSON.stringify({cmd:'check release'}),output:'Check failed: release manifest missing'}}
];
createRoot(document.getElementById('root')).render(<TerminalTranscriptSurface entries={entries} composer={<div>Ready for your next message</div>} canLoadOlder={false} isLoadingOlder={false} inactiveMessage="" mode="chat" status="ready" onLoadOlder={async()=>false}/>);
`,resolveDir:new URL('..',import.meta.url).pathname,loader:'tsx'},bundle:true,write:false,outfile:'app.js',jsx:'automatic'});
const server=createServer((req,res)=>{
 if(req.url==='/app.js'){res.setHeader('Content-Type','text/javascript');res.end(bundle.outputFiles.find(f=>f.path.endsWith('.js')).text);return;}
 if(req.url==='/illustration.svg'){res.setHeader('Content-Type','image/svg+xml');res.end('<svg xmlns="http://www.w3.org/2000/svg" width="720" height="160"><rect width="720" height="160" rx="16" fill="#ececec"/><text x="32" y="88" font-family="sans-serif" font-size="28">Release overview</text></svg>');return;}
 if(req.url==='/missing.png'){res.writeHead(404);res.end();return;}
 res.setHeader('Content-Type','text/html');res.end(`<meta name="viewport" content="width=device-width,initial-scale=1"><style>html,body{margin:0}*{box-sizing:border-box}body{font-family:system-ui}#root{max-width:900px;margin:auto;height:100vh;display:flex;flex-direction:column}${bundle.outputFiles.find(f=>f.path.endsWith('.css')).text}#root > .agent-terminal-shell {height:100%;flex:1} .agent-composer-dock {padding:16px;border-top:1px solid #ddd;color:#666;font-size:13px}</style><div id="root"></div><script src="/app.js"></script>`);
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
const output=new URL('../../../output/rich-transcript/',import.meta.url);mkdirSync(output,{recursive:true});
const browser=await chromium.launch({headless:true,...(process.env.BROWSER_CHANNEL?{channel:process.env.BROWSER_CHANNEL}:{})});
const evidence=[];
try{
 for(const width of [1280,768,390,320]){
  const context=await browser.newContext({viewport:{width,height:1000},permissions:['clipboard-read','clipboard-write']});
  await context.tracing.start({screenshots:true,snapshots:true});
  const page=await context.newPage();const errors=[];page.on('pageerror',error=>errors.push(error.message));
  await page.goto(`http://127.0.0.1:${server.address().port}`);
  await page.locator('.agent-rich-markdown table').waitFor();
  await page.locator('[data-streamdown="mermaid-block"]').scrollIntoViewIfNeeded();
  try { await page.locator('[data-streamdown="mermaid"] svg').waitFor({timeout:10000}); } catch(error) { writeFileSync(new URL('failure.html',output),await page.content()); await page.screenshot({path:new URL('failure.png',output).pathname,fullPage:true}); console.log(errors); throw error; }
  const preview=page.getByRole('link',{name:/Open preview.*View/});assert.equal(await preview.getAttribute('href'),'https://preview.example.com/release');
  assert.equal(await page.locator('.agent-terminal-tool.is-completed').getAttribute('open'),null);
  assert.equal(await page.locator('.agent-terminal-tool.is-failed').getAttribute('open'),'');
  await page.getByRole('button',{name:'Copy table',exact:true}).click();
  assert.match(await page.evaluate(()=>navigator.clipboard.readText()),/Release\tState\tOwner/);
  const downloadPromise=page.waitForEvent('download');await page.getByRole('button',{name:'Save CSV'}).click();
  const download=await downloadPromise;assert.equal(download.suggestedFilename(),'table.csv');await download.saveAs(new URL(`table-${width}.csv`,output).pathname);
  await page.locator('.agent-rich-markdown blockquote').scrollIntoViewIfNeeded();
  assert.equal(await page.locator('.agent-rich-markdown ol > li').count(),2);
  assert.equal(await page.getByRole('checkbox').count(),2);
  await page.locator('.agent-rich-image-unavailable').scrollIntoViewIfNeeded();
  assert.match(await page.locator('.agent-rich-image-unavailable').textContent(),/Unavailable illustration/);
  assert.ok(await page.locator('img[alt="Release illustration"]').evaluate(img=>img.complete&&img.naturalWidth>0));
  const sizes=await page.evaluate(()=>({viewport:innerWidth,document:document.documentElement.scrollWidth,tableWidth:document.querySelector('.agent-rich-table-scroll').clientWidth,tableContent:document.querySelector('.agent-rich-table-scroll').scrollWidth}));
  assert.ok(sizes.document<=width,JSON.stringify(sizes));
  if(width<=390)assert.ok(sizes.tableContent>sizes.tableWidth,'Wide table scrolls locally');
  await page.locator('.agent-dom-transcript').evaluate(el=>el.scrollTop=0);
  await page.screenshot({path:new URL(`transcript-${width}.png`,output).pathname,fullPage:true});
  await page.locator('[data-streamdown="mermaid"] svg').scrollIntoViewIfNeeded();
  await page.screenshot({path:new URL(`diagram-${width}.png`,output).pathname,fullPage:true});
  assert.deepEqual(errors,[]);evidence.push({...sizes,errors,copy:'TSV verified',download:download.suggestedFilename(),diagram:'SVG rendered'});
  await context.tracing.stop({path:new URL(`trace-${width}.zip`,output).pathname});await context.close();
 }
 writeFileSync(new URL('results.json',output),JSON.stringify(evidence,null,2));console.log(JSON.stringify(evidence,null,2));
}finally{await browser.close();server.close();}
