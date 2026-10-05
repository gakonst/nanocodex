import assert from 'node:assert/strict';
import { test } from 'node:test';
import { fork } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { build } from 'esbuild';

// Reproduce with the current generated WASM package:
// node --test js/managed/test/durable-children-scheduler-journey.test.mjs

// Real production managed HTTP admission, DurableAgentSession.alarm(), Worker
// SQLite/R2, hosted child factory and Rust WASM. Only the external account
// discovery/credential/model service is synthetic. Fixture routes bootstrap
// identity/configuration, observe state and deliver the platform alarm; they
// never create child registry rows, mark turns terminal or invoke child APIs.
const source = `
import { DurableObject } from 'cloudflare:workers';
import { DurableAgentSession } from './src/index.ts';
import { createBrainBucket } from './src/brain-bucket.ts';
import { createBrainWorkspace } from './src/brain-workspace.ts';
export class FixtureSession extends DurableAgentSession {
  async fetch(request) {
    const path = new URL(request.url).pathname;
    if(path === '/__seed') {
      this.ctx.storage.sql.exec("INSERT INTO session_state (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES (1,'00000000-0000-7000-8000-000000000001','00000000-0000-4000-8000-000000000005','00000000-0000-4000-8000-000000000003','00000000-0000-4000-8000-000000000004',1,'https://nanocodex.example/','managed',?)",Date.now());
      this.ctx.storage.sql.exec('INSERT INTO managed_configuration VALUES (1, ?)',JSON.stringify({tools:['exec_command'],environment:{files:[],skills:[],setup_commands:[],network:{access:'disabled'}}}));
      this.ctx.storage.sql.exec("UPDATE managed_agent_settings SET model='gpt-6.1-sol',thinking='low'");
      await this.ctx.storage.sync(); return new Response(null,{status:204});
    }
    if(path === '/__alarm') {
      // A delivered platform alarm is consumed before the alarm handler runs.
      await this.ctx.storage.deleteAlarm(); await this.alarm();
      return new Response(null,{status:204});
    }
    if(path === '/__inspect') return Response.json({
      alarm:await this.ctx.storage.getAlarm(),
      recovery:this.ctx.storage.sql.exec('SELECT * FROM managed_child_recovery').toArray(),
      turns:this.ctx.storage.sql.exec('SELECT id,state,error,attempt_count FROM managed_turns ORDER BY created_at').toArray(),
      effects:this.ctx.storage.sql.exec('SELECT name,state,session_id,call_id FROM managed_code_effects ORDER BY created_at').toArray(),
    });
    if(path === '/__proof') {
      const workspace=createBrainWorkspace(createBrainBucket(this.ctx.storage,this.env.NANOCODEX_WORKSPACES,'00000000-0000-7000-8000-000000000001'),'00000000-0000-7000-8000-000000000001');
      return Response.json({text:new TextDecoder().decode(await workspace.readFile('/brain/background-proof.txt'))});
    }
    return super.fetch(request);
  }
}
export class FixtureModel extends DurableObject {
  constructor(ctx,env){super(ctx,env);ctx.storage.sql.exec('CREATE TABLE IF NOT EXISTS provider_state (singleton INTEGER PRIMARY KEY,released INTEGER);INSERT OR IGNORE INTO provider_state VALUES(1,0);CREATE TABLE IF NOT EXISTS provider_requests (seq INTEGER PRIMARY KEY AUTOINCREMENT,child INTEGER,body TEXT)');}
  async fetch(request) {
    const path=new URL(request.url).pathname;
    if(path==='/__release'){this.ctx.storage.sql.exec('UPDATE provider_state SET released=1');await this.ctx.storage.sync();return new Response(null,{status:204});}
    if(path==='/__inspect')return Response.json({released:this.ctx.storage.sql.exec('SELECT released FROM provider_state').one().released,requests:this.ctx.storage.sql.exec('SELECT seq,child,body FROM provider_requests ORDER BY seq').toArray()});
    if(request.headers.get('upgrade')!=='websocket')return Response.json({tools:[],machines:[],connections:[]});
    const [client,server]=Object.values(new WebSocketPair());server.accept();
    let history=[];
    server.addEventListener('message',event=>{
      const body=JSON.parse(event.data);
      // Responses WebSocket continuation requests contain only the new delta.
      // Emulate provider history so a child stays a child after its tool result.
      history=body.previous_response_id?[...history,...(body.input??[])]:body.input??[];
      const input=history;
      const users=JSON.stringify(input.filter(item=>item.role==='user'));
      const child=users.includes('BACKGROUND_SCHEDULER_CHILD');
      this.ctx.storage.sql.exec('INSERT INTO provider_requests(child,body) VALUES (?,?)',child?1:0,JSON.stringify(body));
      if(child&&!this.ctx.storage.sql.exec('SELECT released FROM provider_state').one().released)return;
      const results=input.filter(item=>item.type==='function_call_output');
      const call=(name,id,args)=>[{type:'function_call',name,call_id:id,arguments:JSON.stringify(args)}];
      const say=text=>[{type:'message',role:'assistant',content:[{type:'output_text',text}]}];
      let output;
      if(child){
        if(!results.some(item=>item.call_id==='background-proof'))output=call('exec_command','background-proof',{cmd:"printf 'BACKGROUND_EFFECT\\n' >> /brain/background-proof.txt",workdir:'/brain'});
        else if(!results.some(item=>item.call_id==='background-submit'))output=call('submit_result','background-submit',{output:'BACKGROUND_CHILD_OK'});
        else output=say('BACKGROUND_CHILD_OK');
      } else if(users.includes('INSPECT_BACKGROUND_CHILD')) {
        const result=results.find(item=>item.call_id==='inspect-children');
        output=result?say(result.output):call('list_agents','inspect-children',{include_completed:true,include_self:false});
      } else output=results.some(item=>item.call_id==='background-spawn')?say('ROOT_TERMINAL_WITH_BACKGROUND_PENDING'):call('spawn_agent','background-spawn',{role:'Background proof',task:'BACKGROUND_SCHEDULER_CHILD: append proof once, submit BACKGROUND_CHILD_OK, then finish.',harness:null,model:null,thinking:'low',lifetime:'background',output_contract:{kind:'string'}});
      server.send(JSON.stringify({type:'response.completed',response:{id:'scheduler-'+crypto.randomUUID(),status:'completed',output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
    });
    return new Response(null,{status:101,webSocket:client});
  }
}
export default {fetch(request,env){const path=new URL(request.url).pathname;if(path.startsWith('/model/'))return env.MODEL.getByName('provider').fetch(new Request('https://fixture.internal/'+path.slice(7),request));return env.NANOCODEX_SESSIONS.getByName('scheduler').fetch(request);}};
`;

test('production managed alarm cold-reopens a background child after root terminal, then stops completed-child recovery', { timeout: 120_000 }, async () => {
  const root = fileURLToPath(new URL('..', import.meta.url));
  const output = fileURLToPath(new URL('../../../output/durable-children-scheduler/', import.meta.url)) + crypto.randomUUID();
  await mkdir(output, { recursive: true });
  const assets = [];
  const bundle = await build({
    stdin: { contents: source, resolveDir: root }, bundle: true, write: false,
    format: 'esm', target: 'es2022', platform: 'node', conditions: ['workerd'],
    banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
    external: ['cloudflare:*', 'node:*'],
    alias: { 'node-rsa': root + '/node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs' },
    plugins: [{ name: 'real-wasm', setup(build) {
      build.onResolve({ filter: /\.wasm$/ }, async args => {
        const path = fileURLToPath(new URL(args.path, 'file://' + args.resolveDir + '/'));
        const name = './fixture-' + assets.length + '.wasm';
        const asset = { name, contents: undefined };
        assets.push(asset);
        asset.contents = await readFile(path);
        return { path: name, external: true };
      });
    } }],
  });
  assert.ok(assets.length > 0, 'the journey bundles the actual WASM transport');
  let code = bundle.outputFiles[0].text;
  if (process.env.CHILD_DEBUG) code = code
    .replaceAll('const payload = event?.payload;', 'const payload = event?.payload; console.log("CHILD_ID_EVENT",event?.type,payload);')
    .replace('async resolve(sessionId, parentCallId, turnId, signal) {', 'async resolve(sessionId, parentCallId, turnId, signal) { console.log("CHILD_ID_RESOLVE", sessionId,parentCallId,turnId);')
    .replace('const effectContext = {', 'console.log("CHILD_EFFECT_IDENTITY_READY"); const effectContext = {')
    .replace('decision = await journal.begin(effectContext);', 'console.log("CHILD_JOURNAL_BEGIN"); decision = await journal.begin(effectContext); console.log("CHILD_JOURNAL_READY",decision);');
  await writeFile(output + '/worker.mjs', code);
  for (const asset of assets) await writeFile(output + '/' + asset.name, asset.contents);
  await writeFile(output + '/assets.json', JSON.stringify(assets.map(asset => asset.name)));
  const evidence = {
    command: 'node --test js/managed/test/durable-children-scheduler-journey.test.mjs',
    node: process.version,
    wasm: assets.map(asset => ({ name: asset.name, sha256: createHash('sha256').update(asset.contents).digest('hex') })),
  };
  let processHandle, sequence = 0, lastInspection, logWrites = Promise.resolve();
  const trace = [];
  const appendLog = (name, chunk) => { logWrites = logWrites.then(() => writeFile(output + '/' + name, chunk, { flag: 'a' })); };
  const summarize = inspection => ({
    session: inspection?.session,
    model: inspection?.model && {
      released: inspection.model.released,
      requests: inspection.model.requests.map(row => ({ seq: row.seq, child: row.child })),
    },
  });
  const start = () => new Promise((resolve, reject) => {
    const current = fork(fileURLToPath(new URL('./fixtures/durable-children-scheduler-process.mjs', import.meta.url)), [output], {
      detached: true, stdio: ['ignore', 'pipe', 'pipe', 'ipc'],
    });
    processHandle = current;
    const timer = setTimeout(() => reject(Error('fixture startup timeout; inspect ' + output)), 20_000);
    const startupExit = (code, signal) => { clearTimeout(timer); reject(Error('fixture exited during startup: ' + JSON.stringify({ code, signal }))); };
    current.stdout.on('data', chunk => appendLog('stdout.log', chunk));
    current.stderr.on('data', chunk => appendLog('stderr.log', chunk));
    current.once('error', error => { clearTimeout(timer); reject(error); });
    current.once('exit', startupExit);
    current.once('message', message => {
      clearTimeout(timer); current.off('exit', startupExit);
      if (!message.ready) return reject(Error(JSON.stringify(message)));
      trace.push({ process_started: current.pid });
      resolve();
    });
  });
  const call = (path, method = 'GET', body) => new Promise((resolve, reject) => {
    const current = processHandle, request = ++sequence;
    const cleanup = () => { clearTimeout(timer); current.off('message', receive); current.off('exit', exited); };
    const receive = message => {
      if (message.request !== request) return;
      cleanup(); message.error ? reject(Error(message.error)) : resolve(message.value);
    };
    const exited = (code, signal) => { cleanup(); reject(Error('fixture exited during ' + path + ': ' + JSON.stringify({ code, signal }))); };
    const timer = setTimeout(() => { cleanup(); reject(Error('fixture request timeout: ' + path + '; inspect ' + output)); }, 20_000);
    current.on('message', receive); current.once('exit', exited);
    current.send({ request, path, method, body }, error => { if (error) { cleanup(); reject(error); } });
  });
  const inspect = async () => {
    lastInspection = { session: await call('/__inspect'), model: await call('/model/__inspect') };
    return lastInspection;
  };
  const poll = async predicate => {
    const deadline = Date.now() + 50_000;
    for (;;) {
      const value = await predicate();
      if (value) return value;
      if (Date.now() > deadline) throw Error('scheduler state timeout: ' + JSON.stringify(summarize(lastInspection)) + '; inspect ' + output);
      await new Promise(resolve => setTimeout(resolve, 30));
    }
  };
  const kill = () => new Promise((resolve, reject) => {
    const current = processHandle;
    if (current.exitCode !== null || current.signalCode !== null) { processHandle = undefined; resolve(); return; }
    current.once('exit', (code, signal) => {
      trace.push({ process_killed: current.pid, code, signal });
      processHandle = undefined; resolve();
    });
    try { process.kill(-current.pid, 'SIGKILL'); } catch (error) { reject(error); }
  });
  try {
    await start();
    await call('/__seed', 'POST');
    const admitted = await call('/turns', 'POST', { id: 'root-turn', input: 'Spawn a background proof worker and finish immediately.' });
    trace.push({ admitted });
    const terminal = await poll(async () => {
      const value = await inspect();
      return value.session.turns[0]?.state === 'completed' && value.model.requests.some(row => row.child === 1) ? value : undefined;
    });
    const receipt = await call('/turns/root-turn');
    assert.equal(receipt.state, 'completed');
    assert.equal(receipt.terminal.final_message, 'ROOT_TERMINAL_WITH_BACKGROUND_PENDING');
    assert.equal(terminal.session.recovery[0]?.pending, 1);
    assert.ok(terminal.session.alarm > Date.now());
    assert.equal(terminal.session.effects.filter(row => row.call_id === 'background-proof').length, 0);
    const spawnResult = terminal.model.requests.flatMap(row => JSON.parse(row.body).input ?? []).find(item => item.call_id === 'background-spawn' && item.type === 'function_call_output');
    const childId = JSON.parse(spawnResult.output).agent_id;
    trace.push({ before_process_death: terminal, root_receipt: receipt, child_id: childId });
    await kill();

    await start();
    await call('/model/__release', 'POST');
    await call('/__alarm', 'POST');
    const resumed = await poll(async () => {
      const value = await inspect();
      return value.session.recovery[0]?.pending === 0 && value.session.effects.some(row => row.call_id === 'background-proof' && row.state === 'completed') ? value : undefined;
    });
    assert.equal(resumed.session.turns[0].state, 'completed');
    assert.equal(resumed.session.turns[0].attempt_count, terminal.session.turns[0].attempt_count, 'scheduler must not re-admit terminal root');
    assert.equal(resumed.model.requests.filter(row => row.child === 0).length, terminal.model.requests.filter(row => row.child === 0).length, 'terminal root does not infer again');
    assert.ok(resumed.model.requests.filter(row => row.child === 1).length > terminal.model.requests.filter(row => row.child === 1).length, 'cold child uses a new real WASM transport');
    assert.equal((await call('/__proof')).text, 'BACKGROUND_EFFECT\n');
    trace.push({ alarm_cold_reopen: resumed, proof: await call('/__proof') });

    await call('/turns', 'POST', { id: 'inspect-turn', input: 'INSPECT_BACKGROUND_CHILD: list retained completed children.' });
    await poll(async () => {
      const value = await inspect();
      return value.session.turns.find(row => row.id === 'inspect-turn')?.state === 'completed' ? value : undefined;
    });
    const result = await call('/turns/inspect-turn');
    const children = JSON.parse(result.terminal.final_message).agents;
    assert.equal(children.length, 1);
    assert.equal(children[0].agent_id, childId, 'the public listing retains the child identity across process death');
    assert.equal(children[0].lifetime, 'background');
    assert.deepEqual(children[0].status, { state: 'completed', output: 'BACKGROUND_CHILD_OK' });
    trace.push({ public_child_result: result });

    // Expire the actual configured idle timeout, then deliver platform alarms.
    // Completed reusable children must stop child recovery. Independent history
    // projection/archival work may still have a platform alarm.
    await new Promise(resolve => setTimeout(resolve, 1_100));
    await call('/__alarm', 'POST');
    const idle = await poll(async () => {
      const value = await inspect();
      return value.session.recovery[0]?.pending === 0 ? value : undefined;
    });
    const requestCount = idle.model.requests.length;
    await call('/__alarm', 'POST');
    await call('/__alarm', 'POST');
    const settled = await inspect();
    assert.equal(settled.session.recovery[0].pending, 0);
    assert.equal(settled.model.requests.length, requestCount);
    assert.equal(settled.session.effects.filter(row => row.call_id === 'background-proof').length, 1);
    assert.equal((await call('/__proof')).text, 'BACKGROUND_EFFECT\n');
    trace.push({ completed_children_do_not_poll: settled });
    console.log(JSON.stringify({ evidence: output, root: 'completed before SIGKILL', child: 'alarm cold reopen / BACKGROUND_CHILD_OK', childId, effects: 1, terminalRootReinferences: 0, completedChildRecovery: false }));
  } finally {
    if (processHandle) await kill();
    await logWrites;
    await writeFile(output + '/trace.json', JSON.stringify({ ...evidence, trace, lastInspection }, null, 2) + '\n');
  }
});
