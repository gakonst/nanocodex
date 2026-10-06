import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";

const root = fileURLToPath(new URL("..", import.meta.url));
const repo = fileURLToPath(new URL("../../../", import.meta.url));
const owner = "00000000-0000-4000-8000-000000000081";
const thread = "00000000-0000-7000-8000-000000000082";
const organization = "00000000-0000-7000-8000-000000000083";
const team = "00000000-0000-7000-8000-000000000084";
const command = "pnpm --filter nanocodex-managed-service test:code-mode-only";
const proof = "/brain/code-mode-only-proof.txt";
const scripts = {
  DISCOVERY: `text(await tools.tool_search({query:"exec_command update_plan",limit:8}));`,
  INITIAL: `
    text({catalog:ALL_TOOLS.map(tool=>tool.name)});
    text(await tools.exec_command({cmd:"printf 'INITIAL_OK\\n' > ${proof}; cat ${proof}",workdir:"/brain"}));
    text(await tools.update_plan({plan:[{step:"Code Mode regression",status:"completed"}]}));
  `,
  VERIFY: `text(await tools.exec_command({cmd:"cat ${proof}",workdir:"/brain"}));`,
  RECOVERED: `
    text({catalog:ALL_TOOLS.map(tool=>tool.name)});
    text(await tools.exec_command({cmd:"cat ${proof}; printf 'RECOVERED_OK\\n' >> ${proof}",workdir:"/brain"}));
    text(await tools.update_plan({plan:[{step:"Recovered Code Mode regression",status:"completed"}]}));
  `,
  FINAL: `text(await tools.exec_command({cmd:"cat ${proof}",workdir:"/brain"}));`,
};

// Fixture only account admission/configuration and external providers. The
// public HTTP turn lifecycle, DurableAgentSession, Rust WASM, QuickJS evaluator,
// nested tool routing, Just Bash and durable brain filesystem are shipped code.
const source = `
import { DurableObject } from 'cloudflare:workers';
import { DurableAgentSession, AccountHostedTools } from './src/index.ts';
export { AccountHostedTools };
const info=console.info.bind(console);
console.info=(record,...rest)=>info(record&&typeof record==='object'?JSON.stringify(record):record,...rest);
export class FixtureSession extends DurableAgentSession {
  async fetch(request) {
    if(new URL(request.url).pathname==='/__seed') {
      const {restricted}=await request.json();
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES(1,?,?,?,?,1,'https://fixture.internal/','managed',?)",'${thread}','${owner}','${organization}','${team}',Date.now());
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO managed_configuration VALUES(1,?)",JSON.stringify({...(restricted?{tools:['exec_command','update_plan']}:{}),environment:{files:[],skills:[],setup_commands:[],network:{access:'enabled'}}}));
      this.ctx.storage.sql.exec("UPDATE managed_agent_settings SET model='gpt-6.1-sol',thinking='low'");
      await this.ctx.storage.sync();return new Response(null,{status:204});
    }
    return super.fetch(request);
  }
}
export class FixtureModel extends DurableObject {
  async fetch(request) {
    if(request.headers.get('upgrade')!=='websocket') return Response.json({tools:[],machines:[],connections:[]});
    const [client,server]=Object.values(new WebSocketPair());server.accept();let scenario='',index=0,collected=[],effectiveTools=[];
    server.addEventListener('close',()=>server.close(1000));
    server.addEventListener('message',event=>{
      const body=JSON.parse(event.data),latest=JSON.stringify((body.input??[]).filter(item=>item.role==='user').at(-1));
      const next=['INITIAL','DIRECT','VERIFY','RECOVERED','FINAL','DISCOVERY'].find(key=>latest?.includes('CODE_ONLY_'+key));
      if(next&&next!==scenario){scenario=next;index=0;collected=[];}
      const call=++index;
      const declarations=[...(body.tools??[]),...(body.input??[]).filter(item=>item.type==='additional_tools').flatMap(item=>item.tools??[])];
      if(declarations.length) effectiveTools=declarations;
      console.info({type:'fixture.model',scenario,index:call,input:body.input,tools:effectiveTools});
      collected.push(...(body.input??[]).filter(item=>item.type==='custom_tool_call_output'||item.type==='function_call_output'));
      let output;
      if(call===1&&scenario==='DIRECT') output=[{type:'function_call',name:'exec_command',call_id:'call_code_only_direct',arguments:JSON.stringify({cmd:"printf 'DIRECT_MUST_NOT_RUN\\n' >> ${proof}",workdir:'/brain'})}];
      else if(call===1) output=[{type:'custom_tool_call',name:'exec',call_id:'call_code_only_'+scenario,input:${JSON.stringify(scripts)}[scenario]}];
      else output=[{type:'message',role:'assistant',content:[{type:'output_text',text:'CODE_ONLY_DONE '+scenario+' '+JSON.stringify(collected)}]}];
      server.send(JSON.stringify({type:'response.completed',response:{id:'resp_code_only_'+scenario+'_'+call,status:'completed',end_turn:call>1,output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
    });return new Response(null,{status:101,webSocket:client});
  }
}
export default {fetch(request,env) {
  const url=new URL(request.url);
  return env.NANOCODEX_SESSIONS.getByName('fixture-session').fetch(new Request('https://session.internal'+url.pathname.replace('/v1/agents/${thread}','')+url.search,request));
}};
`;

function declaredNames(tools) {
  return (tools ?? []).flatMap(tool => tool.type === "namespace"
    ? declaredNames(tool.tools)
    : [tool.name ?? tool.function?.name ?? tool.type]);
}

for (const restricted of [true, false]) {
  const catalog = restricted ? "restricted" : "hosted";
  test(`managed ${catalog} catalog stays Code Mode-only across discovery and recreation`, { timeout: 120_000 }, async () => {
    const output = join(repo, "output/code-mode-only-journey", `${catalog}-${Date.now()}-${process.pid}`);
    await mkdir(output, { recursive: true });
    const records = [], http = [], runtime = [], outbound = [];
    const result = { command, catalog, inputs: { owner, thread, restricted, scripts }, expected: {
      model_tools: ["exec", "wait"],
      nested_command_and_plan: true, direct_command_rejected_without_effect: true,
      recreate_preserves_brain_and_code_only: true,
    }, observed: {} };
    const capture = line => {
      runtime.push(line);
      const start = line.indexOf('{"type":');
      if (start >= 0) { try { records.push(JSON.parse(line.slice(start))); } catch {} }
    };
    let mf, base, failure;
    try {
      const assets = [];
      const bundle = await build({ stdin: { contents: source, resolveDir: root }, bundle: true, write: false, metafile: true,
        format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
        banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
        external: ["cloudflare:*", "node:*"],
        alias: { "nanocodex-tools/hosted": join(repo, "js/nanocodex-tools/src/hosted/index.ts"), "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
        plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /\.wasm$/ }, async args => {
          const contents = await readFile(join(args.resolveDir, args.path)), name = `fixture-${assets.length}.wasm`;
          assets.push({ type: "CompiledWasm", path: name, contents }); return { path: `./${name}`, external: true };
        }); } }], logLevel: "silent" });
      const hash = bytes => createHash("sha256").update(bytes).digest("hex");
      await writeFile(join(output, "fixture-source.mjs"), source);
      await writeFile(join(output, "test-source.mjs"), await readFile(fileURLToPath(import.meta.url)));
      await writeFile(join(output, "worker.mjs"), bundle.outputFiles[0].text);
      await writeFile(join(output, "source-resolution.json"), JSON.stringify({
        worker_sha256: hash(bundle.outputFiles[0].text), bundleInputs: Object.keys(bundle.metafile.inputs),
        wasm: assets.map(asset => ({ path: asset.path, bytes: asset.contents.length, sha256: hash(asset.contents) })),
      }, null, 2));
      // The default catalog retains its real MCP clients, but no fixture may
      // reach a live upstream. Only external JSON-RPC receipts are synthetic.
      const outboundProvider = async request => {
        const hosts = ["developers.openai.com", "mcp.tempo.xyz", "mercator.sh", "docs.mcp.cloudflare.com", "viem.sh", "vocs.dev"];
        assert.ok(hosts.includes(new URL(request.url).hostname), `unexpected fixture egress: ${request.url}`);
        const input = request.method === "POST" ? await request.json() : undefined;
        outbound.push({ url: request.url, method: request.method, input });
        if (!input || input.id === undefined) return new Response(null, { status: 202 });
        const result = input.method === "initialize"
          ? { protocolVersion: "2025-03-26", capabilities: { tools: {} }, serverInfo: { name: "synthetic-empty-mcp", version: "1" } }
          : input.method === "tools/list" ? { tools: [] } : undefined;
        return Response.json(result === undefined
          ? { jsonrpc: "2.0", id: input.id, error: { code: -32601, message: "Synthetic provider: method unavailable" } }
          : { jsonrpc: "2.0", id: input.id, result });
      };
      const start = async () => {
        const date = "2026-07-30";
        mf = new Miniflare({ port: 0,
          handleRuntimeStdio(stdout, stderr) { createInterface({ input: stdout }).on("line", capture); createInterface({ input: stderr }).on("line", capture); },
          durableObjectsPersist: join(output, "sqlite"), r2Persist: join(output, "r2"), workers: [
            { name: "managed", compatibilityDate: date, compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
              modules: [{ type: "ESModule", path: "worker.mjs", contents: bundle.outputFiles[0].text }, ...assets],
              bindings: { AGENT_IDLE_TIMEOUT_MS: "60000" }, outboundService: outboundProvider,
              durableObjects: { NANOCODEX_SESSIONS: { className: "FixtureSession", useSQLite: true },
                NANOCODEX_ACCOUNT_TOOLS: { className: "AccountHostedTools", useSQLite: true },
                NANOCODEX_MEMORY: { className: "FixtureModel", useSQLite: true }, MODEL: { className: "FixtureModel", useSQLite: true } },
              serviceBindings: { NANOCODEX: "provider" }, r2Buckets: ["NANOCODEX_HISTORY", "NANOCODEX_WORKSPACES"] },
            { name: "provider", compatibilityDate: date, modules: true,
              script: "export default {fetch(request,env){return env.MODEL.getByName('fixture-model').fetch(request)}}",
              durableObjects: { MODEL: { className: "FixtureModel", scriptName: "managed", useSQLite: true } } },
          ] });
        base = await mf.ready;
      };
      const headers = { "x-nanocodex-owner-id": owner, "x-nanocodex-session-organization-id": organization,
        "x-nanocodex-session-team-id": team, "x-nanocodex-authorization-epoch": "1",
        "x-nanocodex-capabilities": JSON.stringify(["agents:read", "agents:write", "tools:use"]), "content-type": "application/json" };
      const request = async (path, init = {}) => {
        const response = await fetch(new URL(path, base), { ...init, headers: { ...headers, ...init.headers }, signal: AbortSignal.timeout(10_000) });
        const body = await response.text(); http.push({ path, method: init.method ?? "GET", status: response.status, body });
        return { status: response.status, value: body ? JSON.parse(body) : undefined };
      };
      let number = 100;
      const runTurn = async scenario => {
        const id = `00000000-0000-7000-8000-${String(number++).padStart(12, "0")}`;
        const accepted = await request(`/v1/agents/${thread}/turns`, { method: "POST", body: JSON.stringify({ id, input: `CODE_ONLY_${scenario}` }) });
        assert.equal(accepted.status, 202, JSON.stringify(accepted));
        const deadline = Date.now() + 30_000;
        while (Date.now() < deadline) {
          const response = await request(`/v1/agents/${thread}/turns/${accepted.value.turn_id}`);
          assert.equal(response.status, 200); assert.ok(!["failed", "cancelled"].includes(response.value.state), JSON.stringify(response));
          if (response.value.state === "completed") return response.value;
          await delay(20);
        }
        throw Error(`turn deadline: ${scenario}`);
      };
      await start();
      assert.equal((await request("/__seed", { method: "POST", body: JSON.stringify({ restricted }) })).status, 204);
      if (!restricted) result.observed.discovery = await runTurn("DISCOVERY");
      result.observed.initial = await runTurn("INITIAL");
      assert.match(JSON.stringify(result.observed.initial), /INITIAL_OK/);
      result.observed.direct = await runTurn("DIRECT");
      const directOutput = records.filter(row => row.type === "fixture.model" && row.scenario === "DIRECT")
        .flatMap(row => row.input ?? []).find(item => item.call_id === "call_code_only_direct" && item.type.endsWith("_output"));
      assert.ok(directOutput, "the direct-call attempt must return a rejection to the model");
      assert.match(JSON.stringify(directOutput), /unknown|not available|not found|code.mode|not allowed|unsupported/i);
      result.observed.direct_rejection = directOutput;
      result.observed.verify = await runTurn("VERIFY");
      assert.match(JSON.stringify(result.observed.verify), /INITIAL_OK/);
      assert.doesNotMatch(JSON.stringify(result.observed.verify), /DIRECT_MUST_NOT_RUN/);
      // Real Worker teardown/recreation, retaining SQLite and R2. Do not reseed
      // configuration or copy a runtime snapshot into the replacement Worker.
      await mf.dispose(); mf = undefined;
      result.observed.recreated = true;
      await start();
      result.observed.recovered = await runTurn("RECOVERED");
      assert.match(JSON.stringify(result.observed.recovered), /INITIAL_OK/);
      result.observed.final = await runTurn("FINAL");
      assert.match(JSON.stringify(result.observed.final), /INITIAL_OK/);
      assert.match(JSON.stringify(result.observed.final), /RECOVERED_OK/);
      assert.doesNotMatch(JSON.stringify(result.observed.final), /DIRECT_MUST_NOT_RUN/);
      const history = await request(`/v1/agents/${thread}/events/history?after=0&limit=256`);
      assert.equal(history.status, 200);
      await writeFile(join(output, "events.json"), JSON.stringify(history.value, null, 2));
      const modelCalls = records.filter(row => row.type === "fixture.model");
      assert.ok(modelCalls.some(row => row.scenario === "RECOVERED"), "recreated runtime made a fresh provider request");
      result.observed.model_tools = modelCalls.map(row => ({ scenario: row.scenario, index: row.index, names: declaredNames(row.tools) }));
      for (const call of result.observed.model_tools) {
        assert.deepEqual([...new Set(call.names)].sort(), [...result.expected.model_tools].sort(), JSON.stringify(call));
      }
      const nested = records.filter(row => row.type === "managed.agent.tool" && row.message_type === "tool.result");
      for (const tool of ["exec_command", "update_plan"]) {
        const successful = nested.filter(row => row.tool === tool && row.outcome === "success");
        assert.ok(successful.length >= 2, `${tool} must succeed before and after recreation: ${JSON.stringify(nested)}`);
      }
      if (!restricted) {
        assert.ok(nested.some(row => row.tool === "tool_search" && row.outcome === "success"),
          "hosted discovery must execute successfully before checking subsequent declarations");
      }
      result.observed.tool_results = nested;
      result.observed.synthetic_outbound_requests = outbound.length;
      console.log(JSON.stringify({ catalog, evidence: output, model_calls: modelCalls.length, status: "passed" }));
    } catch (error) { failure = error; result.error = error.stack; throw error; }
    finally {
      try { await mf?.dispose(); } finally {
        await writeFile(join(output, "results.json"), JSON.stringify(result, null, 2));
        await writeFile(join(output, "records.json"), JSON.stringify(records, null, 2));
        await writeFile(join(output, "http.json"), JSON.stringify(http, null, 2));
        await writeFile(join(output, "outbound.json"), JSON.stringify(outbound, null, 2));
        await writeFile(join(output, "runtime.log"), runtime.join("\n") + "\n");
        await writeFile(join(output, "README.md"), `Run: \`${command}\`\nCatalog: ${catalog}\nStatus: ${failure ? "FAIL: " + failure.message : "PASS"}\n\nPublic HTTP turns exercise actual DurableAgentSession, Rust WASM, QuickJS, Just Bash and durable brain storage. Synthetic account seed and external model/MCP provider receipts only; outbound traffic is intercepted, never sent to live providers. Provider requests retain the complete model-visible declarations; nested commands and plans execute before and after Worker recreation with persisted SQLite/R2. A direct command must return an error and leave no file effect. Hosted discovery executes through nested tools.tool_search and must not expose direct actions. Worker source/hash, WASM hashes, public receipts, provider inputs, events and runtime logs remain here. Recreation is between completed turns; this does not claim abrupt in-flight crash recovery coverage.\n`);
      }
    }
  });
}
