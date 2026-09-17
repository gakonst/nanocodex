// Real browser-level WebSockets and fresh target attachments; fake local pages only.
// node --experimental-strip-types js/managed/test/browser-vault-ref-sessions.chrome.mjs
import assert from 'node:assert/strict';
import https from 'node:https';
import { readFileSync, mkdtempSync, rmSync, readdirSync } from 'node:fs';
import { execFileSync, spawn } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import WebSocket from 'ws';
import { PrivateBrowserCdp, snapshotBrowserVault, actBrowserVault } from '../src/browser-vault.ts';
const temp = mkdtempSync(join(tmpdir(), 'vault-ref-sessions-'));
let chrome, server;
const connections = [];
try {
  execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', join(temp, 'key'), '-out', join(temp, 'cert'), '-days', '1', '-subj', '/CN=localhost'], { stdio: 'ignore' });
  server = https.createServer({ key: readFileSync(join(temp, 'key')), cert: readFileSync(join(temp, 'cert')) }, (_, res) => {
    res.setHeader('Content-Type', 'text/html'); res.end('<html><body>Order details</body></html>');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = `https://127.0.0.1:${server.address().port}`;
  chrome = spawn(process.env.CHROME_PATH || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', ['--headless', '--no-first-run', '--no-default-browser-check', '--ignore-certificate-errors', '--remote-debugging-port=0', `--user-data-dir=${join(temp, 'profile')}`, 'about:blank'], {stdio:'ignore'});
  for (let i=0; i<100; i++) {
    try { readFileSync(join(temp, 'profile', 'DevToolsActivePort')); break; }
    catch { await new Promise(resolve => setTimeout(resolve, 100)); }
  }
  const [port, endpoint] = readFileSync(join(temp, 'profile', 'DevToolsActivePort'), 'utf8').trim().split('\n');
  const connect = async () => {
    const socket = new WebSocket(`ws://127.0.0.1:${port}${endpoint}`);
    await new Promise((resolve, reject) => { socket.once('open', resolve); socket.once('error', reject); });
    socket.accept = () => {};
    const cdp = new PrivateBrowserCdp(socket);
    connections.push(cdp); return cdp;
  };
  const setup = await connect();
  const {targetId} = await setup.send('Target.createTarget',{url:origin});
  await new Promise(resolve => setTimeout(resolve, 500));
  const target = {targetId};
  const {sessionId} = await setup.send('Target.attachToTarget',{targetId,flatten:true});
  const html = '<nav>' + Array.from({length:57},(_,i)=>`<a href="/nav${i}" style="display:block;height:30px">Menu ${i}</a>`).join('') + '</nav><a href="/order">View order details</a>';
  await setup.send('Runtime.evaluate',{expression:`document.body.innerHTML = ${JSON.stringify(html)}`},sessionId);
  setup.close();
  await new Promise(resolve => setTimeout(resolve, 100));
  const cdp = await connect();
  const request = {vault_id:'a'.repeat(22),expected_origin:origin,target_id:target.targetId};
  const snapshot = await snapshotBrowserVault(cdp,request,[]);
  assert.equal(snapshot.elements[57].text,'View order details');
  cdp.close();
  const actionCdp = await connect();
  assert.deepEqual(await actBrowserVault(actionCdp,request,{action:'click',snapshot_id:snapshot.snapshot_id,ref:'e58'}),{status:'action_requested'});
  await new Promise(resolve => setTimeout(resolve, 300));
  assert.equal((await actionCdp.send('Target.getTargetInfo',{targetId})).targetInfo.url, `${origin}/order`);
  const {sessionId: diagnosticsSession} = await actionCdp.send('Target.attachToTarget',{targetId,flatten:true});
  const evaluate = expression => actionCdp.send('Runtime.evaluate',{expression},diagnosticsSession);
  const setHtml = html => evaluate(`document.body.innerHTML = ${JSON.stringify(html)}`);
  const rejectClick = (snapshot, reason) => assert.rejects(actBrowserVault(actionCdp,request,{action:'click',snapshot_id:snapshot.snapshot_id,ref:'e1'}), new RegExp(`\\(${reason}\\)`));
  await setHtml('<a href="/next">Next</a>');
  const stale = await snapshotBrowserVault(actionCdp,request,[]);
  await snapshotBrowserVault(actionCdp,request,[]);
  await rejectClick(stale,'stale_ref');
  const changed = await snapshotBrowserVault(actionCdp,request,[]);
  await evaluate("document.querySelector('a').setAttribute('data-tracking','new')");
  await rejectClick(changed,'changed_element');
  await rejectClick(changed,'snapshot_missing');
  await setHtml('<a href="/next">Next</a><div style="position:fixed;inset:0;z-index:99;background:white">Overlay</div>');
  await rejectClick(await snapshotBrowserVault(actionCdp,request,[]),'occluded');
  await setHtml('<a href="/next" style="display:block;width:200vw;height:20px">Next</a>');
  await rejectClick(await snapshotBrowserVault(actionCdp,request,[]),'outside_viewport');
  console.log('PASS: real browser-level CDP reconnect/ref navigation and fixed stale/missing/mutated/occluded/outside-viewport diagnostics');
} finally {
  for (const cdp of connections) cdp.close();
  if (chrome) { chrome.kill(); await new Promise(resolve => chrome.once('exit', resolve)); }
  if (server) await new Promise(resolve => server.close(resolve));
  rmSync(temp, { recursive: true, force: true });
}
