// Production account catalog over HTTP; the external broker/OAuth provider is synthetic.
// Build connect-ui first, then: node js/account/scripts/figma-ui-journey.mjs
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdirSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import { chromium } from 'playwright-core';
import { canonicalRemoteMcpTarget } from '../../mcp-target.mts';
const output = new URL('../../../output/figma-ui/', import.meta.url);
mkdirSync(output, { recursive: true });
const compiled = await build({ stdin: { contents: `
import React from 'react'; import {createRoot} from 'react-dom/client';
import {QueryClient, QueryClientProvider} from '@tanstack/react-query';
import {ProfileConnectors} from './src/ProfileConnectors';
import {AccountSessionProvider} from './src/AccountSession';
createRoot(document.getElementById('root')).render(<QueryClientProvider client={new QueryClient()}>
<AccountSessionProvider><ProfileConnectors accountId="018f0000-0000-4000-8000-000000000001" presentation={new URLSearchParams(location.search).has('profile')?'profile':'wizard'} refreshSession={async()=>{}} />
</AccountSessionProvider></QueryClientProvider>);`, resolveDir: fileURLToPath(new URL('..', import.meta.url)), loader: 'tsx' }, bundle: true, write: false, outfile: 'app.js', jsx: 'automatic' });
let connections = [], failCreation = true, starts = 0;
const trace = [], errors = [];
const server = createServer(async (req, res) => {
  let raw = ''; for await (const chunk of req) raw += chunk;
  const url = new URL(req.url, 'http://fixture.test');
  const json = (body, status = 200) => {res.writeHead(status, {'content-type':'application/json'});res.end(JSON.stringify(body));};
  if (url.pathname.startsWith('/v1/')) trace.push({method:req.method,path:url.pathname});
  if (url.pathname === '/v1/me') return json({user:{id:'018f0000-0000-4000-8000-000000000001',persistent:true}});
  if (url.pathname === '/v1/connectors') return json({connectors:{}});
  if (url.pathname === '/v1/connectors/mcp-connections') {
    if (req.method === 'GET') return json({mcp_connections:connections});
    assert.equal(JSON.parse(raw).target, 'https://mcp.figma.com/mcp');
    assert.deepEqual(canonicalRemoteMcpTarget(JSON.parse(raw).target), {endpoint:'https://mcp.figma.com/mcp',name:'Figma'});
    if (failCreation) {failCreation=false;return json({error:'mcp_broker_failed'},502);}
    const connection={id:'f'.repeat(43),name:'Figma',status:'authorization_required'};
    connections=[connection]; return json({mcp_connection:connection},201);
  }
  if (url.pathname.endsWith('/start')) {starts++;return json({error:'mcp_broker_failed'},502);}
  if (url.pathname === `/v1/connectors/mcp-connections/${'f'.repeat(43)}` && req.method === 'DELETE') {connections=[];res.writeHead(204);return res.end();}
  if (url.pathname.startsWith('/v1/')) return json({error:'fixture_unavailable'},404);
  res.setHeader('content-type','text/html');res.end(`<div id="root"></div><script>${compiled.outputFiles.find(f=>f.path.endsWith('.js')).text}</script>`);
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
let browser;
try {
  const executablePath=process.env.CHROME_PATH || process.env.NANOCODEX_TEST_BROWSER;
  browser=await chromium.launch({headless:true,...(executablePath?{executablePath}:{})});
  const page=await browser.newPage();page.on('pageerror',e=>errors.push(e.message));page.setDefaultTimeout(8000);
  const origin=`http://127.0.0.1:${server.address().port}`;
  await page.goto(origin);
  const preset=page.getByRole('button',{name:/Figma.*Add connection/});
  await preset.click();await page.getByRole('alert').waitFor();
  assert.equal(connections.length,0);assert.equal(starts,0);
  await preset.click();
  const row=page.locator('.mcp-connector-row').filter({has:page.getByRole('button',{name:'Connect',exact:true})});
  await row.getByRole('button',{name:'Connect',exact:true}).click();
  await row.getByRole('alert').waitFor();assert.equal(starts,1);
  // Failure retains an explicit Connect retry and closes the failed OAuth popup.
  await row.getByRole('button',{name:'Connect',exact:true}).click();
  await row.getByRole('alert').waitFor();
  assert.equal(starts,2);
  connections=[{...connections[0],status:'reauthorization_required'}];await page.reload();
  await page.getByRole('button',{name:'Reconnect',exact:true}).waitFor();
  connections=[{...connections[0],status:'connected'}];await page.goto(origin+'/?profile=1');
  await preset.waitFor();await page.getByRole('button',{name:'Revoke',exact:true}).click();
  await page.getByRole('button',{name:'Revoke',exact:true}).waitFor({state:'detached'});
  assert.equal(connections.length,0);assert.deepEqual(errors,[]);
  await page.screenshot({path:fileURLToPath(new URL('catalog.png',output)),fullPage:true});
  writeFileSync(new URL('trace.json',output),JSON.stringify({command:'node js/account/scripts/figma-ui-journey.mjs',expected:'Figma preset; failure without OAuth; retry; explicit OAuth start failure and retry; reconnect; revoke',observed:'all assertions passed',trace},null,2));
  console.log('PASS Figma catalog: create failure/retry, OAuth failure/retry, reconnect, revoke; output/figma-ui/trace.json');
} finally {await browser?.close();await new Promise(resolve=>server.close(resolve));}
