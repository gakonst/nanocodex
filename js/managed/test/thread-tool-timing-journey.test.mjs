import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import { createTools } from "../../nanocodex/tools/Tools.mjs";
import { createNodeProcessTools } from "../../nanocodex-tools/tools/nodeProcess.mjs";

// The actual managed Session/account Durable Objects, WASM, Code Mode, SQLite,
// reverse Hand WebSocket publisher and native shell run here. The fixture seeds
// a synthetic account and replaces only the external model/authentication.
const threadId = "00000000-0000-7000-8000-000000000001";
const owner = "00000000-0000-4000-8000-000000000003";
const org = "00000000-0000-7000-8000-000000000004";
const team = "00000000-0000-7000-8000-000000000005";
const root = fileURLToPath(new URL("..", import.meta.url));
const output = join(root, "../../output/thread-tool-timing-journey", `${Date.now()}-${process.pid}`);
const source = `
import { DurableObject } from 'cloudflare:workers';
import { DurableAgentSession, AccountHostedTools } from './src/index.ts';
export { AccountHostedTools };
const info = console.info.bind(console);
console.info = (record, ...rest) => info(record && typeof record === 'object' ? JSON.stringify(record) : record, ...rest);
export class FixtureSession extends DurableAgentSession {
  async fetch(request) {
    if (new URL(request.url).pathname === '/__seed') {
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO session_state (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES (1,?,?,?,?,1,'https://fixture.internal/','managed',?)", '${threadId}', '${owner}', '${org}', '${team}', Date.now());
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO managed_configuration VALUES (1, ?)", JSON.stringify({environment:{files:[],skills:[],setup_commands:[],network:{access:'enabled'}}}));
      this.ctx.storage.sql.exec("UPDATE managed_agent_settings SET model='gpt-6.1-sol',thinking='low'");
      await this.ctx.storage.sync(); return new Response(null,{status:204});
    }
    return super.fetch(request);
  }
}
const codeCall = (name, callId, args) => ({type:'custom_tool_call',name:'exec',call_id:callId,input:'text(await tools.'+name+'('+JSON.stringify(args)+'));'});
export class FixtureModel extends DurableObject {
  async fetch(request) {
    if (request.headers.get('upgrade') !== 'websocket') return Response.json({tools:[],machines:[],connections:[]});
    const [client, server] = Object.values(new WebSocketPair()); server.accept(); let index=0,child=false,scenario='',scenarioIndex=0;
    server.addEventListener('close', () => server.close(1000));
    server.addEventListener('message', event => {
      const body=JSON.parse(event.data);
      child ||= JSON.stringify((body.input??[]).filter(item=>item.role==='user')).includes('THREAD_CHILD_FIXTURE');
      const latestUser=JSON.stringify((body.input??[]).filter(item=>item.role==='user').at(-1));
      const nextScenario=['FIXTURE_ERROR_ONLY','FIXTURE_LOST_RESULT','FIXTURE_RECONNECTED'].find(marker=>latestUser?.includes(marker));
      if(!child && nextScenario && nextScenario!==scenario){scenario=nextScenario;index=0;scenarioIndex++;}
      const call=++index;
      const output=child ? call===1 ? [codeCall('exec_command','call_fixture_child_command',{cmd:'printf CHILD_OK',workdir:'/brain'})]
        : call===2 ? [codeCall('submit_result','call_fixture_child_submit',{output:'CHILD_OK'})]
        : [{type:'message',role:'assistant',content:[{type:'output_text',text:'CHILD_OK'}]}]
        : scenario ? call===1 ? [codeCall('exec_command','call_fixture_'+scenario+'_'+crypto.randomUUID(),{cmd:scenario==='FIXTURE_RECONNECTED'?'printf RECONNECTED_OK':
            'printf '+(scenario==='FIXTURE_ERROR_ONLY'?'E':'L')+' >> hand-effects.log; sleep 0.5; printf LOST_EFFECT',workdir:'/fixture-hand',yield_time_ms:1000})]
          : [{type:'message',role:'assistant',content:[{type:'output_text',text:'BOUNDARY_'+scenario+'_OK'}]}]
        : call===1 ? [{type:'custom_tool_call', name:'exec', call_id:'call_fixture_outer', input:
        'text(await tools.exec_command({cmd:"sleep 0.35; printf HAND_OK",workdir:"/fixture-hand",yield_time_ms:1000})); '+
        'text(await tools.exec_command({cmd:"printf BRAIN_OK",workdir:"/brain"})); '+
        'text(await tools.exec_command({cmd:"printf SHOULD_NOT_RUN",workdir:"/fixture-hand",tty:true}));'}]
        : call===2 ? [codeCall('spawn_agent','call_fixture_spawn',{role:'Fixture child',task:'THREAD_CHILD_FIXTURE',model:'sol',thinking:'low',output_contract:{kind:'string'}})]
        : call===3 ? [codeCall('wait_agent','call_fixture_wait',{agent_ids:[1],timeout_ms:10000})]
        : [{type:'message',role:'assistant',content:[{type:'output_text',text:'TOOL_TIMELINE_OK'}]}];
      server.send(JSON.stringify({type:'response.completed',response:{id:'resp_fixture_'+(child?'child_':'root_')+(scenario?scenarioIndex+'_':'')+call,status:'completed',end_turn:child?call>2:scenario?call>1:call>3,output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
    });
    return new Response(null,{status:101,webSocket:client});
  }
}
export default {fetch(request,env) {
  const path=new URL(request.url).pathname;
  if(path.startsWith('/account-tools/')) return env.NANOCODEX_ACCOUNT_TOOLS.getByName('${owner}').fetch(new Request('https://account-tools.internal/'+path.slice('/account-tools/'.length),request));
  if(path==='/tool-host') return env.NANOCODEX_ACCOUNT_TOOLS.getByName('${owner}').fetch(new Request('https://account-tools.internal/tool-host',request));
  const resource=path.replace('/v1/agents/${threadId}', '');
  return env.NANOCODEX_SESSIONS.getByName('fixture-session').fetch(new Request('https://session.internal'+resource+new URL(request.url).search,request));
}};
`;

test("one thread exposes every nested tool and separates Hand execution from roundtrip", { timeout: 60_000 }, async () => {
  await mkdir(join(output, "hand"), { recursive: true });
  const records = [], raw = [], wire = [], frames = [], hostRecords = [], sockets = [];
  let fault, failedConnects = 0, connectAttempts = 0;
  const log = console.info;
  console.info = (record, ...rest) => {
    if (record?.type === "hand.attachment") hostRecords.push(record);
    else log(record, ...rest);
  };
  const capture = line => {
    raw.push(line);
    const offset = line.indexOf('{"type":');
    if (offset >= 0) { try { records.push(JSON.parse(line.slice(offset))); } catch {} }
  };
  const assets = [];
  const bundle = await build({ stdin: { contents: source, resolveDir: root }, bundle: true, write: false,
    format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
    external: ["cloudflare:*", "node:*"], alias: { "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
    plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /\.wasm$/ }, async args => {
      const contents = await readFile(join(args.resolveDir, args.path));
      const name = `fixture-${assets.length}.wasm`;
      assets.push({ type: "CompiledWasm", path: name, contents });
      return { path: `./${name}`, external: true };
    }); } }], logLevel: "silent",
  });
  const proxy = await build({ stdin: { contents: `import {routeManaged} from '../account/worker/managedProxy.ts';
    export default {async fetch(request,env){return await routeManaged(request,env,new URL(request.url)) ?? env.NANOCODEX_BACKEND.fetch(request)}}`, resolveDir: root },
    bundle: true, write: false, format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    external: ["cloudflare:*", "node:*"], logLevel: "silent" });
  const mf = new Miniflare({ port: 0, unsafeLocalExplorer: true, unsafeObservability: true, handleRuntimeStdio(stdout, stderr) {
    createInterface({ input: stdout }).on("line", capture); createInterface({ input: stderr }).on("line", capture);
  }, workers: [
    { name: "account", modules: true, script: proxy.outputFiles[0].text,
      compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat"], serviceBindings: { NANOCODEX_BACKEND: "managed" } },
    { name: "managed", modules: [{ type: "ESModule", path: "worker.mjs", contents: bundle.outputFiles[0].text }, ...assets],
      compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
      bindings: { AGENT_IDLE_TIMEOUT_MS: "1000" },
      durableObjects: { NANOCODEX_SESSIONS: { className: "FixtureSession", useSQLite: true },
        MODEL: { className: "FixtureModel", useSQLite: true }, NANOCODEX_ACCOUNT_TOOLS: { className: "AccountHostedTools", useSQLite: true },
        NANOCODEX_MEMORY: { className: "FixtureModel", useSQLite: true } },
      serviceBindings: { NANOCODEX: "provider" }, r2Buckets: ["NANOCODEX_HISTORY", "NANOCODEX_WORKSPACES"] },
    { name: "provider", modules: true, script: "export default {fetch(request,env){return env.MODEL.getByName('fixture-model').fetch(request)}};",
      compatibilityDate: "2026-07-30", durableObjects: { MODEL: { className: "FixtureModel", scriptName: "managed", useSQLite: true } } },
  ] });
  const native = await createNodeProcessTools({ workspace: join(output, "hand") });
  const tools = await createTools({ tools: native.tools, attachmentId: "fixture-hand", machines: [
    { id: "fixture-hand", name: "Fixture Hand", workspace: join(output, "hand"), capabilities: ["shell"] },
  ] });
  const headers = { "x-nanocodex-owner-id": owner, "x-nanocodex-session-organization-id": org,
    "x-nanocodex-session-team-id": team, "x-nanocodex-authorization-epoch": "1",
    "x-nanocodex-capabilities": JSON.stringify(["agents:read", "agents:write", "tools:use"]), "content-type": "application/json" };
  let attachment;
  try {
    assert.equal((await mf.dispatchFetch("https://fixture.internal/__seed", { method: "POST" })).status, 204);
    attachment = tools.attach({ endpoint: "wss://fixture.internal/tool-host", transport: { async connect() {
      connectAttempts++;
      if (failedConnects > 0) { failedConnects--; throw new Error("synthetic transient connection failure"); }
      const response = await mf.dispatchFetch("https://fixture.internal/tool-host", { headers: { upgrade: "websocket", "x-nanocodex-owner-id": owner } });
      assert.equal(response.status, 101); const socket = response.webSocket; socket.accept();
      const connection = { socket, error: undefined }; sockets.push(connection);
      // Deterministic network delays bracket real shell execution. Their sum
      // must appear as transport overhead, rather than Hand execution time.
      return { get readyState() { return socket.readyState; }, close: (...args) => socket.close(...args),
        addEventListener(type, listener) { if (type === "error") connection.error = listener; socket.addEventListener(type, event => {
          const frame = type === "message" ? JSON.parse(String(event.data)) : undefined;
          if (frame?.type === "call") { frames.push(frame); wire.push({ direction: "call", call_id: frame.call_id }); setTimeout(() => listener(event), 250); }
          else listener(event);
        }); },
        send(data) { const frame = JSON.parse(data);
          if (frame.type === "diagnostic") wire.push({ direction: "diagnostic", ...frame });
          if (fault === "error_only" && frame.type === "diagnostic" && frame.stage === "execution_started") {
            socket.send(data); fault = undefined; failedConnects = 2;
            setTimeout(() => connection.error(new Error("synthetic error without a close event")), 150); return;
          }
          if (fault === "lost_result" && frame.type === "result") {
            fault = undefined; socket.close(1012, "synthetic result transport loss"); return;
          }
          if (frame.type === "result") { wire.push({ direction: "result", call_id: frame.call_id, timing: frame.timing }); setTimeout(() => socket.send(data), 150); }
          else socket.send(data);
        },
      };
    } } });
    await attachment.connect();
    const accepted = await mf.dispatchFetch(`https://fixture.internal/v1/agents/${threadId}/turns`,
      { method: "POST", headers, body: JSON.stringify({ id: "00000000-0000-7000-8000-000000000002", input: "Run the synthetic tool timeline." }) });
    assert.equal(accepted.status, 202, await accepted.clone().text());
    const receipt = await accepted.json(); const turnId = receipt.turn_id;
    let completed;
    const deadline = Date.now() + 30_000;
    while (Date.now() < deadline) {
      const response = await mf.dispatchFetch(`https://fixture.internal/v1/agents/${threadId}/turns/${turnId}`, { headers });
      assert.equal(response.status, 200, await response.clone().text());
      completed = await response.json(); if (["completed", "failed", "cancelled"].includes(completed.state)) break;
      await new Promise(resolve => setTimeout(resolve, 25));
    }
    assert.equal(completed.state, "completed", JSON.stringify(completed));
    const history = await mf.dispatchFetch(`https://fixture.internal/v1/agents/${threadId}/events/history?after=0&limit=256`, { headers });
    assert.equal(history.status, 200, await history.clone().text());
    await writeFile(join(output, "events.json"), JSON.stringify(await history.json(), null, 2) + "\n");
    assert.match(JSON.stringify(completed), /TOOL_TIMELINE_OK/);
    const calls = records.filter(row => row.type === "managed.agent.tool");
    assert.equal(calls.filter(row => row.message_type === "tool.call").length, 12, JSON.stringify(calls));
    assert.equal(calls.filter(row => row.message_type === "tool.result").length, 12);
    assert.ok(calls.every(row => row.thread_id === threadId));
    assert.ok(calls.every(row => row.turn_id === turnId));
    assert.ok(calls.some(row => row.agent_id === 1 && row.tool === "exec_command"));
    assert.ok(calls.some(row => row.agent_id === 1 && row.tool === "submit_result"));
    assert.equal(new Set(calls.filter(row => row.message_type === "tool.call").map(row => row.runtime_session_id)).size, 2);
    assert.ok(calls.some(row => row.parent_call_id === "call_fixture_outer"));
    const toolCalls = calls.filter(row => row.message_type === "tool.call");
    assert.equal(toolCalls.filter(row => row.tool === "exec").length, 5);
    for (const [parent, tool] of [["call_fixture_spawn", "spawn_agent"], ["call_fixture_wait", "wait_agent"],
      ["call_fixture_child_command", "exec_command"], ["call_fixture_child_submit", "submit_result"]]) {
      assert.ok(toolCalls.some(row => row.tool === tool && row.parent_call_id === parent
        && row.tool_call_id === parent + "/code-1"), JSON.stringify(toolCalls));
    }
    assert.ok(calls.some(row => row.outcome === "failure"));
    const broker = records.filter(row => row.type === "hand.call.broker");
    assert.ok(broker.length >= 2, JSON.stringify(records.filter(row => row.type?.startsWith("hand."))));
    assert.ok(broker.every(row => row.thread_id === threadId));
    const timing = broker.find(row => row.host_timing?.execution_ms >= 300);
    assert.ok(timing, JSON.stringify(broker));
    assert.ok(timing.roundtrip_ms >= timing.host_timing.host_elapsed_ms + 350);
    assert.ok(timing.transit_return_overhead_ms >= 350);
    const provider = records.find(row => row.type === "hand.call.provider" && row.source_call_id === timing.source_call_id);
    assert.ok(provider.fetch_ms >= 700);
    assert.ok(provider.decode_ms >= 0);
    assert.ok(provider.total_ms >= provider.fetch_ms + provider.decode_ms);
    const model = records.find(row => row.type === "managed.agent.transport" && row.message_type === "model.call.completed");
    assert.equal(model.thread_id, threadId); assert.equal(typeof model.time_to_first_event_ms, "number");
    assert.ok(model.response_id.startsWith("resp_"));
    // Reconstruct the actual account DO with its retained WebSocket and SQLite
    // ledger, then repeat a completed source ID. No command may execute twice.
    await mf.unsafeEvictDurableObject("managed", "AccountHostedTools", { name: owner, webSockets: "hibernate" });
    const snapshot = await (await mf.dispatchFetch("https://fixture.internal/account-tools/snapshot", {
      method: "POST", body: JSON.stringify({ owner_id: owner }) })).json();
    const machine = snapshot.machines.find(entry => entry.machine.id === "fixture-hand");
    const replay = await mf.dispatchFetch("https://fixture.internal/account-tools/invoke", { method: "POST",
      body: JSON.stringify({ owner_id: owner, name: "exec_command", machine_id: "fixture-hand",
        session_id: timing.session_id, thread_id: threadId, turn_id: frames[0].turn_id,
        call_id: timing.source_call_id, model: frames[0].model, input: frames[0].input,
        route_token: machine.tools.find(tool => tool.name === "exec_command").route_token }) });
    assert.equal(replay.status, 200, await replay.clone().text());
    assert.equal((await replay.json()).structured_result.output, "HAND_OK");
    assert.equal(frames.length, 2, "durable replay must not rerun the shell command");
    assert.ok(records.some(row => row.type === "hand.call.broker" && row.stage === "replay"
      && row.thread_id === threadId && row.transport_call_id === timing.transport_call_id));
    const base = await mf.ready;
    async function query(sql, params = []) {
      const response = await fetch(new URL("/cdn-cgi/explorer/api/local/observability/query", base), {
        method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ sql, params }) });
      assert.equal(response.status, 200, await response.clone().text());
      const capture = await response.json(); assert.equal(capture.success, true, JSON.stringify(capture));
      return capture.result.rows.map(row => Object.fromEntries(capture.result.columns.map((column, index) => [column, row[index]])));
    }
    let spans = [];
    const traceDeadline = Date.now() + 10_000;
    do {
      spans = await query("SELECT trace_id,span_id,parent_id,service,name,duration_ms,json(attributes) AS attributes FROM spans ORDER BY start_ms,span_id");
      if (spans.some(span => span.name === "hand.account.invoke" && span.duration_ms >= 700)) break;
      await new Promise(resolve => setTimeout(resolve, 25));
    } while (Date.now() < traceDeadline);
    await writeFile(join(output, "spans.json"), JSON.stringify(spans, null, 2) + "\n");
    const hostedSpan = spans.find(span => span.name === "hand.account.invoke" && JSON.parse(span.attributes)["nanocodex.tool_call_id"] === timing.source_call_id);
    assert.ok(hostedSpan, JSON.stringify(spans.filter(span => span.name?.startsWith("hand."))));
    assert.ok(hostedSpan.duration_ms >= 700);
    assert.equal(JSON.parse(hostedSpan.attributes)["nanocodex.thread_id"], threadId);
    assert.equal(JSON.parse(hostedSpan.attributes)["nanocodex.hand_id"], "fixture-hand");
    assert.equal(JSON.parse(hostedSpan.attributes)["nanocodex.connection_id"], timing.connection_id);
    assert.equal(JSON.parse(hostedSpan.attributes)["nanocodex.host_connection_id"], timing.host_connection_id);
    assert.equal(JSON.parse(hostedSpan.attributes)["nanocodex.host_turn_id"], frames[0].turn_id);
    assert.notEqual(frames[0].turn_id, calls[0].runtime_turn_id);
    // The pinned DO runtime may finalize the admission tracer when a later
    // request takes over background work. Native /invoke still measures the
    // full callee wait; runtime and invocation records own the complete timeline.
    const invoked = records.filter(row => row.type === "managed.tool.invocation"
      && row.operation === "nanocodex.tool");
    assert.ok(invoked.some(row => row.tool_call_id === timing.source_call_id
      && row.stage === "finished" && row.duration_ms >= 700 && row.thread_id === threadId));
    const ancestors = [];
    let current = hostedSpan;
    while (current.parent_id) {
      current = spans.find(span => span.trace_id === hostedSpan.trace_id && span.span_id === current.parent_id);
      assert.ok(current, "a native span must retain its real parent"); ancestors.push(current);
    }
    assert.ok(ancestors.some(span => span.name === "hand.provider.invoke"));
    assert.ok(ancestors.some(span => span.name === "durable_object_subrequest"));
    assert.ok(ancestors.some(span => span.name === "managed.proxy" && span.service === "account"));
    assert.ok(ancestors.some(span => span.name === "fetch"));
    const traceLogs = await query("SELECT trace_id,span_id,message FROM logs WHERE trace_id = ?", [hostedSpan.trace_id]);
    await writeFile(join(output, "trace-logs.json"), JSON.stringify(traceLogs, null, 2) + "\n");
    assert.ok(traceLogs.some(log => log.span_id === hostedSpan.span_id && log.message.includes("managed.tool.invocation")));
    assert.doesNotMatch(JSON.stringify(calls.concat(broker)), /sleep 0\.35|printf|HAND_OK|BRAIN_OK|\/hand\//);

    // Read the shipped diagnostic API over actual HTTP. Evidence survives the
    // account DO reconstruction above and needs no live tail/native parent.
    const api = (path, init = {}) => fetch(new URL(`/v1/agents/${threadId}${path}`, base), { ...init, headers: { ...headers, ...init.headers } });
    const diagnostics = async () => {
      const response = await api("/diagnostics?limit=1024");
      assert.equal(response.status, 200, await response.clone().text());
      return response.json();
    };
    const initial = await diagnostics();
    assert.ok(initial.services.every(service => service.available), JSON.stringify(initial));
    const handPage = initial.services.find(service => service.service === "hand.broker");
    const progress = handPage.events.filter(event => event.source_call_id === timing.source_call_id && event.stage === "host_progress");
    assert.deepEqual(progress.map(event => event.host_stage), ["received", "execution_started", "execution_finished", "result_prepared"]);
    assert.ok(progress.every(event => event.connection_id && event.lease_id && event.host_runtime_id && event.host_connection_id));
    assert.ok(progress.every(event => event.runtime_generation === event.connection_generation));
    assert.ok(handPage.events.some(event => event.type === "hand.connection" && event.stage === "resumed"));
    assert.equal((await api("/diagnostics?limit=1025")).status, 400);
    assert.equal((await api("/diagnostics", { headers: { "x-nanocodex-capabilities": "[]" } })).status, 403);
    assert.equal((await api("/diagnostics", { headers: { "x-nanocodex-owner-id": "00000000-0000-4000-8000-000000000099" } })).status, 404);
    const page = await (await api("/diagnostics?limit=2")).json();
    const next = await (await api(`/diagnostics?limit=2&after_managed=${page.services[0].next_after}&after_hand=${page.services[1].next_after}`)).json();
    assert.ok(next.services.every((service, index) => service.events.every(event => event.seq > page.services[index].next_after)));

    let nextTurn = 10;
    async function runTurn(marker) {
      const id = `00000000-0000-7000-8000-${String(nextTurn++).padStart(12, "0")}`;
      const response = await api("/turns", { method: "POST", body: JSON.stringify({ id, input: marker }) });
      assert.equal(response.status, 202, await response.clone().text());
      const accepted = await response.json();
      const deadline = Date.now() + 15_000;
      while (Date.now() < deadline) {
        const turn = await (await api(`/turns/${accepted.turn_id}`)).json();
        if (turn.state === "completed") return turn;
        assert.ok(!["failed", "cancelled"].includes(turn.state), JSON.stringify(turn));
        await new Promise(resolve => setTimeout(resolve, 25));
      }
      assert.fail(`turn ${marker} did not complete`);
    }
    async function waitForReady(count) {
      const deadline = Date.now() + 8_000;
      while (Date.now() < deadline) {
        if (hostRecords.filter(record => record.event === "ready").length >= count) return;
        await new Promise(resolve => setTimeout(resolve, 25));
      }
      assert.fail(`Hand did not reconnect: ${JSON.stringify(hostRecords)}`);
    }

    // A transport error that never delivers a close event used to be ignored
    // after ready. Recover a live command after two failed reconnect attempts.
    fault = "error_only";
    await runTurn("FIXTURE_ERROR_ONLY");
    await waitForReady(2);
    assert.ok(connectAttempts >= 4, "two failed reconnect attempts must be followed by a successful attempt");
    await runTurn("FIXTURE_RECONNECTED");
    const afterError = await diagnostics();
    const errorEvents = afterError.services.find(service => service.service === "hand.broker").events;
    const lost = errorEvents.find(event => event.stage === "transport_lost" && event.source_call_id?.includes("FIXTURE_ERROR_ONLY"));
    assert.ok(lost?.reason_code && lost.connection_id && lost.lease_id, JSON.stringify(errorEvents));
    assert.ok(errorEvents.some(event => event.source_call_id === lost.source_call_id && event.stage === "receipt"
      && event.outcome === "completed" && event.transport_call_id === lost.transport_call_id));
    assert.ok(errorEvents.some(event => event.source_call_id?.includes("FIXTURE_RECONNECTED") && event.stage === "receipt"
      && event.runtime_generation === lost.runtime_generation && event.connection_id !== lost.connection_id));

    // Now let the Hand finish its effect and prepare a receipt, then lose that
    // receipt before the broker sees it. The gap must be distinguishable from
    // an executing Hand, and the effect must never be replayed after reconnect.
    fault = "lost_result";
    await runTurn("FIXTURE_LOST_RESULT");
    await waitForReady(3);
    await runTurn("FIXTURE_RECONNECTED");
    let completeDiagnostics = await diagnostics();
    const completeHand = completeDiagnostics.services.find(service => service.service === "hand.broker");
    const lostResult = completeHand.events.find(event => event.stage === "transport_lost" && event.source_call_id?.includes("FIXTURE_LOST_RESULT"));
    assert.ok(lostResult, JSON.stringify(completeHand));
    const lostBoundary = completeHand.events.filter(event => event.source_call_id === lostResult.source_call_id);
    assert.deepEqual(lostBoundary.filter(event => event.stage === "host_progress").map(event => event.host_stage), ["received", "execution_started", "execution_finished", "result_prepared"]);
    assert.ok(lostBoundary.some(event => event.stage === "receipt" && event.outcome === "completed"
      && event.transport_call_id === lostResult.transport_call_id));
    assert.ok(lostBoundary.some(event => event.stage === "terminal" && event.outcome === "completed"));
    assert.ok(completeHand.events.some(event => event.source_call_id?.includes("FIXTURE_RECONNECTED") && event.stage === "receipt"
      && event.runtime_generation === lostResult.runtime_generation && event.connection_id !== lostResult.connection_id));
    assert.equal(frames.length, 6, "each fresh recovery command must actually reach the Hand");
    assert.equal(new Set(completeHand.events.filter(event => event.host_runtime_id).map(event => event.host_runtime_id)).size, 1);
    assert.ok(completeHand.connections.some(connection => connection.hand_id === "fixture-hand" && connection.active && connection.connected
      && connection.runtime_generation === lostResult.runtime_generation && connection.connection_id !== lostResult.connection_id));
    assert.equal(await readFile(join(output, "hand/hand-effects.log"), "utf8"), "EL", "neither recovered effect may execute twice");
    assert.equal(frames.filter(frame => frame.input.cmd?.includes("hand-effects.log")).length, 2);

    // A screen host uses a separate authenticated socket protocol. A send is
    // observable, but this protocol has no execution acknowledgment. Losing a
    // click must expose that uncertainty without silently sending it again.
    const screenResponse = await mf.dispatchFetch("https://fixture.internal/account-tools/hands/host", {
      headers: { ...headers, upgrade: "websocket" },
    });
    assert.equal(screenResponse.status, 101);
    const screenSocket = screenResponse.webSocket;
    const screenWire = [];
    const nextScreen = () => new Promise((resolve, reject) => {
      const timeout = setTimeout(() => { screenSocket.removeEventListener("message", receive); reject(new Error("No screen host frame")); }, 2_000);
      function receive(event) {
        clearTimeout(timeout); screenSocket.removeEventListener("message", receive);
        const frame = JSON.parse(String(event.data)); screenWire.push(frame); resolve(frame);
      }
      screenSocket.addEventListener("message", receive);
    });
    const readyScreen = nextScreen(); screenSocket.accept(); const screenState = await readyScreen;
    const publishedScreen = nextScreen();
    screenSocket.send(JSON.stringify({ type: "catalog", machine_id: "fixture-screen-hand", machine_name: "Private fixture screen name",
      surfaces: [{ id: "desktop", name: "Private desktop title", kind: "desktop", width: 1, height: 1, controllable: true, agent_tools: true }] }));
    await publishedScreen;
    const screenCatalog = await (await mf.dispatchFetch("https://fixture.internal/account-tools/snapshot", {
      method: "POST", body: JSON.stringify({ owner_id: owner }),
    })).json();
    const screenTool = screenCatalog.tools.find(tool => tool.route_token.startsWith("screen:v1:") && tool.route_token.includes("fixture-screen-hand"));
    assert.ok(screenTool);
    const invokeScreen = (callId, input, diagnosticThread = threadId) => mf.dispatchFetch("https://fixture.internal/account-tools/invoke", {
      method: "POST", body: JSON.stringify({ owner_id: owner, name: screenTool.definition.name, route_token: screenTool.route_token,
        session_id: timing.session_id, thread_id: diagnosticThread, call_id: callId, input }),
    });
    const observedScreen = nextScreen(), screenObservation = invokeScreen("screen-observe", { action: "observe" });
    const screenCall = await observedScreen;
    assert.equal(screenCall.type, "agent_call");
    screenSocket.send(JSON.stringify({ type: "agent_result", request_id: screenCall.request_id, status: "ok", jpeg: "/9j/2Q==", width: 1, height: 1 }));
    assert.equal((await (await screenObservation).json()).success, true);
    const unrelatedRequest = nextScreen(), unrelatedCall = invokeScreen("other-thread-screen-release", { action: "release" }, "00000000-0000-7000-8000-000000000099");
    const unrelatedFrame = await unrelatedRequest;
    screenSocket.send(JSON.stringify({ type: "agent_result", request_id: unrelatedFrame.request_id, status: "ok" }));
    assert.equal((await (await unrelatedCall).json()).success, true);
    const beforeScreenLoss = await diagnostics();
    assert.ok(beforeScreenLoss.services.find(service => service.service === "hand.broker").remote_connections.some(connection =>
      connection.hand_id === "fixture-screen-hand" && connection.connection_id === screenState.connection_id
      && connection.remote_generation === screenState.generation && connection.connected && connection.active));
    const requestedClick = nextScreen(), uncertainClick = invokeScreen("screen-lost-click", { action: "click", x: 0.5, y: 0.5 });
    const clickCall = await requestedClick; assert.equal(clickCall.input.action, "click"); screenSocket.close(1012);
    assert.equal((await (await uncertainClick).json()).structured_result.status, "unavailable");
    completeDiagnostics = await diagnostics();
    const remotePage = completeDiagnostics.services.find(service => service.service === "hand.broker");
    const screenBoundaries = remotePage.events.filter(event => event.type === "hand.remote" && event.source_call_id === "screen-lost-click");
    assert.deepEqual(screenBoundaries.map(event => event.stage), ["call.received", "call.admitted", "call.send_started", "call.sent", "call.transport_loss", "call.terminal"]);
    assert.ok(screenBoundaries.slice(1).every(event => event.connection_id === screenState.connection_id
      && event.remote_generation === screenState.generation && event.hand_id === "fixture-screen-hand"));
    assert.ok(screenBoundaries.some(event => event.stage === "call.transport_loss" && event.reason_code === "websocket_closed" && event.close_code === 1012));
    assert.ok(remotePage.events.some(event => event.type === "hand.remote" && event.stage === "connection.published"
      && event.connection_id === screenState.connection_id));
    assert.ok(!remotePage.remote_connections.some(connection => connection.connection_id === screenState.connection_id && connection.active));
    assert.equal(screenWire.filter(frame => frame.type === "agent_call").length, 3);
    assert.ok(!remotePage.events.some(event => event.source_call_id === "other-thread-screen-release"), "sharing a connection must not import another thread's calls");
    assert.doesNotMatch(JSON.stringify(completeDiagnostics), /Private fixture screen name|Private desktop title|\/9j\/2Q|hand-effects\.log|LOST_EFFECT/);
    await writeFile(join(output, "remote-before-loss.json"), JSON.stringify(beforeScreenLoss, null, 2) + "\n");
    await writeFile(join(output, "remote-wire.json"), JSON.stringify(screenWire, null, 2) + "\n");

    await writeFile(join(output, "diagnostics.json"), JSON.stringify(completeDiagnostics, null, 2) + "\n");
    await writeFile(join(output, "disconnect-diagnostics.json"), JSON.stringify(afterError, null, 2) + "\n");
    console.log(JSON.stringify({ evidence: output, thread_id: threadId, turn_id: turnId,
      initial_tool_calls: 12, tool_calls: records.filter(record => record.type === "managed.agent.tool" && record.message_type === "tool.call").length,
      child_tools: 4, replay_without_redispatch: true,
      hand_calls: wire.filter(row => row.direction === "call").length, reconnect_attempts: connectAttempts,
      error_without_close_recovered: true, lost_receipt_recovered: true, remote_click_not_replayed: true, timing }));
  } finally {
    if (attachment) await attachment.close();
    await tools.close(); await native.close(); await mf.dispose();
    console.info = log;
    await writeFile(join(output, "host-records.json"), JSON.stringify(hostRecords, null, 2) + "\n");
    await writeFile(join(output, "records.json"), JSON.stringify(records, null, 2) + "\n");
    await writeFile(join(output, "wire.json"), JSON.stringify(wire, null, 2) + "\n");
    await writeFile(join(output, "runtime.log"), raw.join("\n") + "\n");
  }
});
