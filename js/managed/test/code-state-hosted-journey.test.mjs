import assert from 'node:assert/strict';
import { test } from 'node:test';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';
import { Miniflare } from 'miniflare';

// Public HTTP fixture, actual workerd/SQLite/QuickJS. Fixture routes select
// crash/corruption boundaries; all execution and storage behavior is shipped.
const source = `
import { DurableObject } from 'cloudflare:workers';
import { createManagedCodeEffectJournal } from './src/managed-recovery-safety.ts';
import { managedCodeEvaluator } from './src/code-evaluator.ts';
import { createCodeRuntime } from 'nanocodex-tools/runtime/code-runtime';
export class CodeSession extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.journal = createManagedCodeEffectJournal(ctx.storage);
    ctx.storage.sql.exec('CREATE TABLE IF NOT EXISTS fixture_effects (singleton INTEGER PRIMARY KEY, count INTEGER NOT NULL); INSERT OR IGNORE INTO fixture_effects VALUES (1,0)');
    const evaluate = managedCodeEvaluator();
    this.runtime = createCodeRuntime({ effect: { handler: async () => {
      ctx.storage.sql.exec('UPDATE fixture_effects SET count=count+1');
      await ctx.storage.sync(); return { count: ctx.storage.sql.exec('SELECT count FROM fixture_effects').one().count };
    }}}, { evaluate: async (source, environment) => {
      if (source === this.pausedSource) { this.entered(); await this.gate; }
      return evaluate(source, environment);
    }, effectJournal: this.journal,
      effectIdentity: () => ({ operationId: 'fixture-operation', modelCallIndex: 1 }) });
  }
  async fetch(request) {
    const input = await request.json();
    try {
      if (input.action === 'concurrent') {
        this.pausedSource = input.staleSource;
        const starting = new Promise(resolve => { this.entered = resolve; });
        let release;
        this.gate = new Promise(resolve => { release = resolve; });
        const stale = this.runtime.executeCode(input.staleSource, input.session, 'stale');
        await starting;
        try {
          const admitted = JSON.parse(await this.runtime.executeCode(input.admittedSource, input.session, 'admitted'));
          release();
          return Response.json({admitted, stale:JSON.parse(await stale)});
        } finally { release(); this.pausedSource = undefined; }
      }
      if (input.action === 'execute') return new Response(await this.runtime.executeCode(input.source, input.session, input.cell), { headers: { 'content-type': 'application/json' } });
      if (input.action === 'abort') {
        const first = JSON.parse(await this.runtime.executeCodeObserved(input.source, input.session, input.cell));
        const cellId = /Script running with cell ID ([^\\n]+)/.exec(first.output)?.[1];
        if (!cellId) throw Error('abort fixture did not yield: ' + JSON.stringify(first));
        const terminated = JSON.parse(await this.runtime.waitCodeObserved(JSON.stringify({cell_id:cellId, terminate:true}), input.session, input.cell+'-wait'));
        return Response.json({first,terminated});
      }
      if (input.action === 'snapshot-summary') return Response.json((await this.journal.snapshotStore(input.session)).map(([key,value])=>[key,typeof value==='string'?value.length:value]));
      if (input.action === 'snapshot') return Response.json(await this.journal.snapshotStore(input.session));
      if (input.action === 'fork') {
        await this.journal.restoreStore(input.destination, await this.journal.snapshotStore(input.session));
        return Response.json(await this.journal.snapshotStore(input.destination));
      }
      if (input.action === 'restore') { await this.journal.restoreStore(input.session,input.entries); return Response.json({restored:true}); }
      if (input.action === 'inspect') return Response.json({effects:this.ctx.storage.sql.exec('SELECT count FROM fixture_effects').one().count,
        cells:this.ctx.storage.sql.exec('SELECT cell_key,writes_hash,expected_version FROM managed_code_cells ORDER BY cell_key').toArray(),
        versions:this.ctx.storage.sql.exec('SELECT session_id,version FROM managed_code_store_versions ORDER BY session_id').toArray(),
        blobs:this.ctx.storage.sql.exec('SELECT blob_key,chunks,bytes FROM managed_code_store_blobs ORDER BY blob_key').toArray()});
      if (input.action === 'orphan-cell') {
        const key=input.kind+':'+JSON.stringify([input.session,'fixture-operation',1,input.cell]);
        this.ctx.storage.sql.exec("INSERT INTO managed_code_store_chunks VALUES (?,0,'[]')",key);
        await this.ctx.storage.sync(); return Response.json({corrupted:key});
      }
      if (input.action === 'corrupt') {
        const key='session:'+input.session;
        if(input.kind==='orphan') {
          this.ctx.storage.sql.exec("INSERT INTO managed_code_store_chunks VALUES (?,0,'[]')",key);
        } else if(input.kind==='missing') this.ctx.storage.sql.exec('DELETE FROM managed_code_store_blobs WHERE blob_key=?',key);
        else if(input.kind==='gap') this.ctx.storage.sql.exec('UPDATE managed_code_store_chunks SET chunk_index=100 WHERE blob_key=? AND chunk_index=0',key);
        else if(input.kind==='hash') this.ctx.storage.sql.exec("UPDATE managed_code_store_chunks SET value_json=replace(value_json,'seed','evil') WHERE blob_key=?",key);
        else if(input.kind==='oversized') this.ctx.storage.sql.exec('UPDATE managed_code_store_chunks SET value_json=? WHERE blob_key=? AND chunk_index=0','x'.repeat(8*1024*1024+1),key);
        await this.ctx.storage.sync(); return Response.json({corrupted:input.kind});
      }
      throw Error('unknown fixture action');
    } catch(error) { return Response.json({error:String(error.message)}, {status:409}); }
  }
}
export default { fetch(request,env) { return env.CODE.getByName('fixture-state').fetch(request); } };
`;

test('hosted Code Mode retains terminal state across cold workerd, rolls back failure/abort, and bounds forks', { timeout: 120_000 }, async () => {
  const root = fileURLToPath(new URL('..', import.meta.url));
  const output = fileURLToPath(new URL('../../../output/code-state-hosted/', import.meta.url)) + crypto.randomUUID();
  await mkdir(output, {recursive:true});
  const wasm = [];
  const bundled = await build({ stdin: {contents:source, resolveDir:root}, bundle:true, write:false, format:'esm', target:'es2022', platform:'node', conditions:['workerd'], external:['cloudflare:*','node:*'], plugins:[{name:'compiled-wasm',setup(build) {
    build.onResolve({filter:/\.wasm$/}, async args => {
      const path = fileURLToPath(new URL(args.path, 'file://' + args.resolveDir + '/'));
      const name = './guest-' + wasm.length + '.wasm';
      wasm.push({name,contents:await readFile(path)}); return {path:name,external:true};
    });
  }}] });
  await writeFile(output+'/worker.mjs', bundled.outputFiles[0].text);
  const modules = [{type:'ESModule',path:'worker.mjs',contents:bundled.outputFiles[0].text}, ...wasm.map(asset=>({type:'CompiledWasm',path:asset.name,contents:asset.contents}))];
  const start = () => new Miniflare({modules, durableObjectsPersist:output+'/sqlite', compatibilityDate:'2026-07-29', compatibilityFlags:['nodejs_compat','durable_object_io_tasks_prevent_eviction'], durableObjects:{CODE:{className:'CodeSession',useSQLite:true}}});
  let mf = start();
  const trace = [];
  const request = async input => {
    const response = await mf.dispatchFetch('https://fixture.example/code', {method:'POST',body:JSON.stringify(input)});
    const value = await response.json();
    // Preserve observed state while bounding generated evidence for large inputs.
    trace.push({input:{...input,...(input.entries?{entries:'oversized fixture omitted'}:{})},status:response.status,value});
    return {status:response.status,value};
  };
  const exec = async (session,cell,source) => (await request({action:'execute',session,cell,source})).value;
  try {
    const originalSource = 'store("memo", {label:"seed",count:1}); text(await tools.effect({})); text(load("memo"));';
    const original = await exec('root','success',originalSource);
    assert.equal(original.success,true);
    assert.equal((await request({action:'inspect'})).value.effects,1);
    await mf.dispose(); mf = start();
    const replay = await exec('root','success',originalSource);
    assert.deepEqual(replay,original); // Exact terminal receipt, not reevaluation.
    assert.equal((await request({action:'inspect'})).value.effects,1);
    const loaded = await exec('root','cold-load','text(load("memo"));');
    assert.equal(loaded.success,true); assert.match(JSON.stringify(loaded.output),/seed/);
    const updated = await exec('root','newer','store("memo", {label:"newer",count:2}); text(load("memo"));');
    assert.equal(updated.success,true);
    assert.deepEqual(await exec('root','success',originalSource),original);
    assert.deepEqual((await request({action:'snapshot',session:'root'})).value,[['memo',{label:'newer',count:2}]]);
    const failed = await exec('root','fail','store("memo", {label:"failed"}); throw new Error("fixture script failure");');
    assert.equal(failed.success,false); assert.match(failed.output,/fixture script failure/);
    const abort = await request({action:'abort',session:'root',cell:'abort',source:'store("memo", {label:"aborted"}); await yield_control(); await new Promise(resolve => setTimeout(resolve,60000));'});
    assert.equal(abort.status,200); assert.match(JSON.stringify(abort.value.terminated.output),/Script terminated/);
    await mf.dispose(); mf = start();
    assert.deepEqual((await request({action:'snapshot',session:'root'})).value,[['memo',{label:'newer',count:2}]]);
    assert.deepEqual((await request({action:'fork',session:'root',destination:'fork'})).value,[['memo',{label:'newer',count:2}]]);
    assert.equal((await exec('fork','fork-write','store("memo", {label:"fork-only"}); text(load("memo"));')).success,true);
    await mf.dispose(); mf = start();
    assert.match(JSON.stringify((await exec('fork','fork-cold','text(load("memo"));')).output),/fork-only/);
    assert.deepEqual((await request({action:'snapshot',session:'root'})).value,[['memo',{label:'newer',count:2}]]);
    assert.equal((await request({action:'fork',session:'root',destination:'fork'})).status,409);
    await request({action:'corrupt',session:'orphan-destination',kind:'orphan'});
    assert.equal((await request({action:'fork',session:'root',destination:'orphan-destination'})).status,409);
    for (const kind of ['cell','receipt']) {
      const cell='orphan-'+kind;
      await request({action:'orphan-cell',session:'root',cell,kind});
      const fenced=await exec('root',cell,'await tools.effect({});');
      assert.equal(fenced.success,false); assert.match(fenced.output,/store identity is missing/);
    }
    const tooLarge = await request({action:'restore',session:'too-large',entries:[['large','x'.repeat(8*1024*1024+1)]]});
    assert.equal(tooLarge.status,409); assert.match(tooLarge.value.error,/exceeds/);
    assert.equal((await request({action:'restore',session:'aggregate',entries:[['large','x'.repeat(5*1024*1024)]]})).status,200);
    const overflow=await exec('aggregate','overflow','store("second","y".repeat(4*1024*1024));');
    assert.equal(overflow.success,false); assert.match(overflow.output,/exceeds/);
    assert.deepEqual((await request({action:'snapshot-summary',session:'aggregate'})).value,[['large',5*1024*1024]]);
    for (const kind of ['missing','gap','hash','oversized']) {
      const session='corrupt-'+kind;
      assert.equal((await exec(session,'seed','store("memo","seed");')).success,true);
      assert.equal((await request({action:'corrupt',session,kind})).status,200);
      const rejected = await request({action:'fork',session,destination:'rejected-'+kind});
      assert.equal(rejected.status,409); assert.match(rejected.value.error,/snapshot.*(missing|incomplete|corrupt|bounds)/);
      const fenced = await exec(session,'fenced','await tools.effect({});');
      assert.equal(fenced.success,false); assert.match(fenced.output,/outcome unknown/);
    }
    const observed = (await request({action:'inspect'})).value;
    assert.equal(observed.effects,1);
    assert.equal(observed.cells.find(cell=>cell.cell_key==='["aggregate","fixture-operation",1,"overflow"]').writes_hash,null);
    assert.equal(observed.blobs.some(blob=>blob.blob_key==='receipt:["aggregate","fixture-operation",1,"overflow"]'),false);
    assert.equal(observed.blobs.some(blob=>blob.blob_key==='session:too-large'||blob.blob_key.startsWith('session:rejected-')),false);
    // Both real guests read 41 from durable overlapping snapshots. Only the
    // admitted increment may commit; the stale increment and its second key
    // remain unknown even after a cold workerd and a newer committed update.
    assert.equal((await exec('concurrent','seed','store("counter",41);')).success,true);
    const staleSource = 'const count=load("counter"); text(await tools.effect({})); store("counter",count+1); store("staleOnly",true); text(count);';
    const admittedSource = 'const count=load("counter"); text(await tools.effect({})); store("counter",count+1); text(count);';
    const concurrent = await request({action:'concurrent',session:'concurrent',staleSource,admittedSource});
    assert.equal(concurrent.status,200);
    assert.equal(concurrent.value.admitted.success,true);
    assert.equal(concurrent.value.stale.success,false);
    assert.match(concurrent.value.stale.output,/version conflict.*outcome unknown/);
    assert.deepEqual(concurrent.value.admitted.output.at(-1),{type:'input_text',text:'41'});
    assert.deepEqual((await request({action:'snapshot',session:'concurrent'})).value,[['counter',42]]);
    await mf.dispose(); mf = start();
    assert.equal((await exec('concurrent','newer','store("counter",load("counter")+1);')).success,true);
    const retainedUnknown = await exec('concurrent','stale',staleSource);
    assert.equal(retainedUnknown.success,false);
    assert.match(retainedUnknown.output,/version conflict.*outcome unknown/);
    assert.deepEqual(await exec('concurrent','admitted',admittedSource),concurrent.value.admitted);
    assert.deepEqual((await request({action:'snapshot',session:'concurrent'})).value,[['counter',43]]);
    const afterConflict = (await request({action:'inspect'})).value;
    assert.equal(afterConflict.effects,3);
    const staleKey = '["concurrent","fixture-operation",1,"stale"]';
    assert.deepEqual(afterConflict.cells.find(cell=>cell.cell_key===staleKey),{cell_key:staleKey,writes_hash:null,expected_version:1});
    assert.equal(afterConflict.blobs.some(blob=>blob.blob_key==='receipt:'+staleKey),false);
    assert.deepEqual(afterConflict.versions.find(row=>row.session_id==='concurrent'),{session_id:'concurrent',version:3});
    assert.equal((await exec('empty','read','text(load("missing")===undefined);')).success,true);
    await mf.dispose(); mf = start();
    assert.deepEqual((await request({action:'snapshot',session:'empty'})).value,[]);
    assert.equal((await exec('empty','read-again','text(load("missing")===undefined);')).success,true);
    console.log('HOSTED_CODE_STATE_TRACE',JSON.stringify({output,trace}));
  } finally {
    await writeFile(output+'/trace.json',JSON.stringify(trace,null,2));
    await mf.dispose();
  }
});
