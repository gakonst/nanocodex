// Real executable and device-approval HTTP transport; synthetic external account service.
// node bin/nanocodex/tests/figma_connect_cli_e2e.mjs /absolute/path/to/nanocodex
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {mkdir, mkdtemp, readFile, writeFile, rm} from 'node:fs/promises';
import {join, resolve} from 'node:path';
const binary=resolve(process.argv[2] || 'target/debug/nanocodex');
const output=resolve('output/figma-cli');await mkdir(output,{recursive:true});
const temporary=await mkdtemp(join(output,'home-'));
const id='f'.repeat(43), account=`0x${'11'.repeat(20)}`;
const connection={id,name:'Figma'};
let mode='success',requests=[],failure;
const server=createServer(async(req,res)=>{
  try {
    let raw='';for await(const chunk of req)raw+=chunk;
    const body=raw?JSON.parse(raw):null;requests.push({method:req.method,path:req.url,body});
    const json=(status,value)=>res.writeHead(status,{'content-type':'application/json'}).end(JSON.stringify(value));
    if(req.url==='/v1/mcp-intents') {
      assert.deepEqual(body,{target:'mcp.figma.com'});
      if(mode==='denied')return json(403,{error:'synthetic-denial'});
      return json(201,connection);
    }
    if(req.url==='/v1/device/register') {
      const resources=body.message.payload[0].params[0].capabilities.auth.resources;
      assert.ok(resources.includes(`urn:nanocodex:mcp:${id}`));assert.ok(resources.includes(`urn:nanocodex:mcp-focus:${id}`));
      return json(200,{device_code:'synthetic_device_code',user_code:'ABCD-EFGH',verification_uri:`${origin}/v1/device/verify`,verification_uri_complete:`${origin}/v1/device/verify?user_code=ABCDEFGH`,expires_in:60,interval:1});
    }
    if(req.url==='/v1/device/token')return json(200,{type:'rpc-responses',payload:[{jsonrpc:'2.0',id:'nanocodex-cli-login',result:{accounts:[{address:account,capabilities:{auth:{approval_id:'a'.repeat(43),mode:'hosted'}}}]}}]});
    assert.equal(req.url,'/v1/connections');assert.deepEqual(body.requested_mcp_connections,[id]);assert.deepEqual(body.requested_connectors,[]);
    return json(201,{authorization_mode:'hosted',grant_token:'s'.repeat(43),account_id:'123e4567-e89b-42d3-a456-426614174000',account_address:account,agent_id:'agent_hosted',mcp_connections:[connection],grant:{id:`0x${'22'.repeat(32)}`,permission:'agent.run',status:'active',expires_at:Math.floor(Date.now()/1000)+86400,capabilities:['nanocodex.agent','agent.output.final','agent.output.actions','agent.history.read','history:read','memory:read','memory:write',`mcp:${id}`],mcp_connections:[connection]},mpp:{}});
  }catch(error){failure=error;res.writeHead(500).end('{}');}
});
await new Promise(resolve=>server.listen(0,'::1',resolve));const origin=`http://nanocodex.localhost:${server.address().port}`;
async function run(target,home){
  await mkdir(home,{recursive:true});
  const child=spawn(binary,['connect',target,'--no-open','--device-base-url',`${origin}/v1/device`],{env:{PATH:process.env.PATH,CODEX_HOME:home,HOME:home,NO_PROXY:'nanocodex.localhost',no_proxy:'nanocodex.localhost'},stdio:['ignore','pipe','pipe']});
  let stdout='',stderr='';child.stdout.on('data',b=>stdout+=b);child.stderr.on('data',b=>stderr+=b);
  const timer=setTimeout(()=>child.kill('SIGKILL'),20000);const code=await new Promise((resolve,reject)=>{child.on('error',reject);child.on('exit',resolve);});clearTimeout(timer);if(failure)throw failure;return {code,stdout,stderr};
}
const evidence=[];
try {
  for(const target of ['figma','mcp.figma.com']){
    requests=[];const home=join(temporary,target);const result=await run(target,home);assert.equal(result.code,0,result.stderr);
    const stored=JSON.parse(await readFile(join(home,'connect.json'),'utf8'));assert.deepEqual(stored.mcp_connections,[connection]);
    assert.ok(!JSON.stringify(stored).includes('mcp.figma.com'));assert.ok(!result.stdout.includes('s'.repeat(43)));
    evidence.push({target,expected:'focused Figma MCP approval and opaque connection stored',observed:'passed',requests:requests.map(({method,path})=>({method,path})),stdout:result.stdout});
  }
  mode='denied';requests=[];const denied=await run('figma',join(temporary,'denied'));assert.notEqual(denied.code,0);assert.deepEqual(requests.map(r=>r.path),['/v1/mcp-intents']);
  evidence.push({target:'figma',expected:'preflight denial fails before device ceremony; no repeated write',observed:'passed',stderr:denied.stderr});
  await writeFile(join(output,'trace.json'),JSON.stringify({command:`node bin/nanocodex/tests/figma_connect_cli_e2e.mjs ${binary}`,evidence},null,2));console.log('PASS Figma CLI: alias, host, exact focused grant, preflight denial; output/figma-cli/trace.json');
}finally{await new Promise(resolve=>server.close(resolve));await rm(temporary,{recursive:true,force:true});}
