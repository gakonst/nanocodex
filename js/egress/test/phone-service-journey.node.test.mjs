import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHmac, randomUUID } from 'node:crypto';
import { mkdir, readFile, readdir, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { build } from 'esbuild';
import { Miniflare, convertV4MiniflareOptions, Log, LogLevel } from 'miniflare';
const SID = 'AC'+'1'.repeat(32), TOKEN = 'synthetic-phone-auth-token', WEBHOOK = 'https://account.test/v1/services/phone/webhook';
const outputRoot = new URL('../output/phone-service-journey/', import.meta.url).pathname;
const root = new URL('../',import.meta.url).pathname;
const source = root+'src/egress.ts';

// Real workerd HTTP -> phone service -> durable storage; only Twilio is synthetic.
for (const viaBroker of [false,true]) test(`dedicated phone HTTP journey (${viaBroker ? 'private provider binding' : 'direct provider'})`, {timeout:90000}, async () => {
  const out=join(outputRoot,viaBroker?'broker':'direct');
  await rm(out,{recursive:true,force:true}); await mkdir(out,{recursive:true});
  const result=await build({entryPoints:[source],bundle:true,write:false,format:'esm',platform:'node',external:['cloudflare:*','node:*'],
    alias:{'node-rsa':root+'../nanocodex/tools/browser/unsupportedNodeRsa.mjs'},
    plugins:[{name:'external-transports',setup(b){
      b.onResolve({filter:/^nanocodex\/wasm$/},()=>({path:'./nanocodex.wasm',external:true}));
      b.onResolve({filter:/^\.\/whatsapp-runtime$/},()=>({path:root+'test/whatsapp/runtime.fixture.ts'}));
    }}]});
  let purchaseCalls=0,releaseCalls=0,monthly='1.15',failNextPurchase=false,failNextRelease=false;
  const trace=[]; const owned=new Map();
  const provider=async request=>{
    const url=new URL(request.url);
    assert.equal(request.headers.get('authorization'),'Basic '+Buffer.from(`${SID}:${TOKEN}`).toString('base64'));
    if(url.hostname==='pricing.twilio.com') return Response.json(url.pathname.includes('PhoneNumbers')?{price_unit:'USD',phone_number_prices:[{number_type:'local',current_price:monthly}]}:{price_unit:'USD',inbound_sms_prices:[{number_type:'local',current_price:'0.0083'}]});
    assert.equal(url.hostname,'api.twilio.com');
    if(url.pathname.includes('AvailablePhoneNumbers')) {
      const numbers=url.searchParams.has('Contains')?[url.searchParams.get('Contains')]:['+14155550101','+14155550102'];
      return Response.json({available_phone_numbers:numbers.map(phone_number=>({phone_number,iso_country:'US',capabilities:{SMS:true}}))});
    }
    if(request.method==='POST'&&url.pathname.endsWith('/IncomingPhoneNumbers.json')) {
      purchaseCalls++;const form=new URLSearchParams(await request.text());assert.equal(form.get('SmsUrl'),WEBHOOK);assert.equal(form.get('SmsMethod'),'POST');
      const phone=form.get('PhoneNumber');const id='PN'+String(purchaseCalls).padStart(32,'0');owned.set(id,{sid:id,phone_number:phone,account_sid:SID,friendly_name:form.get("FriendlyName"),sms_url:WEBHOOK,sms_method:"POST"});
      if(failNextPurchase){failNextPurchase=false;return new Response('upstream connection lost after commit',{status:503});}
      return Response.json({sid:id,account_sid:SID,phone_number:phone,sms_url:WEBHOOK,sms_method:'POST'},{status:201});
    }
    if(request.method==='GET' && url.pathname.endsWith('/IncomingPhoneNumbers.json')) return Response.json({incoming_phone_numbers:[...owned.values()].filter(n=>n.phone_number===url.searchParams.get('PhoneNumber') && n.friendly_name===url.searchParams.get('FriendlyName'))});
    if(request.method==='GET' && url.pathname.includes('/IncomingPhoneNumbers/')) {const sid=url.pathname.split('/').at(-1).replace('.json','');return owned.has(sid)?Response.json(owned.get(sid)):Response.json({code:20404},{status:404});}
    if(request.method==='DELETE') {releaseCalls++;owned.delete(url.pathname.split('/').at(-1).replace('.json',''));if(failNextRelease){failNextRelease=false;return new Response(null,{status:503});} return new Response(null,{status:204});}
    throw new Error('Unexpected provider call '+request.method+' '+url.pathname);
  };
  const options={log:new Log(LogLevel.WARN),upstream:'https://phone-service.internal',modules:[{type:'ESModule',path:root+'output/phone-service.js',contents:result.outputFiles[0].text},{type:'CompiledWasm',path:root+'output/nanocodex.wasm',contents:await readFile(root+'../nanocodex/pkg-web/nanocodex_bg.wasm')}],compatibilityFlags:['nodejs_compat'],compatibilityDate:'2026-10-01',durableObjects:{PHONE_SERVICE_ACCOUNTS:{className:'PhoneServiceAccount',useSQLite:true}},resourcePersistencePath:join(out,'storage'),bindings:{ENVIRONMENT:'test',CREDENTIAL_ENCRYPTION_KEY:'MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY',TWILIO_ACCOUNT_SID:SID,TWILIO_AUTH_TOKEN:TOKEN,PHONE_WEBHOOK_URL:WEBHOOK,PHONE_PROVISIONING_ENABLED:'true',PHONE_MAX_MONTHLY_PRICE:'2',PHONE_MAX_INBOUND_SMS_PRICE:'0.02',PHONE_MAX_NUMBERS_PER_OWNER:'3',PHONE_MESSAGE_TTL_SECONDS:'2'},outboundService:provider};
  const providerBundle = viaBroker ? await build({entryPoints:[root+'../managed/src/phone-provider.ts'],bundle:true,write:false,format:'esm',platform:'node',external:['cloudflare:*','node:*']}) : null;
  const start = () => {
    if (!viaBroker) return new Miniflare({...convertV4MiniflareOptions(options),resourcePersistencePath:join(out,'storage')});
    const {TWILIO_ACCOUNT_SID,TWILIO_AUTH_TOKEN,...egressBindings}=options.bindings;
    return new Miniflare(convertV4MiniflareOptions({resourcePersistencePath:join(out,'storage'),upstream:'https://phone-service.internal',log:new Log(LogLevel.WARN),workers:[
      {...options,name:'phone-journey',bindings:egressBindings,serviceBindings:{TWILIO_PHONE_PROVIDER:{name:'phone-provider',entrypoint:'PhoneProvider'}}},
      {name:'phone-provider',modules:true,script:providerBundle.outputFiles[0].text,compatibilityDate:'2026-10-01',compatibilityFlags:['nodejs_compat'],bindings:{TWILIO_ACCOUNT_SID,...(TWILIO_AUTH_TOKEN?{TWILIO_AUTH_TOKEN}:{})},outboundService:provider},
    ]}));
  };
  let mf=start();let origin=(await mf.ready).origin;
  const call=async(owner,path,method='GET',value,human=false)=>{
    const response=await fetch(origin+`/v1/users/${owner}`+path,{method,headers:{...(value?{'content-type':'application/json'}:{}),...(human?{'x-nanocodex-phone-human-approval':'true'}:{})},...(value?{body:JSON.stringify(value)}:{})});
    const data=await response.json();trace.push({owner,path,method,status:response.status,result:data});return {status:response.status,...data};
  };
  const stage=(owner,phone,operation_id=randomUUID())=>call(owner,'/numbers','POST',{operation_id,phone_number:phone,country:'US'});
  const approve=(owner,op,human=true)=>call(owner,`/requests/${op.operation_id}/approve`,'POST',op.kind==='purchase'?{quote_id:op.quote.id,accept_recurring:true}:{confirm_release:true},human);
  const sms=async(number,sid='SM'+'2'.repeat(32),body='Synthetic test code 481927',changes={},invalid=false,canonical=WEBHOOK)=>{
    const form=new URLSearchParams({AccountSid:SID,To:number,From:'+14155550999',MessageSid:sid,Body:body,NumMedia:'0',...changes});
    const signed=canonical+[...form.keys()].sort().map(k=>k+form.get(k)).join('');
    const signature=createHmac('sha1',TOKEN).update(signed).digest('base64');
    const response=await fetch(origin+'/v1/phone/webhook',{method:'POST',headers:{'content-type':'application/x-www-form-urlencoded','x-twilio-signature':invalid?'invalid':signature},body:form});
    trace.push({webhook:sid,status:response.status});return response;
  };
  try {
    assert.equal((await call('alice','/numbers')).numbers.length,0);
    assert.equal((await call('alice','/numbers/available?country=US&area_code=415')).numbers.length,2);
    assert.equal((await call('alice','/numbers/available?country=GB')).status,400);
    const draft=await stage('alice','+14155550101');assert.equal(draft.request.status,'pending_approval');assert.equal(purchaseCalls,0);
    assert.equal((await approve('alice',draft.request,false)).status,403);
    assert.equal((await call('bob',`/requests/${draft.request.operation_id}`)).status,404);
    assert.equal((await call('alice',`/requests/${draft.request.operation_id}/approve`,'POST',{quote_id:randomUUID(),accept_recurring:true},true)).status,409);
    const purchased=await approve('alice',draft.request);assert.equal(purchased.request.status,'complete');assert.equal(purchaseCalls,1);
    const id=purchased.request.number_id;
    await approve('alice',draft.request);await stage('alice','+14155550101',draft.request.operation_id);assert.equal(purchaseCalls,1);
    assert.equal((await stage('alice','+14155550102',draft.request.operation_id)).status,409);
    assert.equal((await call('bob',`/numbers/${id}`)).status,404);
    assert.equal((await call('bob',`/numbers/${id}/messages`)).status,404);
    const competing=await stage('bob','+14155550101');assert.equal((await approve('bob',competing.request)).status,409);assert.equal(purchaseCalls,1);
    assert.equal((await sms('+14155550101',undefined,undefined,{},true)).status,403);
    assert.equal((await sms('+14155550101',undefined,undefined,{},false,'https://attacker.test/v1/services/phone/webhook')).status,403);
    assert.equal((await sms('+14155550101',undefined,undefined,{AccountSid:'AC'+'3'.repeat(32)})).status,400);
    assert.equal((await sms('+14155550998')).status,404);
    assert.equal((await sms('+14155550101')).status,200);assert.equal((await sms('+14155550101')).status,200);
    const inbox=await call('alice',`/numbers/${id}/messages?limit=1`);assert.equal(inbox.messages.length,1);assert.equal(inbox.messages[0].body,'Synthetic test code 481927');
    assert.equal((await sms('+14155550101','SM'+'6'.repeat(32),'second synthetic message')).status,200);
    const firstPage=await call('alice',`/numbers/${id}/messages?limit=1`);assert.ok(firstPage.next_cursor);
    const secondPage=await call('alice',`/numbers/${id}/messages?limit=1&cursor=${encodeURIComponent(firstPage.next_cursor)}`);assert.equal(secondPage.messages.length,1);assert.notEqual(secondPage.messages[0].id,firstPage.messages[0].id);assert.equal(secondPage.next_cursor,null);
    assert.equal((await call('alice',`/numbers/${id}/messages?limit=51`)).status,400);
    assert.equal((await call('alice',`/numbers/${id}/messages?cursor=${btoa('message:other:1')}`)).status,400);
    await mf.dispose();
    const files=await readdir(join(out,'storage'),{recursive:true});
    assert.ok(files.some(file=>file.endsWith('.sqlite')),'real persistent SQLite databases inspected');
    for(const file of files){if(file.endsWith('.sqlite')||file.endsWith('-wal')){const data=await readFile(join(out,'storage',file));assert.equal(data.includes(Buffer.from('Synthetic test code 481927')),false,'message plaintext absent from durable database');}}
    mf=start();origin=(await mf.ready).origin;
    assert.equal((await call('alice',`/requests/${draft.request.operation_id}`)).request.status,'complete');
    await approve('alice',draft.request);assert.equal(purchaseCalls,1);
    await new Promise(resolve=>setTimeout(resolve,2100));
    assert.equal((await call('alice',`/numbers/${id}/messages`)).messages.length,0);
    assert.equal((await sms('+14155550101')).status,200);assert.equal((await call('alice',`/numbers/${id}/messages`)).messages.length,0,'expired signed message replay must not resurrect');
    const priceDraft=await stage('carol','+14155550102');monthly='1.25';const stale=await approve('carol',priceDraft.request);assert.equal(stale.status,409);assert.equal(stale.request.error,'price_changed');assert.equal(purchaseCalls,1);monthly='1.15';
    const deny=await stage('dave','+14155550103');assert.equal((await call('dave',`/requests/${deny.request.operation_id}/deny`,'POST',{},true)).request.status,'denied');await approve('dave',deny.request);assert.equal(purchaseCalls,1);
    const unknown=await stage('eve','+14155550104');failNextPurchase=true;assert.equal((await approve('eve',unknown.request)).request.status,'outcome_unknown');assert.equal(purchaseCalls,2);
    await mf.dispose();mf=start();origin=(await mf.ready).origin;
    assert.equal((await approve('eve',unknown.request)).request.status,'outcome_unknown');assert.equal(purchaseCalls,2);
    assert.equal((await call('eve',`/requests/${unknown.request.operation_id}`)).request.status,'complete');assert.equal(purchaseCalls,2,'read-only reconciliation adopts exact purchased number without rebuy');
    const retry=await stage('eve','+14155550104');assert.equal((await approve('eve',retry.request)).status,409);assert.equal(purchaseCalls,2);
    const release=await call('alice',`/numbers/${id}`,'DELETE',{operation_id:randomUUID()});assert.equal(release.request.status,'pending_approval');assert.equal(releaseCalls,0);
    assert.equal((await approve('alice',release.request,false)).status,403);
    assert.equal((await approve('alice',release.request)).request.status,'complete');assert.equal(releaseCalls,1);
    await approve('alice',release.request);assert.equal(releaseCalls,1);
    assert.equal((await sms('+14155550101','SM'+'4'.repeat(32))).status,404);
    assert.equal((await call('alice',`/numbers/${id}`)).number.status,'released');
    const recycled=await stage('bob','+14155550101');assert.equal((await approve('bob',recycled.request)).status,409,'retired number cannot be assigned to another tenant');
    const releaseUnknownDraft=await stage('frank','+14155550105');const next=await approve('frank',releaseUnknownDraft.request);
    const unknownRelease=await call('frank',`/numbers/${next.request.number_id}`,'DELETE',{operation_id:randomUUID()});failNextRelease=true;
    assert.equal((await approve('frank',unknownRelease.request)).request.status,'outcome_unknown');assert.equal(releaseCalls,2);
    await mf.dispose();mf=start();origin=(await mf.ready).origin;await approve('frank',unknownRelease.request);assert.equal(releaseCalls,2);
    assert.equal((await call('frank',`/requests/${unknownRelease.request.operation_id}`)).request.status,'complete');assert.equal(releaseCalls,2,'read-only reconciliation sees confirmed provider deletion');
    assert.equal((await sms('+14155550105','SM'+'5'.repeat(32))).status,404);
    // Operator reductions apply after restart and fail closed before another provider purchase.
    await mf.dispose();options.bindings.PHONE_MAX_NUMBERS_PER_OWNER='1';options.bindings.PHONE_MAX_MESSAGE_RECEIPTS='1';
    mf=start();origin=(await mf.ready).origin;
    const excess=await stage('eve','+14155550107');assert.equal((await approve('eve',excess.request)).error,'number_limit_reached');assert.equal(purchaseCalls,3);
    assert.equal((await sms('+14155550104','SM'+'7'.repeat(32))).status,200);
    assert.equal((await sms('+14155550104','SM'+'8'.repeat(32))).status,429);
    await new Promise(resolve=>setTimeout(resolve,2100));
    assert.equal((await call('eve',`/numbers/${unknown.request.number_id ?? (await call('eve',`/requests/${unknown.request.operation_id}`)).request.number_id}/messages`)).messages.length,0);
    assert.equal((await sms('+14155550104','SM'+'8'.repeat(32))).status,429,'lifetime SID storage stays bounded after message expiry');
    await mf.dispose();options.bindings.PHONE_PROVISIONING_ENABLED='false';
    mf=start();origin=(await mf.ready).origin;
    assert.equal((await stage('grace','+14155550108')).error,'provisioning_disabled');assert.equal(purchaseCalls,3);
    await mf.dispose();delete options.bindings.TWILIO_AUTH_TOKEN;
    mf=start();origin=(await mf.ready).origin;
    assert.equal((await call('grace','/numbers')).error,'phone_not_configured');
    trace.push({verified:'no live provider calls; purchaseCalls/releaseCalls remained fenced across restarts',purchaseCalls,releaseCalls});
  } finally {await mf.dispose().catch(error=>{if(error.code!=="ERR_SERVER_NOT_RUNNING")throw error;});const {writeFile}=await import('node:fs/promises');await writeFile(join(out,'trace.json'),JSON.stringify(trace,null,2));}
});
