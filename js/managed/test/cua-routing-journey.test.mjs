import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
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

// Resolve the validator from the existing nanocodex dependency, matching its tool validation library.
const { Validator } = createRequire(import.meta.resolve("nanocodex/tools"))("@cfworker/json-schema");

const root = fileURLToPath(new URL("..", import.meta.url));
const candidate = fileURLToPath(new URL("../../../", import.meta.url));
const owner = "00000000-0000-4000-8000-000000000041";
const thread = "00000000-0000-7000-8000-000000000042";
const organization = "00000000-0000-7000-8000-000000000043";
const team = "00000000-0000-7000-8000-000000000044";
const machine = "synthetic-cua-hand";
const command = "pnpm --filter nanocodex-managed-service test:cua-routing";
const discovery = `text(await tools.mcp__cua_repl__js({workdir:"/${machine}"}));`;
const scripts = {
  UPSTREAM: `text({hand:(await tools.environment({})).hands["user:${machine}"]}); ${discovery}
    text(await tools.mcp__cua_repl__js({workdir:"/${machine}",code:"UPSTREAM_OK"}));
    text(await tools.mcp__cua_repl__js_reset({workdir:"/${machine}"}));`,
  PREFERRED: `${discovery} text(await tools.mcp__cua_repl__js({workdir:"/${machine}",code:"PREFERRED_OK"}));`,
  PINNED_UPSTREAM: `${discovery} await new Promise(resolve=>setTimeout(resolve,3000)); ${discovery}
    try { text(await tools.mcp__cua_repl__js({workdir:"/${machine}",code:"MUST_NOT_RETARGET"})); } catch(error) { text({error:error.message}); }`,
  SCREEN: `${discovery} text(await tools.mcp__cua_repl__js({workdir:"/${machine}",action:"observe"}));`,
  BAD_SCROLL: `try { text(await tools.mcp__cua_repl__js({workdir:"/${machine}",action:"scroll",deltaY:120})); } catch(error) { text({error:error.message}); }`,
  SCROLL: `text(await tools.mcp__cua_repl__js({workdir:"/${machine}",action:"scroll",x:0.4,y:0.6,deltaX:0,deltaY:120}));`,
  PINNED_MISSING: `try { ${discovery} } catch(error) { text({error:error.message}); }
    await new Promise(resolve=>setTimeout(resolve,3000));
    try { text(await tools.mcp__cua_repl__js({workdir:"/${machine}",action:"observe"})); } catch(error) { text({error:error.message}); }`,
  RECOVERED: `${discovery} text(await tools.mcp__cua_repl__js({workdir:"/${machine}",action:"observe"}));
    text(await tools.mcp__cua_repl__js_reset({workdir:"/${machine}"}));`,
};

// Fixture boundaries are synthetic account state/admission and the external
// model. Every tool passes through the shipped Session preparation callback,
// namespace wrapper, AccountHostedTools, publisher ToolRouter and screen socket.
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
      const next=Object.keys(${JSON.stringify(scripts)}).find(key=>latest?.includes('CUA_'+key));
      if(next && next!==scenario) { scenario=next; index=0; }
      const call=++index;
      console.info({type:'fixture.model.request',socket_id:socketId,scenario,index:call,input:body.input});
      const toolOutputs=(body.input??[]).filter(item=>item.type==='custom_tool_call_output'||item.type==='function_call_output');
      const output=call===1?[{type:'custom_tool_call',name:'exec',call_id:'call_cua_'+scenario,input:${JSON.stringify(scripts)}[scenario]}]
        :[{type:'message',role:'assistant',content:[{type:'output_text',text:JSON.stringify(toolOutputs.at(-1))}]}];
      server.send(JSON.stringify({type:'response.completed',response:{id:'resp_cua_'+scenario+'_'+call,status:'completed',end_turn:call>1,output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
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

test("managed CUA cells prefer live upstream, pin routes, and recover screen fallback on fresh discovery", { timeout: 60_000 }, async () => {
  const output = join(candidate, "output/cua-routing-journey", `${Date.now()}-${process.pid}`);
  const workspace = join(output, "hand");
  await mkdir(workspace, { recursive: true });
  const records = [], runtime = [], wire = [], http = [], activity = [];
  const screenWire = [], screenSockets = [];
  const result = { command, inputs: { owner, thread, machine, scripts },
    expected: { upstream_preferred: true, environment_agrees: true, old_cell_pinned: true,
      fresh_screen_fallback: true, discovered_scroll_schema_validated: true, invalid_scroll_predispatch: true, complete_scroll_forwarded: true,
      missing_route_predispatch: true, same_hand_recovery: true }, observed: {} };
  const capture = line => {
    runtime.push(line);
    const offset = line.indexOf('{"type":');
    if (offset >= 0) { try { records.push(JSON.parse(line.slice(offset))); } catch {} }
  };
  let mf, native, tools, attachment, failure, upstreamSocket, upstreamDisconnected = false;
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
    const sourcePaths = ["js/managed/src/index.ts", "js/managed/src/account-hosted-tools.ts", "js/managed/src/namespace-tools.ts",
      "js/managed/src/hand-remote-agent.ts", "js/managed/test/cua-routing-journey.test.mjs", "js/managed/package.json"];
    const sourceHashes = Object.fromEntries(await Promise.all(sourcePaths.map(async path =>
      [path, createHash("sha256").update(await readFile(join(candidate, path))).digest("hex")])));
    await writeFile(join(output, "source-resolution.json"), JSON.stringify({ resolutions, sourceHashes, bundleInputs: Object.keys(bundle.metafile.inputs) }, null, 2));
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
        script: "export default {fetch(request,env){return env.MODEL.getByName('fixture-model').fetch(request)}}",
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
      const accepted = await request(`/v1/agents/${thread}/turns`, { method: "POST", headers: extraHeaders, body: JSON.stringify({ id, input: `CUA_${scenario}` }) });
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
    native = await createNodeProcessTools({ workspace, onActivity: event => activity.push(event) });
    tools = await createTools({ tools: {
      ...Object.fromEntries(native.tools.map(tool => [tool.name, tool])),
      mcp__cua_repl__js: { description: "Synthetic upstream CUA JavaScript", parameters: {
        type: "object", required: ["code"], properties: { code: { type: "string" } }, additionalProperties: false },
        handler(input) { return { backend: "upstream", code: input.code }; } },
      mcp__cua_repl__js_reset: { description: "Synthetic upstream CUA reset", parameters: {
        type: "object", properties: {}, additionalProperties: false }, handler() { return { reset: "UPSTREAM_RESET_OK" }; } },
    } });
    const endpoint = new URL("/tool-host", base); endpoint.protocol = "ws:";
    attachment = createAttachment(tools, { endpoint: endpoint.href, transport: { connect() {
      if (upstreamDisconnected) throw new Error("Synthetic upstream remains disconnected");
      const socket = new WebSocket(endpoint, { headers: { "x-nanocodex-owner-id": owner } });
      upstreamSocket = socket;
      const send = socket.send.bind(socket);
      socket.send = (data, ...args) => { wire.push({ direction: "host", frame: JSON.parse(String(data)) }); return send(data, ...args); };
      socket.on("message", data => wire.push({ direction: "broker", frame: JSON.parse(String(data)) }));
      socket.on("close", (code, reason) => wire.push({ event: "close", code, reason: String(reason) }));
      return socket;
    } } }, { machines: [{ id: machine, name: "Synthetic CUA Hand", workspace, capabilities: ["shell"] }], attachmentId: machine });
    assert.equal((await attachment.connect()).connected, true);
    assert.ok(wire.some(row => row.direction === "broker" && row.frame.type === "ready"));
    const snapshot = async () => {
      const response = await request("/account-tools/snapshot", { method: "POST", body: JSON.stringify({ owner_id: owner }) });
      assert.equal(response.status, 200); return response.value;
    };
    const screenCalls = () => screenWire.filter(row => row.direction === "broker" && row.frame.type === "agent_call");
    const publishScreen = async () => {
      const response = await mf.dispatchFetch("https://fixture.internal/account-tools/hands/host", { headers: { ...headers, upgrade: "websocket" } });
      assert.equal(response.status, 101);
      const socket = response.webSocket;
      screenSockets.push(socket);
      const received = [];
      socket.addEventListener("message", event => {
        const frame = JSON.parse(String(event.data)); received.push(frame);
        screenWire.push({ direction: "broker", frame });
        if (frame.type === "agent_call") {
          const reply = { type: "agent_result", request_id: frame.request_id, status: "ok", jpeg: "/9j/2Q==", width: 1, height: 1 };
          screenWire.push({ direction: "host", frame: reply }); socket.send(JSON.stringify(reply));
        }
      });
      socket.accept();
      await waitFor(() => received.find(frame => frame.type === "ready"), "screen socket ready");
      const catalog = { type: "catalog", machine_id: machine, machine_name: "Synthetic CUA Hand",
        surfaces: [{ id: "desktop", name: "Synthetic desktop", kind: "desktop", width: 1, height: 1, controllable: true, agent_tools: true }] };
      screenWire.push({ direction: "host", frame: catalog }); socket.send(JSON.stringify(catalog));
      await waitFor(() => received.find(frame => frame.type === "published"), "screen published");
      assert.ok((await snapshot()).screens.some(screen => screen.machine_id === machine));
      return socket;
    };
    const stages = (scenario, stage) => records.filter(row => row.type === "hand.tool.stage"
      && row.parent_call_id === `call_cua_${scenario}` && row.stage === stage);
    const rendered = value => JSON.stringify(value);
    const returnedValues = turn => JSON.parse(turn.terminal.final_message).output.flatMap(item => {
      try { return [JSON.parse(item.text)]; } catch { return []; }
    });

    // With no screen publisher, the complete online CUA pair alone must advertise computer.
    const upstream = await runTurn("UPSTREAM");
    assert.match(rendered(upstream), /UPSTREAM_OK/);
    assert.match(rendered(upstream), /UPSTREAM_RESET_OK/);
    const upstreamValues = returnedValues(upstream);
    assert.ok(upstreamValues.find(value => value.hand)?.hand.capabilities.includes("computer"),
      "upstream-only CUA pair must advertise computer in environment");
    assert.match(rendered(upstream), /upstream/);
    assert.equal(callFrames().length, 2);
    assert.deepEqual(callFrames().map(row => row.frame.input), [{ code: "UPSTREAM_OK" }, {}]);
    assert.equal(screenCalls().length, 0);

    const screen = await publishScreen();
    const preferred = await runTurn("PREFERRED");
    assert.match(rendered(preferred), /PREFERRED_OK/);
    assert.equal(callFrames().length, 3);
    assert.equal(screenCalls().length, 0, "the live upstream pair wins even with a published screen");

    const pinnedDone = await startTurn("PINNED_UPSTREAM");
    await waitFor(() => stages("PINNED_UPSTREAM", "namespace.route").length === 1, "old cell captured upstream");
    upstreamDisconnected = true; upstreamSocket.terminate();
    await waitFor(async () => (await snapshot()).machines.find(entry => entry.machine.id === machine)?.online === false,
      "upstream disconnect visible in account catalog");
    assert.equal(stages("PINNED_UPSTREAM", "namespace.route").length, 1, "disconnect must precede the old cell's second discovery");
    const pinned = await pinnedDone();
    assert.match(rendered(pinned), /upstream/);
    assert.doesNotMatch(rendered(pinned), /native_screen/);
    assert.equal(screenCalls().length, 0, "an old cell never falls through to a newly available backend");
    assert.equal(callFrames().length, 3);

    const fresh = await runTurn("SCREEN");
    assert.match(rendered(fresh), /native_screen/);
    assert.match(rendered(fresh), /Screen action completed/);
    assert.equal(screenCalls().length, 1);
    assert.deepEqual(screenCalls()[0].frame.input, { action: "observe" });
    const discovered = returnedValues(fresh).find(value => value.definitions);
    assert.equal(discovered.backend, "native_screen");
    const scrollValidator = new Validator(discovered.definitions.find(tool => tool.name === "mcp__cua_repl__js").parameters);
    const completeScroll = { action: "scroll", x: 0.4, y: 0.6, deltaX: 0, deltaY: 120 };
    assert.equal(scrollValidator.validate(completeScroll).valid, true, "discovered contract accepts complete scroll");
    for (const field of ["x", "y", "deltaX", "deltaY"]) {
      const incomplete = { ...completeScroll }; delete incomplete[field];
      assert.equal(scrollValidator.validate(incomplete).valid, false, `discovered scroll contract requires ${field}`);
    }
    const invalid = await runTurn("BAD_SCROLL");
    assert.match(rendered(invalid), /error|invalid/i);
    assert.equal(screenCalls().length, 1, "incomplete scroll must fail before host dispatch");
    const scroll = await runTurn("SCROLL");
    assert.match(rendered(scroll), /Screen action completed/);
    assert.equal(screenCalls().length, 2);
    assert.deepEqual(screenCalls()[1].frame.input, { action: "scroll", x: 0.4, y: 0.6, deltaX: 0, deltaY: 120 });

    screen.close(1000);
    await waitFor(async () => !(await snapshot()).screens.some(entry => entry.machine_id === machine), "screen disconnect visible");
    const missingDone = await startTurn("PINNED_MISSING");
    await waitFor(() => stages("PINNED_MISSING", "namespace.invoke").some(row => row.outcome === "unavailable"), "old cell captured missing screen");
    await publishScreen();
    assert.equal(stages("PINNED_MISSING", "namespace.invoke").length, 1, "republication must precede the captured cell's retry");
    const missing = await missingDone();
    assert.match(rendered(missing), /No action was dispatched/);
    assert.match(rendered(missing), /new Code Mode cell/);
    assert.equal(stages("PINNED_MISSING", "namespace.invoke").filter(row => row.outcome === "unavailable").length, 2);
    assert.equal(screenCalls().length, 2, "the old missing route must not pick up the replacement screen");
    const recovered = await runTurn("RECOVERED");
    assert.match(rendered(recovered), /native_screen/);
    assert.match(rendered(recovered), /Screen action completed/);
    assert.equal(screenCalls().length, 4);
    assert.deepEqual(screenCalls().slice(2).map(row => row.frame.input), [{ action: "observe" }, { action: "release" }]);
    assert.equal(callFrames().length, 3, "no fallback or recovery may reach the disconnected upstream publisher");
    const requests = records.filter(row => row.type === "fixture.model.request");
    assert.equal(new Set(requests.map(row => row.socket_id)).size, 1, "all scenarios must share the warm managed runtime");
    const history = await request(`/v1/agents/${thread}/events/history?after=0&limit=256`);
    assert.equal(history.status, 200);
    await writeFile(join(output, "events.json"), JSON.stringify(history.value, null, 2));
    result.observed = { ...result.expected, same_warm_runtime: true, upstream_dispatches: callFrames().length,
      screen_dispatches: screenCalls().length };
    console.log(JSON.stringify({ evidence: output, ...result.observed }));
  } catch (error) { failure = error; result.error = error.stack; throw error; }
  finally {
    try { for (const socket of screenSockets) if (socket.readyState < 2) socket.close(1000); await attachment?.close(); await tools?.close(); await native?.close(); await mf?.dispose(); }
    finally {
      await writeFile(join(output, "trace.json"), JSON.stringify({ result, records, activity }, null, 2));
      await writeFile(join(output, "wire.json"), JSON.stringify(wire, null, 2));
      await writeFile(join(output, "screen-wire.json"), JSON.stringify(screenWire, null, 2));
      await writeFile(join(output, "http.json"), JSON.stringify(http, null, 2));
      await writeFile(join(output, "runtime.log"), runtime.join("\n") + "\n");
      await writeFile(join(output, "README.md"), `Run: \`${command}\`

Status: ${failure ? "FAIL: " + failure.message : "PASS"}

Expected: ${JSON.stringify(result.expected)}

Observed: ${JSON.stringify(result.observed)}

Evidence: http.json (public turn admission/receipts and account catalog), wire.json (upstream publisher frames), screen-wire.json (screen WebSocket frames), events.json (public history), trace.json (model requests and namespace diagnostics), runtime.log, source-resolution.json, fixture-source.mjs/worker.mjs and sqlite/.

Scope: actual candidate managed Worker, account provider, namespace and Code Mode through public turns. Account seed/admission, external model, upstream CUA handlers and screen host are synthetic. No real browser input, live identity, deployed service or screenshot/video decoding is exercised. Three-second pauses are bounded synchronization windows; assertions require each catalog transition to complete before the pinned cell resumes.
`);

    }
  }
});
