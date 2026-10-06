import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { connect } from 'node:net';
import { mkdtemp, mkdir, readdir, rm, writeFile, realpath } from 'node:fs/promises';
import { tmpdir, endianness } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const host = fileURLToPath(new URL('../../crates/experimental/nanocodex-computer/src/direct-browser-host.mjs', import.meta.url));
const policy = fileURLToPath(new URL('../../crates/experimental/nanocodex-computer/src/direct-cua-host.mjs', import.meta.url));
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
const rpc = (id, method, params) => ({jsonrpc:'2.0',id,method,params});
const le = endianness() === 'LE';
function peer(input, output = input) {
  const values = [], pending = []; let bytes = Buffer.alloc(0);
  input.on('data', chunk => {
    bytes = Buffer.concat([bytes, chunk]);
    while (bytes.length >= 4) {
      const length = le ? bytes.readUInt32LE() : bytes.readUInt32BE();
      if (bytes.length < length + 4) break;
      const value = JSON.parse(bytes.subarray(4,length+4).toString());
      bytes = bytes.subarray(length+4);
      const next = pending.shift(); next ? next(value) : values.push(value);
    }
  });
  return {next: () => values.length ? Promise.resolve(values.shift()) : new Promise(resolve => pending.push(resolve)),
    send(value) {
      const body=Buffer.from(JSON.stringify(value)), head=Buffer.alloc(4);
      le ? head.writeUInt32LE(body.length) : head.writeUInt32BE(body.length);
      output.write(Buffer.concat([head,body]));
    }, values};
}
async function fixture(t) {
  const directory = await realpath(await mkdtemp(path.join(tmpdir(),'nb-')));
  const sockets = path.join(directory,'s'); await mkdir(sockets,{mode:0o700});
  const child = spawn(process.execPath,[host],{env:{PATH:process.env.PATH,HOME:directory,NANOCODEX_CUA_BROWSER_SOCKET_DIRECTORY:sockets},stdio:['pipe','pipe','pipe']});
  const extension=peer(child.stdout,child.stdin); let stderr='';child.stderr.on('data',b=>{stderr+=b;});
  const exit=new Promise(resolve=>child.once('exit',resolve));
  t.after(async()=>{child.stdin.end();const timer=setTimeout(()=>child.kill('SIGKILL'),3000);await exit;clearTimeout(timer);await rm(directory,{recursive:true,force:true});});
  let socketPath;
  for(let n=0;n<500;n++) {
    const files=await readdir(sockets); if(files.length){socketPath=path.join(sockets,files[0]);break;}
    assert.equal(child.exitCode,null,stderr); await pause(20);
  }
  assert.ok(socketPath,'native message host did not publish a socket');
  return {directory, socketPath, extension, child, async client(){const socket=connect(socketPath);await new Promise((resolve,reject)=>{socket.once('connect',resolve);socket.once('error',reject);});t.after(()=>socket.destroy());return {socket,...peer(socket)};}};
}

test('native-message host multiplexes independent browser sessions and forwards heartbeat without Codex', {timeout:30000}, async t=>{
  const f=await fixture(t), a=await f.client(), b=await f.client();
  a.send(rpc(1,'getInfo',{session_id:'one',turn_id:'turn-α'}));
  b.send(rpc(1,'getInfo',{session_id:'two',turn_id:'turn-β'}));
  const x=await f.extension.next(),y=await f.extension.next();
  assert.notEqual(x.id,y.id);
  f.extension.send({jsonrpc:'2.0',id:y.id,result:y.params});
  f.extension.send({jsonrpc:'2.0',id:x.id,result:x.params});
  assert.deepEqual(await a.next(),{jsonrpc:'2.0',id:1,result:{session_id:'one',turn_id:'turn-α'}});
  assert.deepEqual(await b.next(),{jsonrpc:'2.0',id:1,result:{session_id:'two',turn_id:'turn-β'}});
  const event={jsonrpc:'2.0',method:'onPageEvent',params:{session_id:'one',tabId:4,event:'fixture'}};
  f.extension.send(event);assert.deepEqual(await a.next(),event);assert.deepEqual(await b.next(),event);
  f.extension.send(rpc(77,'ping'));
  const pa=await a.next(),pb=await b.next();assert.equal(pa.method,'ping');assert.equal(pb.method,'ping');
  a.send({jsonrpc:'2.0',id:pa.id,result:false});b.send({jsonrpc:'2.0',id:pb.id,result:'pong'});
  assert.deepEqual(await f.extension.next(),{jsonrpc:'2.0',id:77,result:'pong'});
  f.extension.send(rpc('sidepanel','codexRuntime/ensure',{constraints:{}}));
  assert.equal((await f.extension.next()).error.code,-32601);
  a.send(rpc(9,'createTab',{session_id:'one',turn_id:'turn-α'}));const uncertain=await f.extension.next();a.socket.destroy();
  f.extension.send({jsonrpc:'2.0',id:uncertain.id,result:{id:9}});
  b.send(rpc(10,'turnEnded',{session_id:'two',turn_id:'turn-β'}));const end=await f.extension.next();assert.equal(end.method,'turnEnded');
  assert.equal(uncertain.params.agent_request_header_enabled,true);
  f.extension.send({jsonrpc:'2.0',id:end.id,result:{}});assert.equal((await b.next()).id,10);
  b.socket.destroy();await pause(30);f.extension.send(rpc(78,'ping'));assert.equal((await f.extension.next()).result,false);
});

const runtime=process.env.NANOCODEX_CUA_TEST_RUNTIME;
test('installed upstream browser runtime uses the direct relay and turn cleanup without Codex', {skip:!runtime,timeout:120000}, async t=>{
  const f=await fixture(t), modules=path.join(runtime,'lib/node_modules'), node=path.join(runtime,'bin/node');
  const {connectComputerTools}=await import('../../js/nanocodex-computer/index.mjs');
  const q=s=>"'"+s.replaceAll("'","'\"'\"'")+"'";
  await writeFile(path.join(f.directory,'policy'),`#!/bin/sh\nexec ${q(node)} ${q(policy)} --policy "$@"\n`,{mode:0o700});
  await writeFile(path.join(f.directory,'repl'),`#!/bin/sh\nexec ${q(path.join(runtime,'bin/node_repl'))} --disable-sandbox "$@"\n`,{mode:0o700});
  const observed=[];let running=true;
  let heartbeatResolve; const heartbeatReceipt = new Promise(resolve => { heartbeatResolve = resolve; });
  const replies=(async()=>{while(running){const m=await f.extension.next();if(!running)break;observed.push(m);if(m.id === "live-heartbeat" && !m.method){heartbeatResolve(m);continue;}let result;
    if(m.method==='getInfo')result={type:'extension',family:'chrome',name:'Synthetic browser',capabilities:{browser:[],tab:[]},metadata:{extensionInstanceId:'fixture'},agentRequestHeaderEnabled:false};
    else if(['getTabs','getUserTabs'].includes(m.method))result=[];
    else if(m.method==='turnEnded'||m.method==='nameSession')result={};
    else throw new Error(`Unexpected browser operation ${m.method}`);
    f.extension.send({jsonrpc:'2.0',id:m.id,result});
  }})();
  const computer=await connectComputerTools({executable:process.execPath,args:[path.join(modules,'@oai/cua-repl/bin/cua-repl.mjs')],environment:{HOME:f.directory,CODEX_HOME:f.directory,CODEX_CLI_PATH:path.join(f.directory,'policy'),CUA_REPL_NODE_REPL_PATH:path.join(f.directory,'repl'),CUA_REPL_ENABLED_SURFACES:'browser',NODE_REPL_NODE_PATH:node,NODE_REPL_NODE_MODULE_DIRS:modules,NODE_REPL_TRUSTED_CODE_PATHS:modules,NODE_REPL_DISABLE_ANALYTICS:'1',BROWSER_USE_TINYSKY_ENABLED:'1',BROWSER_USE_DISABLE_AMBIENT_NETWORK:'1',BROWSER_USE_BACKEND_PATHS:f.socketPath}});
  t.after(async()=>{running=false;await computer.close();});
  const context={sessionId:'direct-browser-e2e',turnId:'turn-1',callId:'test',model:'fixture',signal:new AbortController().signal};
  const result=await computer.tool('js').handler({code:'var browsers = await cua.listBrowsers({emit:false}); console.log("BROWSER_COUNT=" + browsers.length); var browser = await cua.getBrowser({id: browsers[0].id}); await browser.nameSession("Isolated host test")',timeout_ms:20000},context);
  assert.equal(result.success,true,result.output.map(c=>c.text??'').filter(s=>s.length<1000).join('\n'));
  assert.ok(result.output.some(c=>c.text?.includes('BROWSER_COUNT=1')));
  const text=result.output.map(c=>c.text??'').join('\n');assert.ok(!text.includes('# Computer Use Confirmations Policy'),'supported confirmation override was ignored');
  f.extension.send(rpc("live-heartbeat", "ping"));
  const heartbeat = await heartbeatReceipt;
  console.log("Installed SDK idle heartbeat:", heartbeat);
  assert.equal(heartbeat.result,"pong","a live idle SDK must preserve the browser control lease");
  await computer.endTurn(context.sessionId,context.turnId);
  await computer.endTurn(context.sessionId,context.turnId);
  assert.ok(observed.some(m=>m.method==='getInfo'&&m.params.session_id===context.sessionId&&m.params.turn_id===context.turnId));
  assert.equal(observed.filter(m=>m.method==='turnEnded'&&m.params.session_id===context.sessionId&&m.params.turn_id===context.turnId).length,1);
  console.log('Observed upstream browser discovery and turnEnded through the native relay; isolated synthetic browser only.');
  void replies;
});
