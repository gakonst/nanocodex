import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fork } from 'node:child_process';
import { mkdir, readFile, writeFile, appendFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
// Real workerd SQLite + QuickJS, abrupt OS process loss, then the shipped Rust/WASM wait consumer.
// Only the model transport and external tool responses are synthetic.
const root = fileURLToPath(new URL('..',import.meta.url));
const text = r => typeof r.output === 'string' ? r.output : r.output.map(v=>v.text??'').join('\n');
const cell = r => text(r).match(/Script running with cell ID (\S+)/)?.[1];

for (const mode of ['pending', 'ack-loss', 'background', 'background-failed']) test('workerd hard restart reconciles '+mode, {timeout:60000}, async () => {
  const terminal = mode !== 'pending', failed = mode === 'background-failed';
  const directory = fileURLToPath(new URL('../../../output/code-observation-workerd/'+crypto.randomUUID()+'/',import.meta.url));
  await mkdir(directory,{recursive:true});
  const assets = [];
  // Resolve workspace packages from this checkout even when dependencies are shared.
  const aliases = {};
  for (const name of ['nanocodex', 'nanocodex-tools']) {
    const exports = JSON.parse(await readFile(root+'/../'+name+'/package.json','utf8')).exports;
    for (const [key,value] of Object.entries(exports)) {
      const target = typeof value === 'string' ? value : value.import;
      if (typeof target === 'string') aliases[name+key.slice(1)] = root+'/../'+name+'/'+target;
    }
  }
  const bundle = await build({entryPoints:[root+'/test/fixtures/code-observation-worker.mjs'],bundle:true,write:false,
    format:'esm',platform:'node',conditions:['workerd'],target:'es2022',external:['cloudflare:*','node:*'],
    alias:{...aliases,'nanocodex/host':root+'/../nanocodex/runtime/quickjs-evaluator.mjs'},
    plugins:[{name:'wasm',setup(build){build.onResolve({filter:/\.wasm$/},async args=>{
      const name='./asset-'+assets.length+'.wasm';
      assets.push({name,contents:await readFile(fileURLToPath(new URL(args.path,'file://'+args.resolveDir+'/')))});
      return {path:name,external:true};
    });}}]});
  await writeFile(directory+'/worker.mjs',bundle.outputFiles[0].text);
  for(const asset of assets) await writeFile(directory+'/'+asset.name,asset.contents);
  await writeFile(directory+'/assets.json',JSON.stringify(assets.map(v=>v.name)));
  let child, sequence=0; const trace=[];
  const start = () => new Promise((resolve,reject) => {
    const timer=setTimeout(()=>reject(Error('fixture startup timed out')),15000);
    child=fork(fileURLToPath(new URL('./fixtures/code-observation-process.mjs',import.meta.url)),[directory],
      {detached:true,stdio:['ignore','pipe','pipe','ipc']});
    child.stdout.on('data',v=>appendFile(directory+'/stdout.log',v));
    child.stderr.on('data',v=>appendFile(directory+'/stderr.log',v));
    child.once('error',error=>{clearTimeout(timer);reject(error);});
    child.once('exit',(code,signal)=>{clearTimeout(timer);reject(Error('fixture exited before ready '+code+' '+signal));});
    child.once('message',v=>{clearTimeout(timer);v.ready ? (trace.push({started:v.pid}),resolve()) : reject(Error('not ready'));});
  });
  const call = input => new Promise((resolve,reject) => {
    const request=++sequence;
    const timer=setTimeout(()=>{child?.off('message',receive);reject(Error('RPC timed out '+input.action));},15000);
    const receive=v=>{if(v.request!==request)return;clearTimeout(timer);child.off('message',receive);
      trace.push({input,...v});v.error?reject(Error(v.error)):resolve(v.value);};
    child.on('message',receive);child.send({request,input});
  });
  const kill = () => new Promise((resolve,reject) => {
    const current=child;if(!current)return resolve();
    current.once('exit',()=>{trace.push({killed:current.pid,signal:'SIGKILL'});child=undefined;resolve();});
    try {process.kill(-current.pid,'SIGKILL');} catch(error){reject(error);}
  });
  try {
    await start();
    const yielded=await call({action:'start',terminal,failed}), id=cell(yielded);
    assert.ok(id,JSON.stringify(yielded));
    if(mode.startsWith('background')) assert.equal((await call({action:'background'})).checkpointed,true);
    else if(terminal) assert.equal((await call({action:'finish',id})).code,'host_interrupted');
    else await call({action:'advance'});
    const before=await call({action:'inspect'});
    assert.deepEqual(before.counts,terminal?[{name:'evaluate',n:1},{name:'second',n:1}]:
      [{name:'evaluate',n:1},{name:'first',n:1},{name:'second',n:1},{name:'third',n:1}]);
    await kill(); // Abruptly kill Node and workerd; no reset(), dispose(), or shutdown.
    await start(); // Fresh process/isolate/QuickJS; same real DO SQLite directory.
    const cold=await call({action:'inspect'});
    assert.deepEqual(cold.yield,yielded,'retained outer receipt survives physical process loss');
    assert.deepEqual(cold.counts,before.counts);
    const recovered=await call({action:'wait',id,call:terminal?'final-wait':'wait-after-restart'});
    assert.deepEqual(recovered.nested_calls,[]);
    assert.deepEqual(recovered.notifications??[],[]);
    assert.match(text(recovered),terminal?/Durable terminal observation recovered/:/CODE_CELL_RECOVERED_EVIDENCE/);
    assert.match(text(recovered),/late-second/);
    if(terminal) {assert.equal(recovered.success,!failed);if(failed) assert.match(text(recovered),/background-failure/);assert.equal(recovered.cell.running,false);assert.match(text(recovered),/final-output/);}
    else {
      assert.equal(recovered.success,false);assert.equal(recovered.cell,undefined);
      const detail=JSON.parse(text(recovered).slice(text(recovered).indexOf('\n{')+1));
      assert.deepEqual(detail.completed_effect_receipts.map(v=>v.call_id),['origin/code-1','origin/code-2']);
      assert.deepEqual(detail.pending_effect_call_ids,['origin/code-3']);
      assert.equal(detail.previous_observation.cell.running,true);
    }
    assert.deepEqual(await call({action:'wait',id,call:'repeat'}),recovered);
    assert.deepEqual(await call({action:'wait',id,call:'repeat',terminate:true}),recovered);
    const foreign=await call({action:'wait',id,session:'stranger'});
    assert.match(text(foreign),/CODE_CELL_UNAVAILABLE/);
    assert.doesNotMatch(text(foreign),/late-second|original-op|stable-third/);
    const after=await call({action:'inspect'});
    assert.deepEqual(after,cold,'waits are read-only, do not evaluate or dispatch, and retain pending intent');
    const rust=await call({action:'rust',id});
    assert.equal(rust.finalMessage,'RECOVERY_CONSUMED');
    assert.equal(rust.requests.length,2);
    assert.match(JSON.stringify(rust.requests[1].input),/late-second/);
    assert.doesNotMatch(JSON.stringify(rust.requests[1].input),/unknown field|failed to parse|CODE_CELL_UNAVAILABLE/);
    const rustResults=rust.events.filter(e=>e.type==='tool.result' && e.payload.call_id==='wasm-wait');
    assert.equal(rustResults.length,1);
    assert.equal(rustResults[0].payload.status,terminal && !failed?'completed':'failed');
    assert.match(JSON.stringify(rustResults[0].payload.result),terminal?/final-output/:/outcome unknown/);
    assert.equal(rust.events.filter(e=>e.type==='tool.result' && e.payload.call_id?.startsWith('origin/code-')).length,0);
    assert.deepEqual((await call({action:'inspect'})).counts,cold.counts);
    await writeFile(directory+'/result.json',JSON.stringify({passed:true,mode,terminal,trace},null,2));
    console.log('WORKERD_RECOVERY_EVIDENCE '+directory);
  } finally {
    await kill();
    await writeFile(directory+'/trace.json',JSON.stringify(trace,null,2));
  }
});
