// Human-only takeover protocol in real Chrome, using synthetic content and fake input.
import assert from 'node:assert/strict';
import https from 'node:https';
import { readFileSync, mkdtempSync, rmSync, readdirSync } from 'node:fs';
import { execFileSync, spawn } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { registerHooks } from 'node:module';
registerHooks({resolve(specifier, context, nextResolve) { return nextResolve(specifier === './browser-vault' ? './browser-vault.ts' : specifier, context); }});
const { PrivateBrowserCdp } = await import('../src/browser-vault.ts');
const { default: WebSocket } = await import('ws');
const { privateVaultTakeover, releasePrivateVaultTakeover } = await import('../src/browser-vault-takeover.ts');
const packages = new URL('../../../node_modules/.pnpm/', import.meta.url);
const entry = readdirSync(packages).find(name => /^playwright-core@/.test(name));
const { chromium } = await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`, packages));
const temp = mkdtempSync(join(tmpdir(), 'private-touch-'));
let browser, server, chrome, privateCdp;
try {
  execFileSync('openssl', ['req','-x509','-newkey','rsa:2048','-nodes','-keyout',join(temp,'key'),'-out',join(temp,'cert'),'-days','1','-subj','/CN=localhost'],{stdio:'ignore'});
  server = https.createServer({key:readFileSync(join(temp,'key')),cert:readFileSync(join(temp,'cert'))}, (_req,res) => {
    res.setHeader('Content-Type','text/html');
    res.end('<meta name="viewport" content="width=device-width,initial-scale=1"><style>body{margin:20px;font:16px sans-serif;min-height:2800px}input{display:block;height:48px;width:90%;margin:16px 0;font:inherit}</style><h1>Private browser fixture</h1><input type="email" placeholder="Email"><input type="password" placeholder="Password"><textarea></textarea><p>Swipe this page</p>');
  });
  await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
  const origin = `https://127.0.0.1:${server.address().port}`;
  chrome = spawn(process.env.CHROME_PATH || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    ['--headless','--no-first-run','--no-default-browser-check','--remote-debugging-port=0',`--user-data-dir=${join(temp,'profile')}`,'about:blank'],{stdio:'ignore'});
  for (let i=0; i<100; i++) {
    try { readFileSync(join(temp,'profile','DevToolsActivePort')); break; }
    catch { await new Promise(resolve => setTimeout(resolve,100)); }
  }
  const [port, endpoint] = readFileSync(join(temp,'profile','DevToolsActivePort'),'utf8').trim().split('\n');
  browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  const context = await browser.newContext({ignoreHTTPSErrors:true});
  const page = await context.newPage();
  await page.goto(origin);
  const session = await context.newCDPSession(page);
  const socket = new WebSocket(`ws://127.0.0.1:${port}${endpoint}`);
  await new Promise((resolve,reject) => { socket.once('open',resolve); socket.once('error',reject); });
  socket.accept = () => {};
  const cdp = privateCdp = new PrivateBrowserCdp(socket);
  const { targetInfo } = await session.send('Target.getTargetInfo');
  const identity={vault_id:'a'.repeat(22),expected_origin:origin,target_id:targetInfo.targetId}, touch={};
  const act = action => privateVaultTakeover(cdp,identity,action,touch);
  let frame=await act({action:'observe',viewport:{width:390,height:740,mobile:true}});
  assert.equal(frame.width,390); assert.equal(frame.height,740);
  const email=frame.inputs.find(input=>input.type==='email');
  assert.ok(email); assert.equal(frame.keyboard,undefined);
  const x=email.x+email.width/2,y=email.y+email.height/2;
  await act({action:'touch',phase:'start',x,y});
  frame=await act({action:'touch',phase:'end'});
  assert.deepEqual(frame.keyboard,{type:'email',multiline:false});
  await act({action:'edit',delete_backward:0,text:'fake🙂'});
  await act({action:'edit',delete_backward:1,text:'!'});
  assert.equal(await page.locator('input[type=email]').inputValue(),'fake!');
  await act({action:'key',key:'Tab'});
  frame=await act({action:'observe'});
  assert.deepEqual(frame.keyboard,{type:'password',multiline:false});
  await act({action:'touch',phase:'start',x:0.85,y:0.8});
  await act({action:'touch',phase:'move',x:0.85,y:0.55});
  await act({action:'touch',phase:'move',x:0.85,y:0.3});
  await act({action:'touch',phase:'end'});
  assert.ok(await page.evaluate(()=>scrollY)>100,'touch swipes must scroll the actual page');
  await act({action:'touch',phase:'start',x:0.5,y:0.5});
  await act({action:'observe'}); // Explicit recovery cancels the finger, never repeats input.
  assert.equal(touch.active,false);
  await assert.rejects(act({action:'touch',phase:'move',x:0.5,y:0.4}));
  // Simulate a failed edit response: no finger is active, but runtime marks
  // the lease uncertain and requires an explicit refresh before further input.
  const failedEditCdp = {attachTarget: target => cdp.attachTarget(target), send: async (method, params, sid) => {
    if (method === 'Input.insertText') throw new Error('Synthetic disconnected edit response');
    return cdp.send(method, params, sid);
  }};
  await assert.rejects(privateVaultTakeover(failedEditCdp,identity,{action:'edit',delete_backward:0,text:'synthetic'},touch));
  touch.uncertain = true;
  await act({action:'observe'});
  assert.equal(touch.uncertain,false);
  await releasePrivateVaultTakeover(cdp,identity.target_id);
  assert.notEqual((await session.send('Page.getLayoutMetrics')).cssLayoutViewport.clientWidth,390);
  console.log('PASS: mobile viewport, native touch focus, keyboard traits, Unicode edit/delete, real touch scrolling, cancel recovery, viewport cleanup');
} finally {
  privateCdp?.close();
  await browser?.close();
  if (chrome && chrome.exitCode === null) {
    const closed = new Promise(resolve => chrome.once('exit',resolve));
    chrome.kill(); await closed;
  }
  if(server) await new Promise(resolve=>server.close(resolve));
  rmSync(temp,{recursive:true,force:true,maxRetries:5,retryDelay:100});
}
