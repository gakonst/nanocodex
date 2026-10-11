import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import { Miniflare } from 'miniflare';
import WebSocket from 'ws';
import { createTools } from 'nanocodex/tools';
import { createAttachment } from 'nanocodex-tools/attachment';
import { createNodeProcessTools } from 'nanocodex-tools/node';
import { fetch } from "./support/miniflare-fetch.mjs";

// Real public HTTP, reverse WebSocket publishers, workerd SQLite and /bin/sh.
// Optional baseline overrides only the two archived source files, not helpers.
const root = fileURLToPath(new URL('../../../', import.meta.url));
const label = process.env.NANOCODEX_CATALOG_LABEL ?? 'candidate';
assert.match(label, /^[a-zA-Z0-9_.-]+$/);
const baseline = process.env.NANOCODEX_CATALOG_BASELINE;
const owner = '00000000-0000-4000-8000-000000000061';
const other = '00000000-0000-4000-8000-000000000062';
const credential = 'Bearer synthetic-catalog-admission';
const percentile = values => {
  const sorted = [...values].sort((a,b)=>a-b);
  return { count: sorted.length, min: sorted[0], p50: sorted[Math.ceil(sorted.length*.5)-1],
    p95: sorted[Math.ceil(sorted.length*.95)-1], max: sorted.at(-1) };
};
const source = `
import { AccountHostedTools } from './src/account-hosted-tools.ts';
export { AccountHostedTools };
const info=console.info.bind(console);
console.info=(record,...rest)=>info(record&&typeof record==='object'?JSON.stringify(record):record,...rest);
export default { fetch(request,env) {
 if(request.headers.get('authorization')!=='${credential}')return new Response(null,{status:401});
 return env.ACCOUNT.getByName('${owner}').fetch(request);
}};
`;

test('catalog discovery scales across 1/8/24 actual reverse publishers without weakening routes', { timeout: 120_000 }, async () => {
  const output = join(root, 'output/hand-catalog-scaling', label, `${Date.now()}-${process.pid}`);
  await mkdir(output, { recursive: true });
  const http = [], wire = [], records = [], lines = [], samples = [], checks = [];
  const publishers = [];
  const originalInfo=console.info;
  console.info=(record,...rest)=>{if(record?.type==='hand.attachment')records.push(record);else originalInfo(record,...rest);};
  let mf, base, failure;
  let sourceHashes;
  const capture = line => { lines.push(line); const offset=line.indexOf('{"type":');
    if(offset>=0)try { records.push(JSON.parse(line.slice(offset))); } catch {} };
  const request = async (path, body) => {
    const start = performance.now();
    const response=await fetch(new URL(path,base), { method:'POST', headers: { authorization:credential,'content-type':'application/json' },
      body:JSON.stringify(body), signal:AbortSignal.timeout(20_000) });
    const value=await response.json();
    const elapsed=performance.now()-start;
    http.push({path,body,status:response.status,value,elapsed_ms:elapsed});
    return {status:response.status,value,elapsed_ms:elapsed};
  };
  const snapshot = async () => {
    const response=await request('/snapshot',{owner_id:owner});
    assert.equal(response.status,200,JSON.stringify(response)); return response;
  };
  const invoke = async (machine,route,call,input,name='exec_command') => request('/invoke', {
    owner_id:owner,machine_id:machine,name,route_token:route,session_id:'catalog-scaling-session',
    thread_id:'catalog-scaling-thread',call_id:call,model:'fixture',input,
  });
  const commandInput = (call, cmd=`printf '%s\\n' '${call}' >> effects.log; printf '%s' '${call}'`) =>
    ({cmd,shell:'/bin/sh',login:false,yield_time_ms:1000});
  async function publish(index, extra = {}) {
    const id=`catalog-hand-${String(index).padStart(2,'0')}`;
    const workspace=join(output,id); await mkdir(workspace,{recursive:true});
    const native=await createNodeProcessTools({workspace});
    const tools=await createTools({attachmentId:id,machines:[{id,name:`Synthetic ${id}`,workspace,capabilities:['shell']}],
      tools:{...Object.fromEntries(native.tools.map(tool=>[tool.name,tool])),
        ...Object.fromEntries(Array.from({length:4},(_,i)=>[`catalog_echo_${index}_${i}`, {
          description:'Synthetic publisher echo',parameters:{type:'object',properties:{},additionalProperties:false},
          supportsParallelToolCalls:true,handler(){return `echo-${index}-${i}`;},
        }])),...extra}});
    const publisher={id,workspace,native,tools}; publishers.push(publisher);
    publisher.attachment=createAttachment(tools,{endpoint:new URL('/tool-host',base).href.replace(/^http/,'ws'),transport:{connect(){
      const socket=new WebSocket(new URL('/tool-host',base).href.replace(/^http/,'ws'),{headers:{authorization:credential,'x-nanocodex-owner-id':owner}});
      publisher.socket=socket;
      const send=socket.send.bind(socket);
      socket.send=(data,...args)=>{wire.push({id,direction:'host',frame:JSON.parse(String(data))});return send(data,...args);};
      socket.on('message',data=>wire.push({id,direction:'broker',frame:JSON.parse(String(data))})); return socket;
    }}}, {reconnectDelayMs:2000,attachmentId:id,machines:[{id,name:`Synthetic ${id}`,workspace,capabilities:['shell']}]});
    assert.equal((await publisher.attachment.connect()).connected,true); return publisher;
  }
  try {
    const overrides=new Map();
    for(const path of ['js/managed/src/account-hosted-tools.ts','js/nanocodex-tools/src/hosted/broker-core.ts']) {
      const contents=await readFile(baseline?join(resolve(baseline),path.split('/').at(-1)):join(root,path),'utf8');
      overrides.set(join(root,path),contents);
    }
    sourceHashes=Object.fromEntries([...overrides].map(([path,contents])=>[path,createHash('sha256').update(contents).digest('hex')]));
    const bundle=await build({stdin:{contents:source,resolveDir:join(root,'js/managed')},bundle:true,write:false,metafile:true,
      format:'esm',platform:'node',conditions:['workerd'],target:'es2022',external:['cloudflare:*','node:*'],
      alias:{'nanocodex-tools/hosted':join(root,'js/nanocodex-tools/src/hosted/index.ts'),
        'nanocodex-tools':join(root,'js/nanocodex-tools/src/index.ts'),
        'node-rsa':join(root,'js/nanocodex/tools/browser/unsupportedNodeRsa.mjs')},
      plugins:[{name:'archived-baseline',setup(builder){builder.onLoad({filter:/\/(broker-core|account-hosted-tools)\.ts$/},args=>
        overrides.has(args.path)?{contents:overrides.get(args.path),loader:'ts',resolveDir:args.path.slice(0,args.path.lastIndexOf('/'))}:undefined);}}],logLevel:'silent'});
    await writeFile(join(output,'worker.mjs'),bundle.outputFiles[0].text);
    await writeFile(join(output,'source-resolution.json'),JSON.stringify({label,baseline,sourceHashes,bundleInputs:Object.keys(bundle.metafile.inputs)},null,2));
    for(const [path,contents] of overrides) await writeFile(join(output,path.split('/').at(-1)),contents);
    mf=new Miniflare({port:0,modules:true,script:bundle.outputFiles[0].text,compatibilityDate:'2026-07-30',
      compatibilityFlags:['nodejs_compat','enable_request_signal'],durableObjects:{ACCOUNT:{className:'AccountHostedTools',useSQLite:true}},
      durableObjectsPersist:join(output,'sqlite'),handleRuntimeStdio(stdout,stderr){
        createInterface({input:stdout}).on('line',capture);createInterface({input:stderr}).on('line',capture);
      }});
    base=String(await mf.ready);
    for(const count of [1,8,24]) {
      while(publishers.length<count) await publish(publishers.length);
      for(let warm=0;warm<3;warm++) await snapshot();
      const snapshots=[], invokes=[];
      let state;
      for(let i=0;i<12;i++) {
        state=await snapshot(); snapshots.push(state.elapsed_ms);
        assert.equal(state.value.machines.length,count);
        assert.equal(state.value.tools.length,count*4);
        assert.ok(state.value.machines.every(row=>row.online));
      }
      const target=state.value.machines.at(-1),route=target.tools.find(tool=>tool.name==='exec_command').route_token;
      for(let i=0;i<12;i++) {
        const call=`scale-${count}-${i}`;
        const result=await invoke(target.machine.id,route,call,commandInput(call)); invokes.push(result.elapsed_ms);
        assert.equal(result.status,200,JSON.stringify(result));assert.equal(result.value.success,true);
        assert.equal(result.value.structured_result.output,call);assert.equal(result.value.structured_result.exit_code,0);
      }
      const last=`scale-${count}-11`;
      const replay=await invoke(target.machine.id,route,last,commandInput(last));
      assert.equal(replay.value.structured_result.output,last);
      const effects=await readFile(join(publishers.at(-1).workspace,'effects.log'),'utf8');
      assert.equal(effects.trim().split('\n').length,12,'replay may not reexecute');
      const resolution=records.filter(row=>row.type==='hand.call.account'&&row.source_call_id?.startsWith(`scale-${count}-`));
      samples.push({hands:count,snapshot_ms:percentile(snapshots),invoke_ms:percentile(invokes),
        account_resolve_ms:percentile(resolution.map(row=>row.resolve_ms).filter(Number.isFinite))});
      checks.push({hands:count,online:true,public_tools:count*4,replay_once:true});
    }
    const state=await snapshot(),target=state.value.machines[0],route=target.tools.find(tool=>tool.name==='exec_command').route_token;
    assert.equal((await request('/snapshot',{owner_id:other})).status,404);
    assert.equal((await request('/invoke',{owner_id:other,name:'exec_command',machine_id:target.machine.id,session_id:'denied',call_id:'denied',route_token:route,input:commandInput('denied')})).status,404);
    const stale=await invoke(target.machine.id,route+'stale','stale',commandInput('stale'));assert.equal(stale.status,409);
    const callsBefore=wire.filter(row=>row.direction==='broker'&&row.frame.type==='call').length;
    const absent=await invoke('absent-hand',route,'absent',commandInput('absent'));assert.equal(absent.status,404);
    assert.equal(wire.filter(row=>row.direction==='broker'&&row.frame.type==='call').length,callsBefore);
    // Public non-machine definitions must resolve to the exact same route.
    const publicTool=state.value.tools[0];
    const publicResult=await request('/invoke',{owner_id:owner,name:publicTool.definition.name,route_token:publicTool.route_token,
      session_id:'catalog-scaling-session',call_id:'public-echo',input:{}});
    assert.equal(publicResult.status,200);assert.equal(publicResult.value.success,true);
    // Transport loss retains identity and process ownership; identical journal
    // recovery keeps the generation. A different runtime must fence old routes.
    const oldProcess=target.tools.find(tool=>tool.name==='write_stdin').route_token;
    const running=await invoke(target.machine.id,route,'process-start',commandInput('process-start',
      'printf PROCESS_START; sleep 4; printf PROCESS_DONE'));
    assert.equal(running.status,200);assert.equal(running.value.success,true);
    assert.equal(running.value.structured_result.output,'PROCESS_START');
    assert.ok(Number.isInteger(running.value.structured_result.session_id));
    assert.equal(running.value.process_route_token,oldProcess,'process route captured at admission');
    publishers[0].socket.terminate();
    let offline;
    for(let i=0;i<100;i++) {offline=await snapshot();if(!offline.value.machines.find(row=>row.machine.id===target.machine.id)?.online)break;
      await new Promise(resolve=>setTimeout(resolve,10));}
    const retained=offline.value.machines.find(row=>row.machine.id===target.machine.id);
    assert.ok(retained);assert.equal(retained.online,false);
    let reconnected;
    for(let i=0;i<500;i++) {reconnected=(await snapshot()).value.machines.find(row=>row.machine.id===target.machine.id);
      if(reconnected?.online)break;await new Promise(resolve=>setTimeout(resolve,10));}
    assert.equal(reconnected.online,true);
    const newRoute=reconnected.tools.find(tool=>tool.name==='exec_command').route_token;
    assert.equal(newRoute,route,'same runtime/catalog recovery retains admitted generation');
    assert.equal(reconnected.tools.find(tool=>tool.name==='write_stdin').route_token,oldProcess);
    let processOutput='',processExit;
    for(let i=0;i<8;i++) {
      const poll=await invoke(target.machine.id,oldProcess,`process-poll-${i}`,
        {session_id:running.value.structured_result.session_id,chars:'',yield_time_ms:1000},'write_stdin');
      assert.equal(poll.status,200);assert.equal(poll.value.success,true);
      processOutput+=poll.value.structured_result.output;
      processExit=poll.value.structured_result.exit_code;
      if(processExit!==undefined)break;
    }
    assert.equal(processExit,0);assert.equal(processOutput,'PROCESS_DONE');
    await publishers[0].attachment.close();
    await publish(0);
    const replacement=(await snapshot()).value.machines.find(row=>row.machine.id===target.machine.id);
    assert.notEqual(replacement.tools.find(tool=>tool.name==='exec_command').route_token,route);
    assert.equal((await invoke(target.machine.id,route,'old-generation',commandInput('old-generation'))).status,409);
    assert.equal((await invoke(target.machine.id,oldProcess,'old-process-runtime',
      {session_id:running.value.structured_result.session_id,chars:'',yield_time_ms:1000},'write_stdin')).status,409);
    // A real non-machine publisher attempting an already exposed public name
    // must be rejected, leaving the admitted catalog and owner unchanged.
    const duplicateName=state.value.tools[0].definition.name;
    const duplicateTools=await createTools({tools:{[duplicateName]:{
      description:'Duplicate public name',parameters:{type:'object',properties:{},additionalProperties:false},
      handler(){throw Error('duplicate must never execute');},
    }}});
    const duplicate=createAttachment(duplicateTools,{endpoint:new URL('/tool-host',base).href.replace(/^http/,'ws'),transport:{connect(){
      const socket=new WebSocket(new URL('/tool-host',base).href.replace(/^http/,'ws'),{headers:{authorization:credential,'x-nanocodex-owner-id':owner}});
      socket.on('message',data=>wire.push({id:'duplicate',direction:'broker',frame:JSON.parse(String(data))}));return socket;
    }}},{attachmentId:'duplicate-public-publisher',reconnect:false});
    try {await assert.rejects(duplicate.connect(),/catalog_contract_mismatch/);}
    finally {await duplicate.close();await duplicateTools.close();}
    assert.equal((await snapshot()).value.tools.length,96);
    checks.push({duplicate_rejected:true,process_poll_after_reconnect:'PROCESS_DONE',old_process_runtime:409,wrong_owner:404,stale_route:409,absent:404,public_dispatch:true,retained_offline:true,
      generation_fenced:true,process_runtime_preserved:true});
    console.log(JSON.stringify({evidence:output,label,samples,checks}));
  } catch(error) {failure=error;throw error;}
  finally {
    for(const publisher of publishers) {await publisher.attachment?.close();await publisher.tools?.close();await publisher.native?.close();}
    await mf?.dispose();
    console.info=originalInfo;
    await writeFile(join(output,'http.json'),JSON.stringify(http,null,2));
    await writeFile(join(output,'wire.json'),JSON.stringify(wire,null,2));
    await writeFile(join(output,'runtime.log'),lines.join('\n')+'\n');
    await writeFile(join(output,'records.json'),JSON.stringify(records,null,2));
    await writeFile(join(output,'result.json'),JSON.stringify({label,sourceHashes,samples,checks,error:failure?.stack},null,2));
    await writeFile(join(output,'README.md'),`Run serially: NANOCODEX_CATALOG_LABEL=${label} ${baseline?`NANOCODEX_CATALOG_BASELINE=${baseline} `:''}pnpm --filter nanocodex-managed-service exec node --test test/hand-catalog-scaling-journey.test.mjs\n\nExpected: 1/8/24 live independent reverse publishers, 4 public tools each; real /bin/sh execution and replay once; owner denial 404, stale route 409, retained disconnected identity, command generation fence, real process poll after reconnect, old runtime fence and duplicate-public-name rejection. Observed: ${JSON.stringify(checks)}\n\nDurations include actual loopback transport, parsing, serialization and shell cost (not one-way WAN estimates). 12 serial warm samples per scale; no overlapping benchmark workload. Source hashes, exact source and bundled worker archived; http.json/wire.json/runtime.log/result.json plus native effects.log and persisted sqlite are inspectable evidence. External admission only is synthetic; no broker helper mocks. Status: ${failure?'FAIL '+failure.message:'PASS'}\n`);
  }
});
