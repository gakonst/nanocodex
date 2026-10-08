import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import WebSocket from "ws";
import { createTools } from "nanocodex/tools";
import { createAttachment } from "nanocodex-tools/attachment";
import { createNodeProcessTools } from "nanocodex-tools/node";

// Agent-side public provider (exactly as the agent DO builds it, with durable
// call routes) -> real AccountHostedTools /invoke -> broker -> real WebSocket
// -> shipped attachment -> native /bin/sh. Only admission credentials are stubbed.
const root = fileURLToPath(new URL("..", import.meta.url));
const owner = "00000000-0000-4000-8000-000000000021";
const thread = "00000000-0000-7000-8000-000000000022";
const machine = "synthetic-reconnect-agent-hand";
const credential = "Bearer synthetic-reconnect-agent";
const command = "pnpm --filter nanocodex-managed-service test:hand-reconnect-agent";
const source = `
import { DurableObject } from 'cloudflare:workers';
import { AccountHostedTools, AccountHostedToolsProvider, AccountHostedToolsCallRoutes } from './src/account-hosted-tools.ts';
export { AccountHostedTools };
const PROCESS = Symbol.for('nanocodex.processSessionTool');
export class AgentFixture extends DurableObject {
  constructor(ctx, env) { super(ctx, env); this.reset(); }
  reset() {
    this.provider = new AccountHostedToolsProvider(this.env.ACCOUNT, '${owner}', () => true, '${thread}',
      new AccountHostedToolsCallRoutes(this.ctx.storage));
    this.captured = new Map(); this.processes = new Map();
  }
  async fetch(request) {
    const path = new URL(request.url).pathname, body = await request.json();
    if (path === '/agent/reset') { this.reset(); return Response.json({reset:true}); }
    if (path === '/agent/refresh') {
      await this.provider.refresh();
      return Response.json({online:this.provider.machineOnline('${machine}'),
        exec:this.provider.machineTool('${machine}','exec_command')?.routeToken ?? null});
    }
    if (path === '/agent/capture') {
      this.captured.set(body.key, this.provider.machineTool('${machine}','exec_command'));
      return Response.json({route:this.captured.get(body.key)?.routeToken ?? null});
    }
    const context = {sessionId:'${thread}', callId:body.call_id, model:'synthetic-model'};
    let tool;
    if (path === '/agent/exec') tool = body.captured ? this.captured.get(body.captured)
      : (await this.provider.refresh(60_000), this.provider.machineTool('${machine}','exec_command'));
    else if (path === '/agent/poll') tool = this.processes.get(body.process);
    if (!tool) return Response.json({error:'no_tool'}, {status:409});
    const started = Date.now(), result = await tool.handler(body.input, context);
    if (result[PROCESS]) this.processes.set(body.call_id, result[PROCESS]);
    return Response.json({output:result.output, structured:result.structuredResult, success:result.success,
      process_tool:Boolean(result[PROCESS]), wall_ms:Date.now() - started});
  }
}
export default { fetch(request, env) {
  if (request.headers.get('authorization') !== '${credential}') return new Response(null, {status:401});
  const path = new URL(request.url).pathname;
  if (path.startsWith('/agent/')) return env.AGENT.getByName('agent').fetch(request);
  return env.ACCOUNT.getByName('${owner}').fetch(request);
} };
`;

const FORBIDDEN_REPAIR = /\b(restart|repair|reconnect (it|this|the)|reconnect or)\b/i;

test("agent Hand calls survive reconnect and runtime replacement without redispatch or Hand-repair instructions", {timeout:90_000}, async () => {
  const output = join(root, "../../output/hand-reconnect-agent-journey", `${Date.now()}-${process.pid}`);
  const workspace = join(output, "hand");
  await mkdir(workspace, {recursive:true});
  const wire = [], trace = [], runtime = [], sockets = [], connectors = [], natives = [];
  const result = {command, inputs:{owner, thread, machine, shell:"/bin/sh"}, expected:{}, observed:{}};
  let failure, mf;
  const phase = (name, value = {}) => { trace.push({phase:name, at:Date.now(), ...value}); };
  const gates = {};
  try {
    const bundle = await build({stdin:{contents:source, resolveDir:root}, bundle:true, write:false,
      format:"esm", platform:"node", target:"es2022", conditions:["workerd"],
      banner:{js:'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");'},
      alias:{"node-rsa":root + "/node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs"},
      external:["cloudflare:*", "node:*"], logLevel:"warning"});
    await writeFile(join(output, "fixture-source.mjs"), source);
    await writeFile(join(output, "worker.mjs"), bundle.outputFiles[0].text);
    mf = new Miniflare({port:0, modules:true, script:bundle.outputFiles[0].text,
      compatibilityDate:"2026-07-30", compatibilityFlags:["nodejs_compat", "enable_request_signal"],
      durableObjects:{ACCOUNT:{className:"AccountHostedTools", useSQLite:true}, AGENT:{className:"AgentFixture", useSQLite:true}},
      durableObjectsPersist:join(output, "sqlite"),
      handleRuntimeStdio(stdout, stderr) { stdout.on("data", c => runtime.push(String(c))); stderr.on("data", c => runtime.push(String(c))); }});
    const base = await mf.ready;
    const post = async (path, body = {}) => {
      const response = await fetch(new URL(path, base), {method:"POST", body:JSON.stringify(body), signal:AbortSignal.timeout(30_000),
        headers:{authorization:credential, "x-nanocodex-owner-id":owner, "content-type":"application/json"}});
      const value = await response.json(); assert.equal(response.status, 200, JSON.stringify({path, value})); return value;
    };
    const exec = (call_id, cmd, extra = {}) => post("/agent/exec", {call_id, input:{cmd, shell:"/bin/sh", login:false, yield_time_ms:extra.yield ?? 10_000}, ...extra});
    const file = async name => { try { return await readFile(join(workspace, name), "utf8"); } catch (error) { if (error.code !== "ENOENT") throw error; } };
    const waitFor = async (predicate, description, ms = 12_000) => {
      const deadline = performance.now() + ms;
      while (performance.now() < deadline) { const value = await predicate(); if (value) return value; await delay(20); }
      assert.fail(`${description}: ${JSON.stringify(wire.slice(-20))}`);
    };
    // Call frames actually delivered to a publisher, matched by command marker.
    const frames = (label, marker) => wire.filter(row => row.publisher === label && row.direction === "broker"
      && row.frame.type === "call" && (!marker || JSON.stringify(row.frame.input).includes(marker)));
    const endpoint = new URL("/tool-host", base); endpoint.protocol = "ws:";
    const publish = async label => {
      const native = await createNodeProcessTools({workspace}); natives.push(native);
      const tools = await createTools({tools:native.tools});
      const connector = createAttachment(tools, {endpoint:endpoint.href, transport:{async connect() {
        if (gates[label] === "blocked") throw new Error("synthetic network partition");
        if (gates[label]) await gates[label];
        const socket = new WebSocket(endpoint, {headers:{authorization:credential, "x-nanocodex-owner-id":owner}});
        sockets.push({label, socket}); wire.push({publisher:label, event:"connect", at:Date.now()});
        socket.on("message", data => wire.push({publisher:label, direction:"broker", at:Date.now(), frame:JSON.parse(String(data))}));
        socket.on("close", (code, reason) => wire.push({publisher:label, event:"close", at:Date.now(), code, reason:String(reason)}));
        return socket;
      }}}, {machines:[{id:machine, name:"Synthetic reconnect Hand", workspace, capabilities:["shell"]}],
        attachmentId:machine, heartbeatMs:200, reconnectDelayMs:20, drainTimeoutMs:500});
      connectors.push(connector);
      const client = await connector.connect();
      return {client, connector};
    };
    const drop = label => { for (const row of sockets) if (row.label === label) row.socket.terminate(); };
    const holdReconnect = label => { let release; gates[label] = new Promise(r => { release = r; }); return () => { gates[label] = undefined; release(); }; };
    const noRepair = (value, label) => assert.doesNotMatch(String(value.output), FORBIDDEN_REPAIR, `${label} must not instruct Hand management`);

    // 1. Living Hand transport loss: an in-flight command delivers a late
    // receipt and a new command waits for the same runtime epoch.
    const A = await publish("A");
    const before = await post("/agent/refresh");
    assert.equal(before.online, true); assert.ok(before.exec);
    await post("/agent/capture", {key:"stale"});
    const late = exec("late", "printf L >> late.log; while [ ! -f release-late ]; do sleep 0.02; done; printf LATE_OK");
    await waitFor(async () => await file("late.log") === "L", "late command started");
    const release = holdReconnect("A"); drop("A");
    await waitFor(() => !A.client.connected, "A detached");
    const gapStarted = Date.now();
    const during = exec("during-gap", "printf G >> gap.log; printf GAP_OK");
    await writeFile(join(workspace, "release-late"), "x");
    await delay(1_500);
    assert.equal(await file("gap.log"), undefined, "nothing is dispatched while disconnected");
    release();
    const [lateResult, gapResult] = await Promise.all([late, during]);
    assert.equal(lateResult.success, true); assert.equal(lateResult.structured.output, "LATE_OK");
    assert.equal(gapResult.success, true, JSON.stringify(gapResult)); assert.equal(gapResult.structured.output, "GAP_OK");
    assert.ok(Date.now() - gapStarted >= 1_500);
    assert.equal(await file("late.log"), "L"); assert.equal(await file("gap.log"), "G");
    assert.equal(frames("A", "late.log").length, 1); assert.equal(frames("A", "gap.log").length, 1);
    const resumed = await post("/agent/refresh");
    assert.equal(resumed.exec, before.exec, "living reconnect keeps the same route epoch");
    const lateReplay = await exec("late", "printf L >> late.log; while [ ! -f release-late ]; do sleep 0.02; done; printf LATE_OK");
    assert.equal(lateReplay.structured.output, "LATE_OK"); assert.equal(frames("A", "late.log").length, 1);
    phase("living_reconnect", {lateResult, gapResult, lateReplay, gap_wall_ms:gapResult.wall_ms});

    // 2. Completed mutation and a process-owning command on A whose responses
    // are lost (agent runtime resets before persisting them), plus one kept.
    const completedCmd = "printf C >> completed.log; printf COMPLETED_OK";
    await exec("completed-lost", completedCmd);
    const kept = await exec("process-kept", "printf K >> kept.log; sleep 30", {yield:300});
    assert.equal(kept.process_tool, true, JSON.stringify(kept));
    const lostProcessCmd = "printf P >> process-lost.log; sleep 30";
    const lostProcess = await exec("process-lost", lostProcessCmd, {yield:300});
    assert.equal(lostProcess.process_tool, true);

    // 3. Runtime replacement: A is partitioned forever; B is a new runtime of
    // the same physical machine id with a new route generation.
    gates.A = "blocked"; drop("A");
    await waitFor(() => !A.client.connected, "A partitioned");
    const B = await publish("B");
    // Observe the account directly so the agent provider's snapshot stays stale.
    const accountRoute = async () => (await post("/snapshot", {owner_id:owner})).machines
      .find(entry => entry.machine.id === machine && entry.online)?.tools.find(tool => tool.name === "exec_command")?.route_token;
    const replacedRoute = await waitFor(async () => { const r = await accountRoute(); return r && r !== before.exec ? r : undefined; }, "B published");
    phase("runtime_replaced", {before:before.exec, after:replacedRoute});

    // 4. New commands through a stale captured handle: explicit never-admitted
    // ledger evidence moves them once to B; concurrent stale callers share refresh.
    // The provider snapshot and the captured handle still name A's route: all
    // four stale callers race the same 409 and share one refresh.
    const [single, ...concurrent] = await Promise.all([
      exec("stale-single", "printf S >> stale-single.log; printf STALE_SINGLE_OK", {captured:"stale"}),
      ...["c1", "c2", "c3"].map(id => exec(`concurrent-${id}`, `printf ${id} >> concurrent-${id}.log; printf CONCURRENT_${id}_OK`, {captured:"stale"}))]);
    assert.equal(single.success, true, JSON.stringify(single)); assert.equal(single.structured.output, "STALE_SINGLE_OK");
    for (const [index, id] of ["c1", "c2", "c3"].entries()) {
      assert.equal(concurrent[index].success, true, JSON.stringify(concurrent[index]));
      assert.equal(concurrent[index].structured.output, `CONCURRENT_${id}_OK`);
      assert.equal(await file(`concurrent-${id}.log`), id);
      assert.equal(frames("B", `concurrent-${id}.log`).length, 1); assert.equal(frames("A", `concurrent-${id}.log`).length, 0);
    }
    // A poll through A's retained process route must never reach B.
    const poll = await post("/agent/poll", {process:"process-kept", call_id:"poll-kept",
      input:{session_id:kept.structured.session_id, chars:"", yield_time_ms:200}});
    assert.equal(poll.success, false, JSON.stringify(poll)); assert.equal(poll.structured.status, "unavailable");
    assert.equal(poll.structured.reason, "process_runtime_replaced"); noRepair(poll, "process poll");
    await post("/agent/reset"); // Fresh agent runtime: durable call routes survive, captured handles do not.
    assert.equal(await file("stale-single.log"), "S");
    assert.equal(frames("B", "stale-single.log").length, 1); assert.equal(frames("A", "stale-single.log").length, 0);
    phase("stale_handles", {single, concurrent});

    // 5. Replays after agent reset with routes pinned to A's old generation.
    const completedReplay = await exec("completed-lost", completedCmd);
    assert.equal(completedReplay.success, true, JSON.stringify(completedReplay));
    assert.equal(completedReplay.structured.output, "COMPLETED_OK", "ledger receipt, not re-execution");
    assert.equal(await file("completed.log"), "C"); assert.equal(frames("A", "completed.log").length, 1);
    assert.equal(frames("B", "completed.log").length, 0);
    const processReplay = await exec("process-lost", lostProcessCmd, {yield:300});
    assert.equal(processReplay.success, false); assert.equal(processReplay.structured.status, "ambiguous");
    assert.equal(processReplay.process_tool, false, "old process ownership never pins to the replacement runtime");
    assert.equal(frames("B", "process-lost.log").length, 0); assert.equal(await file("process-lost.log"), "P");
    noRepair(processReplay, "process replay");
    const singleReplay = await exec("stale-single", "printf S >> stale-single.log; printf STALE_SINGLE_OK");
    assert.equal(singleReplay.structured.output, "STALE_SINGLE_OK"); assert.equal(frames("B", "stale-single.log").length, 1);
    phase("replays", {completedReplay, processReplay, singleReplay});

    // 6. Process ownership stayed with A.
    const stdinFrames = () => wire.filter(row => row.publisher === "B" && row.direction === "broker"
      && row.frame.type === "call" && row.frame.name === "write_stdin").length;
    assert.equal(stdinFrames(), 0, "no poll or stdin reached the replacement runtime");
    phase("process_poll", {poll});

    // 7. B partitioned: a new command waits the bounded window, then reports
    // machine-readable unavailable state without Hand-management instructions.
    gates.B = "blocked"; drop("B");
    await waitFor(() => !B.client.connected, "B partitioned");
    const timedOut = await exec("after-partition", "printf X >> partition.log; printf NEVER");
    assert.equal(timedOut.success, false); assert.equal(timedOut.structured.status, "unavailable");
    assert.equal(timedOut.structured.admitted, false); assert.equal(timedOut.structured.resent, false);
    assert.equal(timedOut.structured.reason, "route_unavailable_after_recovery");
    noRepair(timedOut, "partition timeout");
    assert.ok(timedOut.wall_ms >= 9_000, `bounded admission wait observed: ${timedOut.wall_ms}ms`);
    assert.ok(timedOut.wall_ms < 20_000);
    assert.equal(await file("partition.log"), undefined);
    phase("partition_timeout", {timedOut});
    result.observed = {gap_wall_ms:gapResult.wall_ms, same_route_after_reconnect:true, replaced_route:replacedRoute !== before.exec,
      late:lateResult.structured.output, completed_replay:completedReplay.structured.output, process_replay:processReplay.structured.status,
      partition:timedOut.structured, partition_wall_ms:timedOut.wall_ms,
      effects:{late:await file("late.log"), gap:await file("gap.log"), completed:await file("completed.log"), process:await file("process-lost.log")}};
    console.log(JSON.stringify({evidence:output, ...result.observed}));
  } catch (error) { failure = error; result.error = error.stack; throw error; }
  finally {
    for (const connector of connectors) { try { await Promise.race([connector.close(), delay(2_000)]); } catch {} }
    for (const row of sockets) row.socket.terminate();
    for (const native of natives) { try { await native.close(); } catch {} }
    try { await mf?.dispose(); } catch {}
    await writeFile(join(output, "trace.json"), JSON.stringify({result, trace}, null, 2) + "\n");
    await writeFile(join(output, "wire.json"), JSON.stringify(wire, null, 2) + "\n");
    await writeFile(join(output, "runtime.log"), runtime.join(""));
    await writeFile(join(output, "README.md"), `Run: \`${command}\`\n\nObserved: ${JSON.stringify(result.observed)}\n\nStatus: ${failure ? "FAIL: " + failure.message : "PASS"}\n\nEvidence: trace.json, wire.json (real WebSocket frames per publisher), runtime.log, hand/*.log (native effects), worker.mjs.\n`);
  }
});
