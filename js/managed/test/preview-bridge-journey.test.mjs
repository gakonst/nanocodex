import assert from 'node:assert/strict';
import WebSocket from 'ws';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { mkdir, writeFile } from 'node:fs/promises';
import { build } from 'esbuild';
import { Miniflare } from 'miniflare';
import { fetch } from "./support/miniflare-fetch.mjs";
const source = `
import { routeManaged } from '../account/worker/managedProxy.ts';
import { receiveManagedPreview } from './src/preview-bridge.ts';
import { createManagedAccessClaims, signManagedAccessClaims } from 'nanocodex/cloudflare/managed-access';
export default { async fetch(request, env) {
  if (env.EDGE) {
    if (new URL(request.url).pathname === '/v1/account/hands/view') {
      const claims = await createManagedAccessClaims(request, {kind:'api_key',userId:'owner',organizationId:'org',teamId:'team',authorizationEpoch:1,capabilities:['agents:read','tools:use']});
      const headers = new Headers(request.headers);
      headers.set('x-nanocodex-access', await signManagedAccessClaims(claims, {NANOCODEX_ACCESS_SECRET:env.NANOCODEX_PREVIEW_BRIDGE_SECRET}));
      request = new Request(request, {headers});
    }
    const forbidden = () => { throw new Error('production_binding_used'); };
    const bindings = { ...env, NANOCODEX_ACCESS_SECRET:env.NANOCODEX_PREVIEW_BRIDGE_SECRET, NANOCODEX_BACKEND: { fetch: forbidden },
      NANOCODEX_HAND_BROKER: { getByName: forbidden },
      NANOCODEX_LIVE_API_KEYS: { getByName: forbidden }, NANOCODEX_LIVE_SESSIONS: { getByName: forbidden } };
    return await routeManaged(request, bindings, new URL(request.url)) ?? new Response('missing', {status:404});
  }
  const incoming = await receiveManagedPreview(request, env);
  if (incoming instanceof Response) return incoming;
  const url = new URL(incoming.url);
  if (url.pathname === '/v1/agents/live' || url.pathname === '/v1/account/hands/view') {
    const pair = new WebSocketPair(); pair[1].accept();
    pair[1].addEventListener('message', event => pair[1].send('preview:' + event.data));
    return new Response(null, {status:101, webSocket:pair[0]});
  }
  if (url.pathname.endsWith('/events')) {
    return new Response(new ReadableStream({start(controller) {
      controller.enqueue(new TextEncoder().encode('data: preview\\n\\n'));
      setTimeout(() => { controller.enqueue(new TextEncoder().encode('data: done\\n\\n')); controller.close(); }, 100);
    }}), {headers:{'content-type':'text/event-stream'}});
  }
  if (incoming.method === 'POST' && incoming.headers.get('origin') !== url.origin) return new Response('forbidden_origin', {status:403});
  return Response.json({ url: incoming.url, cookie: incoming.headers.get('cookie'),
    body: incoming.method === 'POST' ? await incoming.text() : null,
    authorization: incoming.headers.get('authorization'),
    controls: [...incoming.headers.keys()].filter(name => name.startsWith('x-nanocodex-preview-') || name === 'x-nanocodex-owner-id' || name === 'x-nanocodex-connect-user' || name === 'x-nanocodex-connect-capabilities')
  }, {headers:{'set-cookie':'nanocodex_account=synthetic; Path=/; HttpOnly; SameSite=Lax'}});
}};`;
const secret = 'synthetic-preview-secret-with-at-least-32-characters';
const compatibility = { modules:true, compatibilityDate:'2026-07-29', compatibilityFlags:['nodejs_compat'] };
test('account -> managed preview over real workerd HTTP, cookies, streams and fail-closed routing', {timeout:90_000}, async () => {
  const trace = [];
  const bundled = await build({ stdin:{contents:source, resolveDir:fileURLToPath(new URL('..', import.meta.url))},
    bundle:true, write:false, format:'esm', platform:'browser', target:'es2022', external:['cloudflare:workers','node:*'] });
  const script = bundled.outputFiles[0].text;
  const managed = new Miniflare({...compatibility, script});
  const account = new Miniflare({...compatibility, script, bindings:{EDGE:true}});
  try {
    const managedOrigin = (await managed.ready).origin;
    const accountOrigin = (await account.ready).origin;
    const config = {NANOCODEX_PREVIEW_MANAGED_URL:managedOrigin, NANOCODEX_PREVIEW_ACCOUNT_ORIGIN:accountOrigin,
      NANOCODEX_PREVIEW_BRIDGE_SECRET:secret};
    await managed.setOptions({...compatibility, script, port:Number(new URL(managedOrigin).port), bindings:config});
    const configureAccount = bindings => account.setOptions({...compatibility, script, port:Number(new URL(accountOrigin).port), bindings:{EDGE:true,...bindings}});
    await configureAccount(config);
    async function check(label, response, expected) {
      assert.equal(response.status, expected, label); trace.push({label,status:response.status}); return response;
    }
    const body = 'synthetic body: punctuation + unicode λ';
    const response = await check('cookie/origin/body through paired preview', await fetch(accountOrigin+'/v1/auth/sms/verify?q=one', {
      method:'POST', headers:{origin:accountOrigin,cookie:'nanocodex_account=synthetic',authorization:'Bearer synthetic',
        'x-nanocodex-preview-url':'https://attacker.invalid/v1/me','x-nanocodex-owner-id':'forged-owner',
        'x-nanocodex-connect-user':'forged-user','x-nanocodex-connect-capabilities':'all'}, body}), 200);
    assert.match(response.headers.get('set-cookie'), /HttpOnly; SameSite=Lax/);
    assert.deepEqual(await response.json(), {url:accountOrigin+'/v1/auth/sms/verify?q=one',cookie:'nanocodex_account=synthetic',body,authorization:'Bearer synthetic',controls:[]});
    await check('wrong browser origin rejected', await fetch(accountOrigin+'/v1/auth/sms/verify',{method:'POST',headers:{origin:'https://evil.invalid'},body:'bad'}),403);
    await check('direct unsigned managed request rejected',await fetch(managedOrigin+'/v1/me'),403);
    await check('forged managed bridge headers rejected',await fetch(managedOrigin+'/v1/me',{headers:{'x-nanocodex-preview-url':accountOrigin+'/v1/me','x-nanocodex-preview-time':String(Date.now()),'x-nanocodex-preview-signature':'0'.repeat(64)}}),403);
    const stream = await check('SSE response',await fetch(accountOrigin+'/v1/agents/11111111-1111-4111-8111-111111111111/events'),200);
    assert.match(stream.headers.get('content-type'), /text\/event-stream/);
    const reader = stream.body.getReader();
    assert.equal(new TextDecoder().decode((await reader.read()).value),'data: preview\n\n');
    assert.equal(new TextDecoder().decode((await reader.read()).value),'data: done\n\n');
    assert.equal((await reader.read()).done,true);
    for (const path of ['/v1/agents/live','/v1/account/hands/view']) {
      const socket = new WebSocket(accountOrigin.replace('http:','ws:')+path, {headers:{authorization:'Bearer ncx_live_abcdefghijkl_'+ 'a'.repeat(43)}});
      await new Promise((resolve,reject)=>{socket.onopen=resolve;socket.onerror=reject;});
      const echoed = new Promise((resolve,reject)=>{socket.onmessage=event=>resolve(event.data);socket.onerror=reject;});
      socket.send('hello'); assert.equal(await echoed,'preview:hello'); socket.close();
      trace.push({label:'WebSocket bypasses production binding: '+path, outcome:'preview:hello'});
    }
    await configureAccount({...config,NANOCODEX_PREVIEW_BRIDGE_SECRET:'wrong-secret-but-long-enough-for-validation'});
    await check('wrong per-preview secret rejected',await fetch(accountOrigin+'/v1/me'),403);
    await configureAccount({...config,NANOCODEX_PREVIEW_ACCOUNT_ORIGIN:'https://another-preview.invalid'});
    await check('wrong account preview origin rejected',await fetch(accountOrigin+'/v1/me'),403);
    await configureAccount({NANOCODEX_PREVIEW_MANAGED_URL:managedOrigin});
    await check('partial account configuration fails closed',await fetch(accountOrigin+'/v1/me'),503);
    await configureAccount(config);
    await managed.setOptions({...compatibility,script,port:Number(new URL(managedOrigin).port),bindings:{NANOCODEX_PREVIEW_ACCOUNT_ORIGIN:accountOrigin}});
    await check('partial managed configuration fails closed',await fetch(accountOrigin+'/v1/me'),503);
    await configureAccount({...config,NANOCODEX_PREVIEW_MANAGED_URL:'http://127.0.0.1:1'});
    await check('unreachable target never falls back to production',await fetch(accountOrigin+'/v1/me'),503);
    const output = new URL('../../../output/preview-bridge/',import.meta.url);
    await mkdir(output,{recursive:true}); await writeFile(new URL('journey.json',output),JSON.stringify({command:'node --test test/preview-bridge-journey.test.mjs',trace},null,2));
  } finally { await Promise.all([account.dispose(),managed.dispose()]); }
});
