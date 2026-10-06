import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { build } from 'esbuild';
import { mkdir, writeFile } from 'node:fs/promises';
const require = createRequire(import.meta.url);
const { Miniflare, convertV4MiniflareOptions } = createRequire(require.resolve('wrangler/package.json'))('miniflare');
const root = new URL('../', import.meta.url).pathname;
const op = '11111111-1111-4111-8111-111111111111';
const op2 = '22222222-2222-4222-8222-222222222222';
const chat = '15550000001@s.whatsapp.net';

test('real workerd WhatsApp pairing, read, privacy, revocation and recovery journey', { timeout: 60000 }, async () => {
  const bundle = await build({entryPoints:[root+'test/whatsapp/account.worker.ts'],bundle:true,write:false,format:'esm',platform:'browser',external:['cloudflare:workers'],plugins:[{name:'upstream-only-fixture',setup(b){b.onResolve({filter:/^\.\/whatsapp-runtime$/},()=>({path:root+'test/whatsapp/runtime.fixture.ts'}));}}]});
  const mf = new Miniflare(convertV4MiniflareOptions({workers:[{name:'whatsapp-test',modules:true,script:bundle.outputFiles[0].text,compatibilityDate:'2026-07-29',durableObjects:{ACCOUNTS:{className:'FixtureAccount',useSQLite:true}},bindings:{ENVIRONMENT:'test'},outboundService:()=>new Response('unexpected network',{status:599})}]}));
  const trace = [];
  async function call(account,path,body,expected=200,method=body===undefined?'GET':'POST',headers={}) {
    const response = await mf.dispatchFetch(`http://fixture/${account}${path}`,{method,headers:{'content-type':'application/json',...headers},...(body===undefined?{}:{body:JSON.stringify(body)})});
    const data = await response.json();
    trace.push({account,path,method,status:response.status,result:data});
    assert.equal(response.status,expected,JSON.stringify({path,data}));
    assert.equal(response.headers.get('cache-control'),path.startsWith('/fixture/')?null:'no-store');
    return data;
  }
  try {
    const initial = await call('alice','/status'); assert.equal(initial.connected,false);
    const started = await call('alice','/start',{operation_id:op,phone:'+15550000001'},202);
    assert.equal(started.attempt.state,'ready'); assert.equal(JSON.stringify(started).includes('TEST-1234'),false);
    const pairing = await call('alice',`/pairing?operation_id=${op}`); assert.equal(pairing.code,'TEST-1234');
    await Promise.all([call('alice','/start',{operation_id:op,phone:'+15550000001'}),call('alice','/start',{operation_id:op,phone:'+15550000001'})]);
    assert.equal((await call('alice','/fixture/stats')).pairingRequests,1);
    assert.equal((await call('alice','/start',{operation_id:op,phone:'+15550000002'},409)).error,'operation_conflict');
    await call('bob',`/pairing?operation_id=${op}`,undefined,404);
    await call('alice','/fixture/register',{});
    assert.equal((await call('alice','/status')).connected,true);
    await call('alice','/fixture/connection',{update:{state:'open'}});
    assert.equal((await call('alice','/status')).socket_connected,true);
    await call('alice',`/pairing?operation_id=${op}`,undefined,409);
    const keys = await call('alice','/fixture/keys',{});
    assert.deepEqual(keys.bytes,[0,1,127,255]); assert.equal(keys.typed,true);
    assert.ok(keys.raw.length>=2); for (const envelope of keys.raw) {assert.equal(envelope.version,1);assert.equal(typeof envelope.ciphertext,'string');assert.equal('noiseKey' in envelope,false);}
    assert.equal(JSON.stringify(keys.raw).includes('synthetic-key-plaintext'),false);
    // Eviction is a real workerd object eviction; constructor and persisted alarms recover it.
    await call('recover','/start',{operation_id:op,phone:'+15550000003'},202);
    await call('recover','/fixture/register',{});
    // WhatsApp requires a restart after registered credentials, before first open.
    await call('recover','/fixture/connection',{update:{state:'close',retryable:true}});
    const approvedRestart = await call('recover','/status');
    assert.equal(approvedRestart.connected,true); assert.equal(approvedRestart.attempt.state,'paired');
    assert.equal(approvedRestart.state,'reconnecting');
    await call('recover','/fixture/alarm',{});
    await call('recover','/fixture/connection',{update:{state:'open'}});
    await call('recover','/fixture/keys',{});
    await mf.unsafeEvictDurableObject('whatsapp-test','FixtureAccount',{name:'recover'});
    const recovered = await call('recover','/status');
    assert.equal(recovered.connected,true); assert.equal(recovered.socket_connected,false);
    assert.equal(recovered.state,'reconnecting'); assert.ok(recovered.retry_at);
    // Wait for the persisted reconnect alarm, bounded by five seconds.
    const deadline = Date.now()+5000;
    while ((await call('recover','/fixture/stats')).sessions === 0 && Date.now()<deadline) await new Promise(resolve=>setTimeout(resolve,50));
    assert.equal((await call('recover','/fixture/stats')).sessions,1);
    const restoredKeys = await call('recover','/fixture/keys-read',{});
    assert.deepEqual(restoredKeys.bytes,[0,1,127,255]); assert.equal(restoredKeys.typed,true);
    await call('recover','/fixture/connection',{update:{state:'open'}});
    assert.equal((await call('recover','/status')).socket_connected,true);
    // Exercise expiration through elapsed wall time, without mutating stored rows.
    const editExpiry = Date.now()+1000;
    const expiring = {id:'edit-expiry',chat_id:chat,timestamp:70,expires_at:editExpiry,text:'transient original'};
    await call('recover','/fixture/events',{events:[{type:'message',message:expiring}]});
    await call('recover','/fixture/events',{events:[{type:'message',message:{id:expiring.id,chat_id:chat,timestamp:70,revision:1,text:'transient edited'}}]});
    assert.equal((await call('recover',`/messages?chat_id=${chat}`)).items[0].expires_at,editExpiry);
    await call('recover','/fixture/events',{events:[{type:'message',message:{...expiring,expires_at:editExpiry+60000,revision:2,text:'transient extended'}}]});
    assert.equal((await call('recover',`/messages?chat_id=${chat}`)).items[0].expires_at,editExpiry);
    await new Promise(resolve=>setTimeout(resolve,Math.max(0,editExpiry-Date.now()+20)));
    assert.equal((await call('recover',`/messages?chat_id=${chat}`)).items.length,0);
    assert.equal((await call('recover','/search?q=transient')).items.length,0);
    await call('recover','/fixture/events',{events:[{type:'message',message:{id:expiring.id,chat_id:chat,timestamp:70,revision:99,text:'transient replay'}}]});
    assert.equal((await call('recover','/search?q=transient')).items.length,0);
    const events = [
      {type:'chat',chat:{id:chat,name:'Synthetic chat',timestamp:30}},
      {type:'contact',contact:{id:chat,name:'Synthetic contact'}},
      ...[1,2,3,4].map(n=>({type:'message',message:{id:`m${n}`,chat_id:chat,timestamp:n*10,text:`needle ${n}`}})),
      {type:'message',message:{id:'once',chat_id:chat,timestamp:50,view_once:true,text:'PRIVATE VIEW ONCE'}},
      {type:'message',message:{id:'once',chat_id:chat,timestamp:50,revision:2,view_once:false,text:'PRIVATE VIEW ONCE EDIT'}},
      {type:'message',message:{id:'expired',chat_id:chat,timestamp:60,expires_at:Date.now()-1,text:'PRIVATE EXPIRED'}},
      {type:'revoke',chat_id:chat,id:'m4',timestamp:40},
      {type:'message',message:{id:'m4',chat_id:chat,timestamp:40,revision:99,text:'PRIVATE REVOKED REPLAY'}},
    ];
    await call('alice','/fixture/events',{events});
    assert.equal((await call('alice','/chats')).items[0].name,'Synthetic chat');
    assert.equal((await call('alice','/contacts?q=Synthetic')).items.length,1);
    const first = await call('alice',`/messages?chat_id=${chat}&limit=2`);
    assert.deepEqual(first.items.map(m=>m.id),['once','m3']); assert.equal(first.items[0].text,undefined); assert.ok(first.next_cursor);
    const second = await call('alice',`/messages?chat_id=${chat}&limit=2&cursor=${first.next_cursor}`);
    assert.deepEqual(second.items.map(m=>m.id),['m2','m1']); assert.equal(second.next_cursor,null);
    assert.equal((await call('alice','/search?q=needle')).items.length,3);
    assert.equal((await call('alice','/search?q=PRIVATE')).items.length,0);
    assert.equal((await call('bob',`/messages?chat_id=${chat}`)).items.length,0);
    await call('bob',`/messages?chat_id=${chat}&cursor=${first.next_cursor}`,undefined,400);
    const context = await call('alice',`/context?chat_id=${chat}&id=m2&limit=3`);
    assert.deepEqual(context.items.map(m=>m.id),['m1','m2','m3']);
    await call('alice','/history',{chat_id:chat,before:10,limit:5},202);
    assert.equal((await call('alice','/search?q=history')).items[0].id,'history-old');
    assert.equal((await call('alice','/status')).coverage.history_complete,true);
    await call('alice','/fixture/connection',{update:{state:'close',retryable:true}});
    const reconnect = await call('alice','/status'); assert.equal(reconnect.connected,true);assert.equal(reconnect.socket_connected,false);assert.equal(reconnect.state,'reconnecting');
    await call('alice','/fixture/alarm',{});
    assert.equal((await call('alice','/fixture/stats')).sessions,2);
    await call('alice','/fixture/connection',{update:{state:'open'}});
    await call('alice',`/connections/${started.connection_id}`,undefined,200,'DELETE');
    assert.equal((await call('alice','/status')).connected,false);
    assert.equal((await call('alice',`/messages?chat_id=${chat}`)).items.length,0);
    const relink = await call('alice','/start',{operation_id:op2,phone:'+15550000001'},202);
    assert.notEqual(relink.connection_id,started.connection_id);
    await call('alice','/status',undefined,404,'GET',{'x-nanocodex-connector-connection':started.connection_id});
    await call('alice','/fixture/connection',{session:0,update:{state:'open'}});
    await call('alice','/fixture/events',{session:0,events});
    assert.equal((await call('alice','/status')).connected,false);
    assert.equal((await call('alice',`/messages?chat_id=${chat}`)).items.length,0);
    await call('alice','/fixture/expire',{});
    assert.equal((await call('alice','/status')).attempt.state,'expired');
    await call('alice','/fixture/connection',{update:{state:'open'}});
    assert.equal((await call('alice','/status')).connected,false);
    await call('alice',`/pairing?operation_id=${op2}`,undefined,409);
  } finally {
    await mkdir(root+'output',{recursive:true});
    await writeFile(root+'output/whatsapp-account-journey.json',JSON.stringify(trace,null,2));
    await mf.dispose();
  }
});

for (const initializationFails of [false, true]) test(`status stays available and pairing handles optional transport ${initializationFails ? 'failure' : 'success'}`, { timeout: 30000 }, async () => {
  const bundle = await build({
    stdin: { contents: `import { WhatsAppAccount } from './src/whatsapp-account';
      export { WhatsAppAccount };
      export default { fetch(request, env) { return env.ACCOUNTS.getByName('lazy-status').fetch(request); } };`,
      resolveDir: root, loader: 'ts' },
    bundle: true, write: false, format: 'esm', platform: 'browser', external: ['cloudflare:workers'],
    plugins: [{ name: 'unavailable-upstream-transport', setup(b) {
      b.onResolve({ filter: /^\.\/whatsapp-runtime$/ }, () => ({ path: 'unavailable-transport', namespace: 'optional-upstream' }));
      b.onLoad({ filter: /.*/, namespace: 'optional-upstream' }, () => ({
        contents: initializationFails
          ? `throw new Error('synthetic private transport initialization failure');
             export function createWhatsAppTransportFactory() { throw new Error('unreachable'); }`
          : `export function createWhatsAppTransportFactory() { return { async connect() { return {
               async requestPairingCode() { return 'TEST-5678'; }, async close() {}, async logout() {},
             }; } }; }`, loader: 'js',
      }));
    } }],
  });
  const mf = new Miniflare(convertV4MiniflareOptions({ workers: [{
    name: 'lazy-whatsapp', modules: true, script: bundle.outputFiles[0].text,
    compatibilityDate: '2026-07-29', durableObjects: { ACCOUNTS: { className: 'WhatsAppAccount', useSQLite: true } },
    bindings: { ENVIRONMENT: 'test' }, outboundService: () => new Response('unexpected network', { status: 599 }),
  }] }));
  const trace = [];
  try {
    const status = await mf.dispatchFetch('https://fixture/status');
    const initial = await status.json();
    trace.push({ path: '/status', status: status.status, connected: initial.connected });
    assert.equal(status.status, 200); assert.equal(initial.connected, false);
    const start = await mf.dispatchFetch('https://fixture/start', { method: 'POST',
      headers: { 'content-type': 'application/json' }, body: JSON.stringify({ operation_id: op, phone: '+15550000001' }) });
    const result = await start.json();
    trace.push({ path: '/start', status: start.status, result });
    assert.equal(JSON.stringify(result).includes('synthetic private'), false);
    assert.equal(start.status, 202);
    assert.equal(result.connected, false); assert.equal(result.attempt.state, initializationFails ? 'unknown' : 'ready');
    if (!initializationFails) {
      const pairing = await mf.dispatchFetch(`https://fixture/pairing?operation_id=${op}`);
      assert.equal(pairing.status, 200); assert.equal((await pairing.json()).code, 'TEST-5678');
    }
    assert.equal((await mf.dispatchFetch('https://fixture/status')).status, 200);
    await mkdir(root + '../../output/whatsapp-account', { recursive: true });
    await writeFile(root + `../../output/whatsapp-account/lazy-status-${initializationFails ? 'failure' : 'success'}.json`, JSON.stringify(trace, null, 2));
  } finally { await mf.dispose(); }
});
