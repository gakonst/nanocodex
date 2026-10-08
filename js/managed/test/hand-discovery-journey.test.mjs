import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import WebSocket from "ws";
import { createTools } from "nanocodex/tools";
import { createAttachment } from "nanocodex-tools/attachment";
import { createNodeProcessTools } from "nanocodex-tools/node";

const root = fileURLToPath(new URL("..", import.meta.url));
const candidate = fileURLToPath(new URL("../../../", import.meta.url));
const owner = "00000000-0000-4000-8000-000000000041";
const thread = "00000000-0000-7000-8000-000000000042";
const organization = "00000000-0000-7000-8000-000000000043";
const team = "00000000-0000-7000-8000-000000000044";
const machine = "synthetic-discovery-hand";
const command = "pnpm --filter nanocodex-vite build:wasm && pnpm --filter nanocodex-tools build && pnpm --filter nanocodex-connect-protocol build && pnpm --filter nanocodex-managed-service prepare:code-evaluator && pnpm --filter nanocodex-managed-service test:hand-discovery";
const sshTarget = { identity_ref: "synthetic-brain-ssh", hostname: "ssh-fixture.example.com", port: 2222,
  username: "deploy", command: ["printf", "%s", "BRAIN_SSH_DIRECT"] };
const sshCommand = "ssh -p 2222 -o IdentityRef=synthetic-brain-ssh deploy@ssh-fixture.example.com -- printf '%s' 'BRAIN_SSH_DIRECT'";
const sshScript = cmd => `const env = await tools.environment({});
  if (Object.keys(env.hands).length !== 0) throw Error("SSH requires an empty Hand namespace");
  text({ssh_environment:{hands:Object.keys(env.hands),default_cwd:env.default_cwd}});
  text(await tools.exec_command({cmd:${JSON.stringify(cmd)},workdir:"/brain"}));`;
const scripts = {
  SSH_SUCCESS: sshScript(sshCommand),
  SSH_REFUSED: sshScript(sshCommand.replace("BRAIN_SSH_DIRECT", "BRAIN_SSH_REFUSED")),
  SSH_AMBIGUOUS: sshScript(sshCommand.replace("BRAIN_SSH_DIRECT", "BRAIN_SSH_AMBIGUOUS")),
  SSH_CONNECT: sshScript(sshCommand),
  WARM: `try { text(await tools.exec_command({cmd:"printf MUST_NOT_RUN",workdir:"/${machine}"})); } catch(error) { text({error:error.message}); }`,
  PINNED: `try { await tools.exec_command({cmd:"printf MUST_NOT_RUN",workdir:"/${machine}"}); } catch(error) { notify({phase:"PINNED_CAPTURED",error:error.message}); }
    await new Promise(resolve=>setTimeout(resolve,3000));
    try { text(await tools.exec_command({cmd:"printf MUST_NOT_RUN",workdir:"/${machine}"})); } catch(error) { text({error:error.message}); }`,
  FRESH: `text(await tools.exec_command({cmd:"printf ONCE >> effect.log; printf DISCOVERED_OK",workdir:"/${machine}",shell:"/bin/sh",login:false,yield_time_ms:1000}));`,
  ABSENT: `try { text(await tools.exec_command({cmd:"printf MUST_NOT_RUN",workdir:"/genuinely-absent-hand"})); } catch(error) { text({error:error.message}); }`,
  RESOURCE_ONLINE: `const env = await tools.environment({}); const hand = env.hands["user:${machine}"];
    if (hand?.resources?.status !== "fresh" || !(hand.resources.cpu_logical_count > 0) || !(hand.resources.disk_total_bytes > 0)
      || !(hand.resources.disk_available_bytes >= 0) || !(hand.resources.observed_at_ms > 0)
      || env.execution_preferences?.native?.recommended_workdir !== "/${machine}"
      || env.execution_preferences?.computer?.recommended_workdir !== "/${machine}"
      || JSON.stringify(hand).includes("PRIVATE_RESOURCE_VALUE")) throw Error(JSON.stringify(env));
    text({resource_check:"RESOURCE_ONLINE_OK",resources:hand.resources});`,
  RESOURCE_PRESSURE: `const env = await tools.environment({}); const candidates = env.execution_preferences?.native?.candidates;
    const constrained = candidates?.find(candidate=>candidate.hand === "user:synthetic-resource-pressured-hand");
    if (env.execution_preferences?.native?.recommended_workdir !== "/${machine}" || !constrained?.constraints.includes("low_disk")
      || env.hands["user:synthetic-resource-pressured-hand"]?.resources?.disk_available_bytes !== 0) throw Error(JSON.stringify(env));
    text({resource_check:"RESOURCE_PRESSURE_OK",preferences:env.execution_preferences});`,
  RESOURCE_CAPACITY: `const env = await tools.environment({});
    if (env.execution_preferences?.native?.candidates[0]?.hand !== "user:synthetic-resource-capacity-hand") throw Error(JSON.stringify(env));
    text({resource_check:"RESOURCE_CAPACITY_OK",preferences:env.execution_preferences});`,
  RESOURCE_ORIGIN: `const env = await tools.environment({});
    if (env.request_origin?.hand?.key !== "user:${machine}" || env.execution_preferences?.native?.recommended_workdir !== "/${machine}"
      || env.execution_preferences?.computer?.recommended_workdir !== "/${machine}") throw Error(JSON.stringify(env));
    text({resource_check:"RESOURCE_ORIGIN_OK",origin:env.request_origin,preferences:env.execution_preferences});`,
  RESOURCE_OFFLINE: `const env = await tools.environment({}); const hand = env.hands["user:${machine}"];
    if (hand !== undefined && (hand.online !== false || hand.resources?.status !== "stale")
      || env.execution_preferences?.native?.recommended_workdir !== null
      || env.execution_preferences.native.candidates.length !== 0) throw Error(JSON.stringify(env));
    text({resource_check:"RESOURCE_OFFLINE_OK",resources:hand?.resources ?? {status:"unknown"}});`,
  BRAIN: `text(await tools.exec_command({cmd:"printf BRAIN_INDEPENDENT",workdir:"/brain"}));`,
};

// Fixture boundaries are synthetic account state/admission, external model and
// SSH broker responses. Session, tool routing and Just Bash are production code;
// later Hand calls also use AccountHostedTools, publisher ToolRouter and /bin/sh.
const source = `
import { DurableObject } from 'cloudflare:workers';
import { DurableAgentSession, AccountHostedTools } from './src/index.ts';
const info=console.info.bind(console);
console.info=(record,...rest)=>info(record && typeof record==='object'?JSON.stringify(record):record,...rest);
export class ObservedAccountHostedTools extends AccountHostedTools {
  async fetch(request) {
    const response=await super.fetch(request);
    if(new URL(request.url).pathname==='/snapshot') console.info({type:'fixture.account.snapshot',status:response.status,snapshot:await response.clone().json()});
    return response;
  }
}
export class FixtureSession extends DurableAgentSession {
  async fetch(request) {
    if(new URL(request.url).pathname==='/__seed') {
      // Fresh sessions defer schema setup to their first production handler.
      await (await super.fetch(new Request('https://fixture.internal/__initialize'))).body?.cancel();
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO session_state (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES (1,?,?,?,?,1,'https://fixture.internal/','managed',?)",'${thread}','${owner}','${organization}','${team}',Date.now());
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO managed_configuration VALUES (1, ?)",JSON.stringify({environment:{files:[],skills:[],setup_commands:[],network:{access:'enabled'}}}));
      this.ctx.storage.sql.exec("UPDATE managed_agent_settings SET model='gpt-6.1-sol',thinking='low'");
      await this.ctx.storage.sync(); return new Response(null,{status:204});
    }
    return super.fetch(request);
  }
}
export class FixtureModel extends DurableObject {
  async fetch(request) {
    if(request.headers.get('upgrade')!=='websocket') return Response.json({tools:[],machines:[],connections:[]});
    const [client,server]=Object.values(new WebSocketPair()); server.accept();
    const socketId=crypto.randomUUID(); let scenario='',index=0;
    server.addEventListener('close',()=>server.close(1000));
    server.addEventListener('message',event=>{
      const body=JSON.parse(event.data);
      const latest=JSON.stringify((body.input??[]).filter(item=>item.role==='user').at(-1));
      const next=Object.keys(${JSON.stringify(scripts)}).find(key=>latest?.includes('DISCOVERY_'+key));
      if(next && next!==scenario) { scenario=next; index=0; }
      const call=++index;
      console.info({type:'fixture.model.request',socket_id:socketId,scenario,index:call,input:body.input});
      const toolOutputs=(body.input??[]).filter(item=>item.type==='custom_tool_call_output'||item.type==='function_call_output');
      const output=call===1?[{type:'custom_tool_call',name:'exec',call_id:'call_discovery_'+scenario,input:${JSON.stringify(scripts)}[scenario]}]
        :[{type:'message',role:'assistant',content:[{type:'output_text',text:JSON.stringify(toolOutputs.at(-1))}]}];
      server.send(JSON.stringify({type:'response.completed',response:{id:'resp_discovery_'+scenario+'_'+call,status:'completed',end_turn:call>1,output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
    });
    return new Response(null,{status:101,webSocket:client});
  }
}
export default {fetch(request,env) {
  const url=new URL(request.url),path=url.pathname;
  if(path.startsWith('/account-tools/')) return env.NANOCODEX_ACCOUNT_TOOLS.getByName('${owner}').fetch(new Request('https://account-tools.internal/'+path.slice('/account-tools/'.length),request));
  if(path==='/tool-host') return env.NANOCODEX_ACCOUNT_TOOLS.getByName('${owner}').fetch(new Request('https://account-tools.internal/tool-host',request));
  const resource=path.replace('/v1/agents/${thread}','');
  return env.NANOCODEX_SESSIONS.getByName('fixture-session').fetch(new Request('https://session.internal'+resource+url.search,request));
}};
`;

test("a warm managed shell cell discovers a newly published Hand without retargeting old cells", { timeout: 60_000 }, async () => {
  const output = join(candidate, "output/hand-discovery-journey", `${Date.now()}-${process.pid}`);
  const workspace = join(output, "hand");
  await mkdir(workspace, { recursive: true });
  const records = [], runtime = [], wire = [], http = [], activity = [], sshBroker = [];
  const privateCanary = "SYNTHETIC_BROKER_PRIVATE_BYTES_MUST_NOT_ESCAPE";
  const result = { command, inputs: { owner, thread, machine, shell: "/bin/sh", scripts },
    expected: { warm_absent: true, old_cell_pinned: true, fresh_output: "DISCOVERED_OK", fresh_exit_code: 0, native_dispatches: 1,
      effect: "ONCE", absent_predispatch: true, restricted_predispatch: true, brain_output: "BRAIN_INDEPENDENT", ssh_without_hands: true, ssh_broker_calls: 3, ssh_connect_predispatch: true, ssh_ambiguous_replays: 0 }, observed: {} };
  const capture = line => {
    runtime.push(line);
    const offset = line.indexOf('{"type":');
    if (offset >= 0) { try { records.push(JSON.parse(line.slice(offset))); } catch {} }
  };
  let mf, native, tools, attachment, pressuredAttachment, failure;
  try {
    const resolutions = Object.fromEntries(["nanocodex/tools", "nanocodex-tools/attachment", "nanocodex-tools/node", "nanocodex-tools/runtime/tool-router"]
      .map(name => [name, fileURLToPath(import.meta.resolve(name))]));
    for (const path of Object.values(resolutions)) assert.ok(path.startsWith(candidate), `dependency escaped candidate checkout: ${path}`);
    const assets = [];
    const bundle = await build({ stdin: { contents: source, resolveDir: root }, bundle: true, write: false, metafile: true,
      format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
      banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
      external: ["cloudflare:*", "node:*"], alias: { "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
      plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /\.wasm$/ }, async args => {
        const path = join(args.resolveDir, args.path);
        assert.ok(path.startsWith(candidate), `WASM escaped candidate checkout: ${path}`);
        const contents = await readFile(path), name = `fixture-${assets.length}.wasm`;
        assets.push({ type: "CompiledWasm", path: name, contents });
        return { path: `./${name}`, external: true };
      }); } }], logLevel: "silent" });
    await writeFile(join(output, "source-resolution.json"), JSON.stringify({ resolutions, bundleInputs: Object.keys(bundle.metafile.inputs) }, null, 2));
    await writeFile(join(output, "fixture-source.mjs"), source);
    await writeFile(join(output, "worker.mjs"), bundle.outputFiles[0].text);
    const compatibilityDate = "2026-07-30";
    mf = new Miniflare({ port: 0, handleRuntimeStdio(stdout, stderr) {
      createInterface({ input: stdout }).on("line", capture); createInterface({ input: stderr }).on("line", capture);
    }, durableObjectsPersist: join(output, "sqlite"), workers: [
      { name: "managed", compatibilityDate, compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
        modules: [{ type: "ESModule", path: "worker.mjs", contents: bundle.outputFiles[0].text }, ...assets],
        bindings: { AGENT_IDLE_TIMEOUT_MS: "60000" },
        durableObjects: { NANOCODEX_SESSIONS: { className: "FixtureSession", useSQLite: true }, MODEL: { className: "FixtureModel", useSQLite: true },
          NANOCODEX_ACCOUNT_TOOLS: { className: "ObservedAccountHostedTools", useSQLite: true }, NANOCODEX_MEMORY: { className: "FixtureModel", useSQLite: true } },
        serviceBindings: { NANOCODEX: "provider" }, r2Buckets: ["NANOCODEX_HISTORY", "NANOCODEX_WORKSPACES"] },
      { name: "provider", compatibilityDate, modules: true,
        script: "export default {fetch(request,env){if(new URL(request.url).hostname==='ssh.internal') return env.SSH_BROKER.fetch(request); return env.MODEL.getByName('fixture-model').fetch(request)}}",
        serviceBindings: { SSH_BROKER: async request => {
          // Record safe request observations, never key material or fixture private bytes.
          const body = await request.json();
          sshBroker.push({ endpoint: request.url, method: request.method,
            subject: request.headers.get("x-nanocodex-subject"), body });
          if (body.command.at(-1) === "BRAIN_SSH_REFUSED") return Response.json({ error: "ssh_identity_target_mismatch", details: privateCanary }, { status: 403 });
          if (body.command.at(-1) === "BRAIN_SSH_AMBIGUOUS") return new Response(privateCanary, { status: 502 });
          return Response.json({ stdout: "BRAIN_SSH_DIRECT", stderr: "", exit_code: 0, private_metadata: privateCanary });
        } },
        durableObjects: { MODEL: { className: "FixtureModel", scriptName: "managed", useSQLite: true } } },
    ] });
    const base = await mf.ready;
    const headers = { "x-nanocodex-owner-id": owner, "x-nanocodex-session-organization-id": organization,
      "x-nanocodex-session-team-id": team, "x-nanocodex-authorization-epoch": "1",
      "x-nanocodex-capabilities": JSON.stringify(["agents:read", "agents:write", "tools:use"]), "content-type": "application/json" };
    const request = async (path, init = {}) => {
      const response = await fetch(new URL(path, base), { ...init, headers: { ...headers, ...init.headers }, signal: AbortSignal.timeout(10_000) });
      const body = await response.text();
      http.push({ path, method: init.method ?? "GET", input: init.body, status: response.status, body });
      return { status: response.status, value: body ? JSON.parse(body) : undefined };
    };
    assert.equal((await request("/__seed", { method: "POST" })).status, 204);
    const waitFor = async (predicate, description, ms = 10_000) => {
      const deadline = performance.now() + ms;
      while (performance.now() < deadline) { const value = await predicate(); if (value) return value; await delay(10); }
      throw Error(`${description} exceeded ${ms}ms; evidence ${output}`);
    };
    let turnNumber = 100;
    const startTurn = async (scenario, extraHeaders = {}) => {
      const id = `00000000-0000-7000-8000-${String(turnNumber++).padStart(12, "0")}`;
      const accepted = await request(`/v1/agents/${thread}/turns`, { method: "POST", headers: extraHeaders, body: JSON.stringify({ id, input: `DISCOVERY_${scenario}` }) });
      assert.equal(accepted.status, 202, JSON.stringify(accepted));
      return async () => waitFor(async () => {
        const turn = await request(`/v1/agents/${thread}/turns/${accepted.value.turn_id}`);
        assert.equal(turn.status, 200, JSON.stringify(turn));
        assert.ok(!["failed", "cancelled"].includes(turn.value.state), JSON.stringify(turn));
        return turn.value.state === "completed" ? turn.value : undefined;
      }, `${scenario} turn completion`);
    };
    const runTurn = async (scenario, extraHeaders) => (await startTurn(scenario, extraHeaders))();
    const callFrames = () => wire.filter(row => row.direction === "broker" && row.frame.type === "call");
    const unavailable = value => assert.match(JSON.stringify(value), /no mount owns namespace cwd/);
    // Exercise the production /brain interpreter and SSH service binding before
    // createNodeProcessTools or any publisher exists. No sandbox/VM provider is bound.
    const sshSuccess = await runTurn("SSH_SUCCESS");
    assert.match(JSON.stringify(sshSuccess), /BRAIN_SSH_DIRECT/);
    assert.equal(sshBroker.length, 1);
    assert.deepEqual(sshBroker[0].body, sshTarget, "broker must receive the exact opaque reference, TCP target and argv");
    assert.equal(sshBroker[0].endpoint, "https://ssh.internal/v1/execute");
    assert.equal(sshBroker[0].method, "POST");
    assert.match(sshBroker[0].subject, /^[a-f0-9]{64}$/);
    const sshRefused = await runTurn("SSH_REFUSED");
    assert.match(JSON.stringify(sshRefused), /private egress rejected SSH.*ssh_identity_target_mismatch/);
    assert.equal(sshBroker.length, 2, "target refusal must not retry");
    const sshAmbiguous = await runTurn("SSH_AMBIGUOUS");
    assert.match(JSON.stringify(sshAmbiguous), /private egress returned an invalid SSH response/);
    assert.equal(sshBroker.length, 3, "an ambiguous broker reply must not automatically replay SSH");
    const sshConnect = await runTurn("SSH_CONNECT", {
      "x-nanocodex-connect-grant-id": `0x${"b".repeat(64)}`,
      "x-nanocodex-connect-connectors": '["chatgpt"]', "x-nanocodex-connect-mcp-ids": "[]",
      "x-nanocodex-connect-sandbox-execution": "true",
    });
    assert.match(JSON.stringify(sshConnect), /SSH identity is not granted for this turn/);
    assert.equal(sshBroker.length, 3, "Connect shell execution authority must not grant an SSH identity");
    for (const [index, marker] of ["BRAIN_SSH_DIRECT", "BRAIN_SSH_REFUSED", "BRAIN_SSH_AMBIGUOUS"].entries()) {
      assert.deepEqual(sshBroker[index], { endpoint: "https://ssh.internal/v1/execute", method: "POST",
        subject: sshBroker[0].subject, body: { ...sshTarget, command: ["printf", "%s", marker] } });
    }
    assert.equal(native, undefined); assert.equal(attachment, undefined);
    assert.equal(wire.length, 0); assert.equal(activity.length, 0);
    const sshRows = records.filter(row => row.parent_call_id?.startsWith("call_discovery_SSH_"));
    assert.ok(!sshRows.some(row => row.type === "hand.tool.stage"), "SSH /brain calls must not prepare or dispatch to a Hand");
    assert.deepEqual(sshRows.filter(row => row.type === "managed.tool.invocation" && row.stage === "started").map(row => row.tool),
      ["environment", "exec_command", "environment", "exec_command", "environment", "exec_command", "environment", "exec_command"],
      "only environment and /brain exec may run; no mount/provisioning tool");
    for (const [scenario, expectedOutput, exitCode] of [
      ["SSH_SUCCESS", "BRAIN_SSH_DIRECT", 0],
      ["SSH_REFUSED", "ssh: private egress rejected SSH (ssh_identity_target_mismatch)\n", 255],
      ["SSH_AMBIGUOUS", "ssh: private egress returned an invalid SSH response\n", 255],
      ["SSH_CONNECT", "ssh: SSH identity is not granted for this turn\n", 255],
    ]) {
      const modelRequest = records.find(row => row.type === "fixture.model.request" && row.scenario === scenario && row.index === 2);
      const toolOutput = modelRequest.input.findLast(item => item.call_id === "call_discovery_" + scenario && item.type === "custom_tool_call_output");
      const observations = toolOutput.output.filter(item => item.text?.startsWith("{")).map(item => JSON.parse(item.text));
      assert.deepEqual(observations[0], { ssh_environment: { hands: [], default_cwd: "/brain" } });
      assert.equal(observations[1].output, expectedOutput);
      assert.equal(observations[1].exit_code, exitCode);
    }
    assert.ok(sshRows.some(row => row.type === "managed.just_bash"), "SSH must execute in the production Just Bash interpreter");
    assert.ok(!JSON.stringify({ http, records, runtime, sshBroker }).includes(privateCanary), "broker private metadata/body must not escape to model, HTTP, or logs");
    result.observed.ssh = { hands: [], broker_calls: sshBroker.length, exact_target: true, connect_predispatch: true,
      ambiguous_replays: 0, native_dispatches: 0, namespace_preparations: 0, private_bytes_exposed: false };
    const warm = await runTurn("WARM"); unavailable(warm);
    assert.ok(records.some(row => row.type === "fixture.account.snapshot"
      && (row.status === 404 && row.snapshot.error === "not_found"
        || row.status === 200 && row.snapshot.machines.length === 0)),
    "the warm runtime must finish discovery before its account Hand publishes");
    const pinnedDone = await startTurn("PINNED");
    await waitFor(() => records.some(row => row.type === "hand.tool.stage" && row.parent_call_id === "call_discovery_PINNED"
      && row.stage === "namespace.route" && row.outcome === "unavailable"), "old cell captured empty namespace");

    native = await createNodeProcessTools({ workspace, onActivity: event => activity.push(event) });
    tools = await createTools({ tools: native.tools });
    const endpoint = new URL("/tool-host", base); endpoint.protocol = "ws:";
    attachment = createAttachment(tools, { endpoint: endpoint.href, transport: { connect() {
      const socket = new WebSocket(endpoint, { headers: { "x-nanocodex-owner-id": owner } });
      const send = socket.send.bind(socket);
      socket.send = (data, ...args) => { wire.push({ direction: "host", frame: JSON.parse(String(data)) }); return send(data, ...args); };
      socket.on("message", data => wire.push({ direction: "broker", frame: JSON.parse(String(data)) }));
      socket.on("close", (code, reason) => wire.push({ event: "close", code, reason: String(reason) }));
      return socket;
    } } }, { machines: [{ id: machine, name: "Synthetic Discovery Hand", workspace, capabilities: ["shell", "computer"], resources: { ...native.resources, private_path: "PRIVATE_RESOURCE_VALUE" } }], attachmentId: machine });
    assert.equal((await attachment.connect()).connected, true);
    assert.ok(wire.some(row => row.direction === "broker" && row.frame.type === "ready"));
    const catalog = await request("/account-tools/snapshot", { method: "POST", body: JSON.stringify({ owner_id: owner }) });
    assert.equal(catalog.status, 200);
    assert.equal(catalog.value.machines.find(entry => entry.machine.id === machine)?.online, true);
    assert.equal(records.filter(row => row.type === "hand.tool.stage" && row.parent_call_id === "call_discovery_PINNED"
      && row.stage === "namespace.route" && row.outcome === "unavailable").length, 1,
    "publisher must become publicly online before the old cell's second call");
    const pinned = await pinnedDone(); unavailable(pinned);
    assert.equal(callFrames().length, 0, "an empty captured cell must not retarget after publication");
    const fresh = await runTurn("FRESH");
    assert.match(JSON.stringify(fresh), /DISCOVERED_OK/);
    assert.equal(callFrames().length, 1, "the fresh cell must dispatch exactly once");
    const receipt = wire.find(row => row.direction === "host" && row.frame.type === "result")?.frame;
    assert.equal(receipt?.outcome.status, "completed");
    assert.equal(receipt.outcome.output.success, true);
    assert.equal(receipt.outcome.output.structured_result.output, "DISCOVERED_OK");
    assert.equal(receipt.outcome.output.structured_result.exit_code, 0);
    assert.equal(await readFile(join(workspace, "effect.log"), "utf8"), "ONCE");
    const sameRuntime = records.filter(row => row.type === "fixture.model.request" && ["WARM", "PINNED", "FRESH"].includes(row.scenario));
    assert.equal(new Set(sameRuntime.map(row => row.socket_id)).size, 1, "the successful cell must use the warm runtime, not a fresh agent");
    const resourceOnline = await runTurn("RESOURCE_ONLINE");
    assert.match(JSON.stringify(resourceOnline), /RESOURCE_ONLINE_OK/);
    assert.equal(catalog.value.machines.find(entry => entry.machine.id === machine)?.machine.resources.observed_at_ms, native.resources.observed_at_ms);
    assert.ok(!JSON.stringify(catalog.value).includes("PRIVATE_RESOURCE_VALUE"));
    pressuredAttachment = createAttachment(tools, { endpoint: endpoint.href,
      transport: { connect: () => new WebSocket(endpoint, { headers: { "x-nanocodex-owner-id": owner } }) } }, {
      machines: [{ id: "synthetic-resource-pressured-hand", name: "Synthetic full-disk Hand", workspace, capabilities: ["shell"],
        resources: { ...native.resources, cpu_logical_count: 1024, disk_available_bytes: 0 } }], attachmentId: "synthetic-resource-pressured-hand" });
    assert.equal((await pressuredAttachment.connect()).connected, true);
    const resourcePressure = await runTurn("RESOURCE_PRESSURE");
    assert.match(JSON.stringify(resourcePressure), /RESOURCE_PRESSURE_OK/);
    await pressuredAttachment.close();
    pressuredAttachment = createAttachment(tools, { endpoint: endpoint.href,
      transport: { connect: () => new WebSocket(endpoint, { headers: { "x-nanocodex-owner-id": owner } }) } }, {
      machines: [{ id: "synthetic-resource-capacity-hand", name: "Synthetic capacity Hand", workspace, capabilities: ["shell", "computer"],
        resources: { ...native.resources, cpu_logical_count: 1024 } }], attachmentId: "synthetic-resource-capacity-hand" });
    assert.equal((await pressuredAttachment.connect()).connected, true);
    assert.match(JSON.stringify(await runTurn("RESOURCE_CAPACITY")), /RESOURCE_CAPACITY_OK/);
    assert.match(JSON.stringify(await runTurn("RESOURCE_ORIGIN", {"x-nanocodex-client-context":JSON.stringify({client:"desktop",hand:"user:"+machine,cwd:"/"+machine})})), /RESOURCE_ORIGIN_OK/);
    await pressuredAttachment.close();
    const absent = await runTurn("ABSENT"); unavailable(absent);
    const restricted = await request(`/v1/agents/${thread}/turns`, { method: "POST",
      headers: { "x-nanocodex-connect-grant-id": `0x${"a".repeat(64)}`,
        "x-nanocodex-connect-connectors": "[]", "x-nanocodex-connect-mcp-ids": "[]", "x-nanocodex-connect-sandbox-execution": "true" },
      body: JSON.stringify({ id: "00000000-0000-7000-8000-000000000999", input: "DISCOVERY_RESTRICTED" }) });
    assert.equal(restricted.status, 403, JSON.stringify(restricted));
    assert.equal(restricted.value.error, "connector_forbidden");
    const brain = await runTurn("BRAIN");
    assert.match(JSON.stringify(brain), /BRAIN_INDEPENDENT/);
    assert.equal(callFrames().length, 1, "absent, restricted and brain calls must never reach the native publisher");
    assert.ok(!records.some(row => row.type === "hand.tool.stage" && row.parent_call_id === "call_discovery_BRAIN"
      && row.stage === "namespace.prepare"), "/brain must bypass Hand namespace preparation");
    await attachment.close();
    await waitFor(async () => {
      const snapshot = await request("/account-tools/snapshot", { method: "POST", body: JSON.stringify({ owner_id: owner }) });
      return !snapshot.value.machines.some(entry => entry.machine.id === machine && entry.online === true);
    }, "offline resource sample");
    const resourceOffline = await runTurn("RESOURCE_OFFLINE");
    assert.match(JSON.stringify(resourceOffline), /RESOURCE_OFFLINE_OK/);
    const history = await request(`/v1/agents/${thread}/events/history?after=0&limit=256`);
    assert.equal(history.status, 200);
    await writeFile(join(output, "events.json"), JSON.stringify(history.value, null, 2));
    assert.equal(sshBroker.length, 3, "later turns and publisher lifecycle must not replay SSH");
    assert.ok(!JSON.stringify({ http, records, runtime, sshBroker, history: history.value }).includes(privateCanary));
    result.observed = { ...result.observed, warm_absent: true, old_cell_pinned: true, fresh_output: "DISCOVERED_OK", fresh_exit_code: 0, same_warm_runtime: true, resource_sample: native.resources, online_resources: "fresh", full_disk_hand_deprioritized: true, higher_capacity_fallback: true, submitting_hand_preferred: true, computer_only_cua_eligible: true, offline_hands_excluded: true, offline_resources: "absent_or_stale", unknown_resource_fields_stripped: true,
      native_dispatches: callFrames().length, effect: "ONCE", absent_predispatch: true, restricted_predispatch: true, brain_output: "BRAIN_INDEPENDENT" };
    console.log(JSON.stringify({ evidence: output, ...result.observed }));
  } catch (error) { failure = error; result.error = error.stack; throw error; }
  finally {
    try { await pressuredAttachment?.close(); await attachment?.close(); await tools?.close(); await native?.close(); await mf?.dispose(); }
    finally {
      await writeFile(join(output, "trace.json"), JSON.stringify({ result, records, activity }, null, 2));
      await writeFile(join(output, "ssh-broker.json"), JSON.stringify({ observations: sshBroker, result: result.observed.ssh }, null, 2));
      await writeFile(join(output, "wire.json"), JSON.stringify(wire, null, 2));
      await writeFile(join(output, "http.json"), JSON.stringify(http, null, 2));
      await writeFile(join(output, "runtime.log"), runtime.join("\n") + "\n");
      await writeFile(join(output, "README.md"), `Run: \`${command}\`\n\nInputs: ${JSON.stringify(result.inputs)}\n\nExpected: ${JSON.stringify(result.expected)}\n\nObserved: ${JSON.stringify(result.observed)}\n\nStatus: ${failure ? "FAIL: " + failure.message : "PASS"}\n\nEvidence: http.json (real HTTP admission, turn receipts and public catalog), wire.json (actual publisher WebSocket frames), events.json (public history), trace.json (model requests, namespace diagnostics, native activity), runtime.log, hand/effect.log, source-resolution.json, exact fixture-source.mjs/worker.mjs and persisted sqlite/. Only synthetic admission/account seed, external model, and SSH broker replies are fixtures. ssh-broker.json records the exact safe broker request observations. Before any publisher or native process tools exist, /brain SSH runs with hands=[] through production Just Bash and the direct Workers service binding, checks an exact opaque identity/target/argv, rejects target mismatch, rejects Connect authority before broker dispatch, and does not replay an ambiguous reply. Synthetic private metadata and corrupt-response bytes never enter model outputs, HTTP results or runtime logs. No sandbox/VM provider is bound. The model invokes the public environment tool before and after publisher disconnect: real Node host CPU/disk measurements remain timestamped, arbitrary resource fields are stripped, and a disconnected publisher is excluded from recommendations (a retained offline sample, when present, is stale). Production Session callback and namespace wrapper are imported unchanged. The 3-second old-cell pause gives the publisher time to become ready; failure to publish before the second old-cell call fails the fresh/online assertions.\n\nScope: local production managed runtime, account tools, public JS publisher and native shell. Live hosted account authentication, Rust publisher and deployed service rollout require the parent's live gate. /brain independence is observed by its result, no native dispatch and no namespace preparation diagnostic.\n`);
    }
  }
});
