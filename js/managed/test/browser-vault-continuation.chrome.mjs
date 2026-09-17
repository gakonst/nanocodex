// Real Chromium integration with fake credentials only; no provider/Vault access.
// node --experimental-strip-types js/managed/test/browser-vault-continuation.chrome.mjs
import assert from 'node:assert/strict';
import https from 'node:https';
import { readFileSync, mkdtempSync, rmSync, readdirSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { inspectBrowserVault, snapshotBrowserVault, actBrowserVault, fillBrowserVaultOtp, captureBrowserVaultBinding, sanitizeBrowserVaultText } from '../src/browser-vault.ts';
const packages = new URL('../../../node_modules/.pnpm/', import.meta.url);
const entry = readdirSync(packages).find(name => /^playwright-core@/.test(name));
const { chromium } = await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`, packages));
const temp = mkdtempSync(join(tmpdir(), 'vault-continuation-'));
let browser, server;
try {
  execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', join(temp, 'key'), '-out', join(temp, 'cert'), '-days', '1', '-subj', '/CN=localhost'], { stdio: 'ignore' });
  const received = [];
  server = https.createServer({ key: readFileSync(join(temp, 'key')), cert: readFileSync(join(temp, 'cert')) }, async (req, res) => {
    let body = ''; for await (const chunk of req) body += chunk;
    if (req.method === 'POST') received.push(body);
    res.setHeader('Content-Type', 'text/html'); res.end('<html><body>Ordinary page</body></html>');
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = `https://127.0.0.1:${server.address().port}`;
  browser = await chromium.launch({ executablePath: process.env.CHROME_PATH || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  const context = await browser.newContext({ ignoreHTTPSErrors: true });
  const page = await context.newPage();
  const session = await context.newCDPSession(page);
  const cdp = { async send(method, params) {
    if (method === 'Target.getTargetInfo') return {targetInfo:{type:'page',url:page.url()}};
    if (method === 'Target.attachToTarget') return {sessionId:'page-session'};
    return session.send(method, params);
  }};
  const request = {vault_id:'a'.repeat(22),expected_origin:origin,target_id:'test'};
  await page.goto(origin);
  assert.equal((await inspectBrowserVault(cdp, request)).status, 'unknown');
  await page.setContent('<form method="post"><input autocomplete="one-time-code" name="code"><button>Continue</button></form>');
  assert.equal((await inspectBrowserVault(cdp, request)).status, 'otp_form');
  await page.setContent('<div id="captcha">Complete challenge</div>');
  assert.equal((await inspectBrowserVault(cdp, request)).status, 'challenge');
  await page.setContent('<form method="post"><input name="username"><input type="password"></form>');
  assert.equal((await inspectBrowserVault(cdp, request)).status, 'login_form');
  const user = 'fake-user@example.test', password = 'FAKE&PASSWORD?', code = '826519';
  await page.setContent(`<title>${user}</title><h1>Account</h1><div>${user} ${encodeURIComponent(password)} ${encodeURIComponent(encodeURIComponent(password))} ${code}</div><div hidden>HIDDEN-CONTENT</div><div style="opacity:0">INVISIBLE-CONTENT</div><script>const x='SCRIPT-CONTENT'</script><style>/* STYLE-CONTENT */</style><input value="INPUT-CONTENT"><textarea>TEXTAREA-CONTENT</textarea><a href="/next">Next</a><a href="https://example.test">External</a><form method="post"><button>Continue</button></form>`);
  const snapshot = await snapshotBrowserVault(cdp, request, [user,password,code]);
  const serialized = JSON.stringify(snapshot);
  for (const forbidden of [user,password,code,encodeURIComponent(password),'HIDDEN-CONTENT','INVISIBLE-CONTENT','SCRIPT-CONTENT','STYLE-CONTENT','INPUT-CONTENT','TEXTAREA-CONTENT']) assert.ok(!serialized.includes(forbidden), forbidden);
  assert.equal(snapshot.title, '[redacted]');
  assert.ok(snapshot.text.includes('Account'));
  assert.deepEqual(snapshot.elements.map(el => el.text), ['Next','Continue']);
  const next = await snapshotBrowserVault(cdp,request,[user,password,code]);
  await assert.rejects(actBrowserVault(cdp,request,{action:'click',snapshot_id:snapshot.snapshot_id,ref:'e1'}), /safely/);
  await page.locator('a').first().evaluate(el => el.href = 'https://example.test');
  await assert.rejects(actBrowserVault(cdp,request,{action:'click',snapshot_id:next.snapshot_id,ref:'e1'}), /safely/);
  await assert.rejects(actBrowserVault(cdp,request,{action:'navigate',url:'https://example.test'}), /safely/);
  await page.setContent('<form method="post"><button name="action" value="delete">Delete</button><button name="action" value="save">Save</button></form>');
  assert.deepEqual((await snapshotBrowserVault(cdp,request,[])).elements, []);
  await page.setContent('<form method="post" action="/original"><button>Submit</button></form>');
  const formSnapshot = await snapshotBrowserVault(cdp,request,[]);
  await page.locator('form').evaluate(form => form.action = '/changed');
  await assert.rejects(actBrowserVault(cdp,request,{action:'click',snapshot_id:formSnapshot.snapshot_id,ref:'e1'}), /safely/);
  await page.setContent('<a href="/next" onclick="throw Error(\'unexpected handler\')">Next</a>');
  const link = await snapshotBrowserVault(cdp,request,[]);
  assert.deepEqual(await actBrowserVault(cdp,request,{action:'click',snapshot_id:link.snapshot_id,ref:'e1'}),{status:'action_requested'});
  await page.waitForURL(`${origin}/next`);
  await page.setContent('<form method="post" action="/done"><input name="code" autocomplete="one-time-code"></form>');
  const binding = await captureBrowserVaultBinding(cdp,request);
  let resolved = false;
  await assert.rejects(fillBrowserVaultOtp({cdp,request:{...request,otp_selector:'input',expected_loader_id:'stale'},resolve:async()=>{resolved=true;return code},submit:true}), /safely/);
  assert.equal(resolved,false);
  assert.deepEqual(await fillBrowserVaultOtp({cdp,request:{...request,otp_selector:'input',expected_loader_id:binding.loaderId},resolve:async()=>code,submit:true}), {status:'submitted'});
  await page.waitForURL(`${origin}/done`);
  assert.deepEqual(received,[`code=${code}`]);
  for (const html of [
    '<form method="get"><input name="code"></form>',
    '<form method="post" action="https://example.test"><input name="code"></form>',
    '<form method="post"><input name="code" hidden></form>',
    '<form method="post"><input name="code"><input name="code"></form>'
  ]) {
    await page.setContent(html);
    await assert.rejects(fillBrowserVaultOtp({cdp,request:{...request,otp_selector:'input'},resolve:async()=>code,submit:false}), /safely/);
    assert.equal(await page.locator('input').first().inputValue(),'');
  }
  assert.equal(sanitizeBrowserVaultText('a @ b a%40b a%2540b',['a@b'],100),'[redacted] [redacted] [redacted]');
  console.log('PASS: Chromium private status, OTP/challenge/unknown distinction, bounded redacted snapshots, hidden/value exclusion, native links, stale/mutated refs, cross-origin actions, document-bound OTP POST and unsafe OTP rejection');
} finally {
  await browser?.close();
  if (server) await new Promise(resolve => server.close(resolve));
  rmSync(temp, { recursive: true, force: true });
}
