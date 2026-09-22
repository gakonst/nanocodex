// Run with: node --experimental-strip-types js/managed/test/browser-vault.chrome.mjs
// Uses only fake credentials and a temporary HTTPS server / isolated Chrome profile.
import assert from 'node:assert/strict';
import https from 'node:https';
import { readFileSync, mkdtempSync, rmSync, readdirSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { BROWSER_VAULT_FILL_FUNCTION } from '../src/browser-vault.ts';
const packages = new URL('../../../node_modules/.pnpm/', import.meta.url);
// This repo already installs Playwright transitively; do not change package manifests.
const entry = readdirSync(packages).find(name => /^playwright-core@/.test(name));
const { chromium } = await import(new URL(`${entry}/node_modules/playwright-core/index.mjs`, packages));
const temp = mkdtempSync(join(tmpdir(), 'vault-browser-'));
let browser, server;
try {
  execFileSync('openssl', ['req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', join(temp, 'key'), '-out', join(temp, 'cert'), '-days', '1', '-subj', '/CN=localhost'], { stdio: 'ignore' });
  const received = [];
  server = https.createServer({ key: readFileSync(join(temp, 'key')), cert: readFileSync(join(temp, 'cert')) }, async (req, res) => {
    let body = ''; for await (const chunk of req) body += chunk;
    if (req.method === 'POST') received.push({ path: req.url, body });
    res.setHeader('Content-Type', 'text/html');
    const form = req.url === '/password' ? '<form method="post" action="/done"><input id="password" name="password" type="password"></form>' : '<form method="post" action="/password"><input id="username" name="username"></form>';
    res.end(`<html><body>${req.url === '/done' ? 'Signed in' : form}</body></html>`);
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const origin = `https://127.0.0.1:${server.address().port}`;
  browser = await chromium.launch({ executablePath: process.env.CHROME_PATH || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', headless: true });
  const context = await browser.newContext({ ignoreHTTPSErrors: true });
  const page = await context.newPage();
  const cdp = await context.newCDPSession(page);
  async function fill(userSelector, passSelector, submit = true) {
    const { frameTree: { frame } } = await cdp.send('Page.getFrameTree');
    const { executionContextId } = await cdp.send('Page.createIsolatedWorld', { frameId: frame.id, worldName: 'nanocodex-vault' });
    return cdp.send('Runtime.callFunctionOn', { executionContextId, functionDeclaration: BROWSER_VAULT_FILL_FUNCTION, arguments: [origin, userSelector, passSelector, userSelector ? 'fake-user@example.test' : null, passSelector ? 'FAKE-password-only' : null, submit].map(value => ({ value })), returnByValue: true });
  }
  await page.goto(origin);
  assert.equal((await fill('#username', null)).result.value, true);
  await page.waitForURL(`${origin}/password`);
  assert.deepEqual(received, [{ path: '/password', body: 'username=fake-user%40example.test' }]);
  assert.equal((await fill(null, '#password')).result.value, true);
  await page.waitForURL(`${origin}/done`);
  assert.deepEqual(received[1], { path: '/done', body: 'password=FAKE-password-only' });
  // Cross-origin and GET forms never receive values; duplicate/hidden fields fail closed.
  for (const html of [
    '<form method="post" action="https://example.test"><input id="password" type="password"></form>',
    '<form method="get"><input id="password" type="password"></form>',
    '<form method="post"><input id="password" type="password" hidden></form>',
    '<form method="post"><input id="password" type="password"><input id="password" type="password"></form>',
  ]) {
    await page.setContent(html);
    assert.equal((await fill(null, '#password', false)).result.value, false);
    assert.equal(await page.locator('input').first().inputValue(), '');
  }
  await page.setContent('<form method="post"><input id="password" type="password"></form>');
  assert.equal((await fill(null, '#password', false)).result.value, true);
  assert.equal(await page.locator('#password').inputValue(), 'FAKE-password-only');
  assert.equal(received.length, 2);
  // Controlled login state requires both bubbling events and the submit handler.
  await page.setContent(`<form method="post"><input id="username"><input id="password" type="password"><button disabled>Log in</button></form><script>
    window.events = []; window.login = false;
    for (const input of document.querySelectorAll('input')) for (const type of ['input','change']) input.addEventListener(type, () => { events.push(type); document.querySelector('button').disabled = false; });
    document.querySelector('form').addEventListener('submit', event => { event.preventDefault(); window.login = document.querySelector('#username').value === 'fake-user@example.test' && document.querySelector('#password').value === 'FAKE-password-only'; });
  </script>`);
  assert.equal((await fill('#username', '#password')).result.value, true);
  assert.deepEqual(await page.evaluate(() => ({events,login})), {events:['input','change','input','change'],login:true});
  assert.equal(received.length, 2);
  // A custom control must never fall through to blind native POST.
  await page.setContent('<form method="post"><input id="password" type="password"><div role="button">Log in</div><input type="submit" disabled></form>');
  assert.equal((await fill(null, '#password')).result.value, 'unsupported');
  for (const mutation of ["form.action='https://example.test'", "form.method='get'", "document.querySelector('#password').replaceWith(document.createElement('input'))", "form.target='_blank'"]) {
    await page.setContent(`<form method="post"><input id="username"><input id="password" type="password"></form><script>var form=document.querySelector('form'); document.querySelector('#username').addEventListener('input',()=>{${mutation}});</script>`);
    assert.equal((await fill('#username','#password')).result.value, false);
    assert.equal(await page.locator('input').last().inputValue(), '');
  }
  await page.setContent('<form method="post"><input id="password" type="password"></form><div id="captcha">Human challenge</div>');
  assert.equal((await fill(null,'#password')).result.value,false);
  assert.equal(await page.locator('#password').inputValue(),'');
  console.log('PASS: Chrome two-step same-origin HTTPS POST, username-only/password-only payloads, fill-only, wrong-origin/GET/hidden/duplicate rejection');
} finally {
  await browser?.close();
  if (server) await new Promise(resolve => server.close(resolve));
  rmSync(temp, { recursive: true, force: true });
}
