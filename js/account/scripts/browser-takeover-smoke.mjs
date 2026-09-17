// Synthetic UI regression: no credentials, private sites, or external requests.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readdirSync, mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
const require = createRequire(new URL('../package.json', import.meta.url));
const { build } = require('esbuild');
const packages = new URL('../../../node_modules/.pnpm/', import.meta.url);
const entry = readdirSync(packages).find(name => /^playwright-core@/.test(name));
const { chromium } = await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`, packages));
const bundle = await build({ stdin: { contents: `import React from 'react'; import {createRoot} from 'react-dom/client'; import {BrowserTakeoverCard} from './src/BrowserTakeoverCard'; createRoot(document.getElementById('root')).render(<BrowserTakeoverCard authenticated intake={{operation:'browser_takeover',kind:'login',agent_id:'agent',challenge_id:'aaaaaaaaaaaaaaaaaaaaaa',origin:'https://example.test'}} onReceipt={r => window.receipts.push(r)} />);`, resolveDir: new URL('..', import.meta.url).pathname, loader:'tsx' }, bundle:true, write:false, outfile:'app.js', jsx:'automatic' });
const server = createServer((_req,res) => { res.setHeader('Content-Type','text/html'); res.end(`<div id="root"></div><style>${bundle.outputFiles.find(f=>f.path.endsWith('.css')).text}</style><script>window.receipts=[];</script><script>${bundle.outputFiles.find(f=>f.path.endsWith('.js')).text}</script>`); });
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
let browser;
try {
 browser = await chromium.launch({executablePath:process.env.CHROME_PATH || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless:true});
 const fixture = await browser.newPage({viewport:{width:390,height:700}});
 await fixture.setContent('<style>body{margin:0;padding:32px;background:#fafaf8;font:16px system-ui;color:#172326}h1{margin-top:70px}input,button{box-sizing:border-box;width:100%;padding:14px;margin:8px 0;border:1px solid #bbc4c4;border-radius:8px;font:inherit}button{background:#235951;color:white}small{color:#65716f}</style><small>SYNTHETIC TEST PAGE</small><h1>Welcome back</h1><p>Sign in to continue.</p><input placeholder="Email"><input type="password" placeholder="Password"><button>Sign in</button>');
 const fixtureImage = 'data:image/png;base64,' + (await fixture.screenshot()).toString('base64'); await fixture.close();
 const screenshots = new URL('../../../.takeover-ui/', import.meta.url); mkdirSync(screenshots,{recursive:true});
 for (const mode of ['no-frame','initial-error','password','desktop','action-error','paused','unexpected-finish']) {
  const page = await browser.newPage({viewport:{width:mode==='desktop'?1200:390,height:mode==='desktop'?850:740}});
  let releaseObserve, observedResolve; const observed = new Promise(resolve => { observedResolve = resolve; });
  const actions=[]; const errors=[]; page.on('pageerror',error=>errors.push(error.message));
  await page.route('**/v1/agents/agent/browser-vault/takeover', async route => {
   const action=route.request().postDataJSON(); actions.push(action);
   if(mode==='no-frame' && action.action==='observe') { observedResolve(); await new Promise(resolve => { releaseObserve = resolve; }); }
   if(mode==='unexpected-finish' && action.action==='observe') return route.fulfill({json:{status:'finished'}});
   if(action.action==='finish') return route.fulfill({json:{status:'finished'}});
   if(mode==='initial-error' || (mode==='action-error' && action.action==='key')) return route.fulfill({status:500,body:'synthetic failure'});
   return route.fulfill({json:{status:'active',image:fixtureImage,width:390,height:700,keyboard:{type:'password',multiline:false},inputs:[{type:'password',multiline:false,x:0,y:0,width:1,height:1}]}});
  });
  await page.goto(`http://127.0.0.1:${server.address().port}`);
  if(mode==='no-frame') { await observed; assert.equal(await page.getByRole('button',{name:'Done',exact:true}).isEnabled(),true); await page.getByRole('button',{name:'Done',exact:true}).click(); releaseObserve(); }
  else if(mode==='initial-error' || mode==='unexpected-finish') { await page.getByRole('alert').waitFor(); assert.equal(await page.evaluate(()=>window.receipts.length),0); }
  else {
   await page.getByAltText('Private browser screen').waitFor();
   if(mode==='password' || mode==='desktop') await page.screenshot({path:new URL(mode+'.png',screenshots).pathname});
   await page.locator('.private-browser-screen').click({position:{x:30,y:30}});
   assert.equal(await page.evaluate(()=>document.activeElement.type),'password');
   await page.keyboard.type('synthetic');
   await page.waitForFunction(()=>document.querySelector('input[type=password]').value==='');
   // Wait for the serialized input queue before simulating a lifecycle pause,
   // which deliberately discards any unconfirmed input.
   await page.waitForFunction(()=>!document.querySelector('button[aria-label="Show keyboard"]').nextElementSibling.disabled);
   if(mode==='action-error') { await page.keyboard.press('Enter'); await page.getByRole('alert').waitFor(); }
   if(mode==='paused') { await page.evaluate(()=>window.dispatchEvent(new Event('pagehide'))); await page.getByRole('alert').waitFor(); }
  }
  if(mode!=='no-frame') { assert.equal(await page.getByRole('button',{name:'Done',exact:true}).isEnabled(),true); await page.getByRole('button',{name:'Done',exact:true}).click(); }
  await page.waitForFunction(()=>window.receipts.length===1);
  assert.deepEqual(JSON.parse(await page.evaluate(()=>window.receipts[0])),{type:'browser_vault_takeover_receipt',status:'finished',challenge_id:'aaaaaaaaaaaaaaaaaaaaaa'});
  assert.equal(actions.filter(a=>a.action==='finish').length,1);
  if(['password','desktop','action-error','paused'].includes(mode)) { assert.equal(actions.filter(a=>a.action==='edit').map(a=>a.text).join(''),'synthetic'); assert.equal(await page.locator('input[type=password]').count(),0); }
  assert.deepEqual(errors,[]);
  console.log(`PASS ${mode}: completion receipt; no page errors`);
  await page.close();
 }
} finally { await browser?.close(); await new Promise(resolve=>server.close(resolve)); }
