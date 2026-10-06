// Production-build profile. Run from js/connect-dialog after pnpm run build.
// No credentials, request bodies, cookies, or response bodies enter the report.
import { chromium } from '@playwright/test';
import { createServer } from 'node:http';
import { readFile, mkdir, writeFile, readdir } from 'node:fs/promises';
import { resolve, extname } from 'node:path';
import { gzipSync } from 'node:zlib';
import assert from 'node:assert/strict';

const output = resolve('../../output/connect-profile', process.env.PROFILE_LABEL ?? 'current');
await mkdir(output, { recursive: true });
const port = 4199;
const origin = `http://modal.nanocodex.localhost:${port}`;
const requestId = 'a'.repeat(43);
const address = '0x' + '1'.repeat(40);
const requestPath = `/oauth/requests/${requestId}`;
const callback = `http://127.0.0.1:${port}/callback`;
const appOrigin = new URL(callback).origin;
const appId = 'mcp:' + 'c'.repeat(43);
const resources = [`urn:nanocodex:app:${encodeURIComponent(appId)}`, `urn:nanocodex:origin:${encodeURIComponent(appOrigin)}`, 'urn:nanocodex:authorization:hosted', 'urn:nanocodex:agent:run'];
const server = createServer(async (req, res) => {
  const path = new URL(req.url, origin).pathname;
  if (path === '/callback') { res.end('Approved synthetic connection'); return; }
  try {
    const fixture = path.startsWith('/fixture/');
    const relative = path.replace(/^\/(connect-dialog|fixture)\/?/, '') || 'index.html';
    if (relative.includes('..')) throw Error('bad path');
    const data = await readFile(resolve(fixture ? '../../output/connect-profile/fixture' : 'dist', relative));
    res.setHeader('content-type', { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml' }[extname(relative)] ?? 'application/octet-stream');
    res.end(data);
  } catch { res.statusCode = 404; res.end(); }
});
await new Promise(resolve => server.listen(port, '127.0.0.1', resolve));
const browser = await chromium.launch({ executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE, args: ['--host-resolver-rules=MAP *.nanocodex.localhost 127.0.0.1'] });
const runs = [];

try {
  for (const surface of ['oauth', 'regular']) for (const delayMs of [0, 150]) for (const mode of ['returning', 'sms']) for (let repetition = 0; repetition < 3; repetition++) {
    const context = await browser.newContext();
    await context.tracing.start({ screenshots: true, snapshots: true });
    const page = await context.newPage();
    let persistent = mode === 'returning';
    const calls = [], errors = [], milestones = {};
    const started = performance.now();
    const elapsed = () => Math.round((performance.now() - started) * 10) / 10;
    page.on('pageerror', error => errors.push(error.message));
    await context.route('**/*', async route => {
      const url = new URL(route.request().url()), path = url.pathname;
      if (!path.startsWith('/v1/') && !path.startsWith('/oauth/requests/')) {
        if (url.origin !== origin && url.origin !== appOrigin) throw Error(`Unexpected external request: ${url.origin}`);
        return route.continue();
      }
      const entry = { path, method: route.request().method(), startMs: elapsed(), injectedDelayMs: delayMs };
      calls.push(entry);
      await new Promise(resolve => setTimeout(resolve, delayMs));
      let body;
      switch (path) {
        case requestPath: body = { client_id: 'c'.repeat(43), client_name: 'Synthetic MCP Client', app_id: appId, app_origin: appOrigin, redirect_uri: callback, resource: 'https://nanocodex-connect-api.gakonst.workers.dev/mcp', scope: 'agent:run', resources: [...resources, 'urn:nanocodex:agent:output:final'], base_resources: resources, scope_resources: { 'agent:run': ['urn:nanocodex:agent:output:final'] } }; break;
        case '/v1/me': body = { user: { id: 'synthetic-user', persistent, ...(persistent ? { address } : {}) } }; break;
        case '/v1/hosted-authorizations': body = { account_address: address, approval_id: 'a'.repeat(43), token: 'synthetic-token', connectors: {}, mcp_connections: [], profile: { linked: true } }; break;
        case '/v1/connectors': body = { connectors: { chatgpt: { connected: true } } }; break;
        case '/v1/auth/sms/start': body = { challenge_id: 'synthetic-challenge', expires_in: 600 }; break;
        case '/v1/auth/sms/verify': persistent = true; body = { user: { id: 'synthetic-user', address } }; break;
        case '/v1/connect/hosted-authorization/authorize': body = { code: 's'.repeat(43) }; break;
        case requestPath + '/approve': body = { redirect_uri: callback + '?code=synthetic-code' }; break;
        default: throw Error(`Unexpected API request: ${path}`);
      }
      await route.fulfill({ json: body });
      entry.fulfilledMs = elapsed();
      entry.durationMs = Math.round((entry.fulfilledMs - entry.startMs) * 10) / 10;
    });
    await page.goto(surface === 'oauth' ? `${origin}/connect-dialog/?oauth_request=${requestId}` : `${origin}/fixture/?strict=1`, { waitUntil: 'domcontentloaded' });
    if (mode === 'sms') {
      const phone = page.getByRole('textbox', { name: 'Mobile number' });
      await phone.waitFor(); milestones.signinReadyMs = elapsed();
      await phone.fill('+12025550100');
      milestones.smsStartClickMs = elapsed();
      await page.getByRole('button', { name: 'Text me a code' }).click();
      const code = page.getByRole('textbox', { name: '6-digit code' });
      await code.waitFor(); milestones.codeReadyMs = elapsed();
      await code.fill('123456'); milestones.verifyClickMs = elapsed();
      await page.getByRole('button', { name: 'Continue', exact: true }).click();
    }
    const allow = page.getByRole('button', { name: 'Allow access', exact: true });
    await allow.waitFor();
    await page.waitForFunction(() => [...document.querySelectorAll('button')].some(el => el.textContent === 'Allow access' && !el.disabled));
    milestones.consentReadyMs = elapsed();
    if (surface === 'oauth') assert(!calls.some(call => call.path.endsWith('/authorize') || call.path.endsWith('/approve')));
    else assert.equal(await page.evaluate(() => window.__hostReceipt !== undefined), false);
    const browserPerformance = await page.evaluate(() => ({ navigation: performance.getEntriesByType('navigation').map(e => e.toJSON()), resources: performance.getEntriesByType('resource').map(e => ({ name: new URL(e.name).pathname, initiatorType: e.initiatorType, startTime: e.startTime, duration: e.duration, transferSize: e.transferSize, decodedBodySize: e.decodedBodySize })), paint: performance.getEntriesByType('paint').map(e => e.toJSON()) }));
    await page.screenshot({ path: resolve(output, `${surface}-${mode}-${delayMs}-${repetition}.png`) });
    milestones.allowClickMs = elapsed();
    await allow.click();
    if (surface === 'oauth') await page.waitForURL(`${callback}?code=synthetic-code`);
    else await page.getByRole('status').filter({ hasText: 'Request approved' }).waitFor();
    milestones.approvedMs = elapsed();
    assert.deepEqual(errors, []);
    assert.equal(calls.filter(call => call.path === '/v1/me').length, 1);
    assert.equal(calls.filter(call => call.path.endsWith(surface === 'oauth' ? '/approve' : '/hosted-authorizations')).length, 1);
    const run = { surface, mode, delayMs, repetition, milestones, approvalAfterClickMs: Math.round((milestones.approvedMs - milestones.allowClickMs) * 10) / 10, calls, browserPerformance, errors };
    runs.push(run);
    await writeFile(resolve(output, 'runs.partial.json'), JSON.stringify(runs, null, 2));
    console.log(JSON.stringify({ surface, mode, delayMs, repetition, milestones, requestCount: calls.length }));
    await context.tracing.stop({ path: resolve(output, `${surface}-${mode}-${delayMs}-${repetition}.zip`) });
    await context.close();
  }
  const assets = [];
  for (const name of await readdir('dist/assets')) {
    const data = await readFile(`dist/assets/${name}`);
    assets.push({ name, bytes: data.length, gzipBytes: gzipSync(data).length });
  }
  await writeFile(resolve(output, 'profile.json'), JSON.stringify({ measuredAt: new Date().toISOString(), browser: browser.version(), scope: 'Real production Connect dialog OAuth entry and production-compiled regular ConnectOnboarding fixture (not SDK popup transport); synthetic API transport; cold isolated context per run; local uncompressed static assets; automated input excludes human/SMS delivery wait. Request duration is route handler duration, ResourceTiming covers browser duration. Milestones include automation wait/poll overhead and are not precise paint timings. Asset totals include all lazy chunks, not just initial payload.', assets, runs }, null, 2));
} finally { await browser.close(); server.close(); }
