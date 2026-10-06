import test from 'node:test';
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {createServicesClient, createHostedRequest} from '../services/index.mjs';

const id='a'.repeat(43), item='v'.repeat(22), op='11111111-1111-4111-8111-111111111111';
async function serve(t, handler) {
  const seen=[];
  const server=createServer(async(req,res)=>{
    let raw='';for await(const chunk of req)raw+=chunk;
    const call={path:req.url,method:req.method,body:raw?JSON.parse(raw):undefined};seen.push(call);
    assert.equal(req.headers.authorization,'Bearer synthetic');
    res.setHeader('content-type','application/json');handler(call,res);
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  t.after(()=>new Promise(resolve=>{server.closeAllConnections();server.close(resolve);}));
  return {client:createServicesClient({apiKey:'synthetic',baseUrl:`http://127.0.0.1:${server.address().port}`}),seen};
}
const end=(res,value)=>res.end(JSON.stringify(value));

test('account SDK: HTTP connector/model/Vault lifecycle and metadata projection',async t=>{
  const {client:c,seen}=await serve(t,(r,res)=>{
    const bad={password:'must-not-escape',seed:'must-not-escape',api_key:'must-not-escape',card_number:'must-not-escape',cvv:'must-not-escape'};
    if(r.path==='/v1/connectors/catalog')return end(res,{providers:[{id:'google',name:'Google Workspace',description:'Workspace',capabilities:[{id:'gmail',name:'Gmail'}]}],...bad});
    if(r.path==='/v1/connectors')return end(res,{connectors:{gmail:{connected:true,connections:[{id,label:'Synthetic',capabilities:['gmail'],...bad}]},cloudflare:{connected:false}},...bad});
    if(r.path==='/v1/credentials/chatgpt/login')return end(res,{state:'pending',verification_url:'https://auth.example/device',user_code:'synthetic-private-panel',poll_after_ms:30000,...bad});
    if(r.path==='/v1/credentials')return end(res,{ready:true,active:'chatgpt',chatgpt:{connected:true,accounts:[{account_id:'example',active:true,connected:true,...bad}]},claude:{connected:false},openai:{connected:false},...bad});
    if(r.path==='/v1/vault/card')return end(res,{status:'ready',vault_id:item,last4:'4242',balance:5,currency:'USD',freshness:'current',observed_at:1000,...bad});
    if(r.method==='DELETE'){res.writeHead(204);res.end();return;}
    return end(res,{id:item,kind:'api_key',name:'Synthetic',created_at:1,ok:true,...bad});
  });
  assert.equal((await c.account.connectors.catalog()).providers[0].capabilities[0].id,'gmail');
  assert.equal((await c.account.connectors.list()).connectors.gmail.connections[0].id,id);
  await c.account.connectors.start('google',{return_to:'/connect'});
  await c.account.connectors.disconnect('google',id);
  await c.account.connectors.cloudflare({vault_id:item});
  await c.account.connectors.link.status(id);
  await c.account.connectors.whatsapp.start({phone:'+14155550123',operation_id:op});
  await c.account.connectors.whatsapp.status();
  await c.account.connectors.mcp.list();await c.account.connectors.mcp.create({target:'https://mcp.example/mcp'});
  await c.account.connectors.mcp.start(id,{return_to:'/connect'});await c.account.connectors.mcp.disconnect(id);
  const overview=await c.account.credentials.overview();assert.equal(overview.active,'chatgpt');assert.equal(overview.chatgpt.accounts[0].account_id,'example');
  const login=await c.account.credentials.chatgpt.start();assert.equal(login.verification_url,'https://auth.example/device');assert.equal(login.poll_after_ms,30000);
  await c.account.credentials.chatgpt.status();await c.account.credentials.chatgpt.disconnect();
  await c.account.credentials.claude.start();await c.account.credentials.claude.status();await c.account.credentials.claude.complete({code:'synthetic-private-input'});await c.account.credentials.claude.disconnect();
  await c.account.credentials.openai.save({api_key:'synthetic-private-input'});await c.account.credentials.openai.delete();
  const entry=await c.account.vault.create('api_key',{name:'Synthetic',api_key:'synthetic-private-input'});assert.equal(entry.id,item);
  await c.account.vault.delete('api_key',item);await c.account.vault.loginOrigin(item,{browser_origin:'https://example.test'});
  await c.account.vault.ssh.put('example',{hostname:'example.test',port:22,username:'example',host_key_sha256:'SHA256:synthetic',generate:true});await c.account.vault.ssh.remove('example');
  await c.account.vault.store({capture_id:item,operation_id:op});
  const card=await c.account.vault.card({vault_id:item,operation:'balance'});assert.equal(card.balance,5);
  await c.account.vault.card({vault_id:item,operation:'refresh',operation_id:op});
  for(const value of [entry,overview,login,card])assert.equal(JSON.stringify(value).includes('must-not-escape'),false);
  assert.deepEqual(seen.map(r=>`${r.method} ${r.path}`),[
    'GET /v1/connectors/catalog','GET /v1/connectors','POST /v1/connectors/google',`DELETE /v1/connectors/google/connections/${id}`,'POST /v1/connectors/cloudflare',`GET /v1/connectors/link?attempt=${id}`,'POST /v1/connectors/whatsapp/start','GET /v1/connectors/whatsapp',
    'GET /v1/connectors/mcp-connections','POST /v1/connectors/mcp-connections',`POST /v1/connectors/mcp-connections/${id}/start`,`DELETE /v1/connectors/mcp-connections/${id}`,
    'GET /v1/credentials','POST /v1/credentials/chatgpt/login','GET /v1/credentials/chatgpt/login','DELETE /v1/credentials/chatgpt','POST /v1/credentials/claude/login','GET /v1/credentials/claude/login','POST /v1/credentials/claude/login/complete','DELETE /v1/credentials/claude','PUT /v1/credentials/openai','DELETE /v1/credentials/openai',
    'POST /v1/credentials/vault/api_key',`DELETE /v1/credentials/vault/api_key/${item}`,`PUT /v1/credentials/vault/login/${item}/origin`,'PUT /v1/credentials/ssh/example','DELETE /v1/credentials/ssh/example','POST /v1/vault/store','POST /v1/vault/card','POST /v1/vault/card']);
  assert.equal(seen[6].body.operation_id,op);assert.equal(seen[27].body.capture_id,item);
  t.diagnostic('30 actual HTTP operations; safe metadata projection; model login values only in trusted private return');
});

test('account SDK: clicked-link handoff, invalid input and Connect isolation',async t=>{
  const {client:c,seen}=await serve(t,(r,res)=>end(res,r.path.startsWith('/v1/account')?{connections:'https://example.test/connect',vault:'https://example.test/vault',wallet:'https://example.test/wallet',access:'https://example.test/access',secret:'discard'}:{url:'https://example.test/vault',origin:'https://example.test',state:op,service:'vault',action:'enroll',kind:'card',secret:'discard'}));
  assert.equal((await c.links({connect:'cloudflare',add:'totp'})).connections,'https://example.test/connect');
  assert.equal((await c.hosted({service:'vault',kind:'card'})).kind,'card');assert.equal(seen.length,2);
  for(const kind of ['login','api_key','card','address','phone','totp']) {
    const link=createHostedRequest({service:'vault',kind});const url=new URL(link.url);
    assert.equal(url.searchParams.get('add'),kind);assert.equal(url.searchParams.has('enrollment_origin'),false);
  }
  assert.throws(()=>c.links({connect:'bad'}),TypeError);
  assert.throws(()=>c.account.vault.card({vault_id:item,operation:'refresh'}),TypeError);
  const g=createServicesClient({connect:{fetch:()=>{throw new Error('must not dispatch');}},grantId:'g'});
  await assert.rejects(g.links(),{code:'account_only'});await assert.rejects(g.hosted(),{code:'account_only'});await assert.rejects(g.account.credentials.overview(),{code:'account_only'});
  assert.equal(seen.length,2);
});

test('account SDK: bounded errors, redirects and unknown mutations never retry',async t=>{
  let mode='unauthorized';
  const {client:c,seen}=await serve(t,(r,res)=>{
    if(mode==='unauthorized'){res.statusCode=403;return end(res,{error:'must-not-escape'});}
    if(mode==='redirect'){res.writeHead(307,{location:'/leak'});res.end();return;}
    if(mode==='oversize')return end(res,{name:'x'.repeat(1024*1024+1),password:'must-not-escape'});
    if(mode==='malformed'){res.end('must-not-escape');return;}
    res.destroy();
  });
  await assert.rejects(c.account.credentials.overview(),e=>e.status===403&&!e.outcomeUnknown&&!String(e).includes('must-not-escape'));
  for(mode of ['redirect','oversize','malformed','drop'])await assert.rejects(c.account.vault.create('api_key',{name:'Test',api_key:'private'}),e=>e.outcomeUnknown&&!String(e).includes('must-not-escape'));
  assert.equal(seen.length,5);assert.ok(seen.every(r=>r.path!=='/leak'));
});
