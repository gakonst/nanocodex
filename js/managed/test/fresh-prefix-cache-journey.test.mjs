import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";

// Two fresh managed sessions of one owner must send a byte-identical
// cacheable prefix (tools + developer instructions, with derived item IDs) in
// their first Responses request. Actual DurableAgentSession, Rust WASM and
// tool catalog; only account seed and external model/MCP receipts are synthetic.
const root = fileURLToPath(new URL("..", import.meta.url));
const repo = fileURLToPath(new URL("../../../", import.meta.url));
const owner = "00000000-0000-4000-8000-000000000091";
const organization = "00000000-0000-7000-8000-000000000093";
const team = "00000000-0000-7000-8000-000000000094";
const threads = ["00000000-0000-7000-8000-000000000095", "00000000-0000-7000-8000-000000000096"];
const model = process.env.FRESH_PREFIX_MODEL ?? "gpt-6-astra";
const source = `
import { DurableObject } from 'cloudflare:workers';
import { DurableAgentSession, AccountHostedTools } from './src/index.ts';
export { AccountHostedTools };
const info=console.info.bind(console);
console.info=(record,...rest)=>info(record&&typeof record==='object'?JSON.stringify(record):record,...rest);
export class FixtureSession extends DurableAgentSession {
  async fetch(request) {
    const url=new URL(request.url);
    if(url.pathname==='/__seed') {
      const {thread}=await request.json();await (await this.brainFilesystem(new Request('https://fixture.internal/'),true)).body?.cancel();
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES(1,?,?,?,?,1,'https://fixture.internal/','managed',?)",thread,'${owner}','${organization}','${team}',Date.now());
      this.ctx.storage.sql.exec("INSERT OR IGNORE INTO managed_configuration VALUES(1,?)",JSON.stringify({environment:{files:[],skills:[],setup_commands:[],network:{access:'enabled'}}}));
      this.ctx.storage.sql.exec("UPDATE managed_agent_settings SET model='${model}',thinking='low'");
      await this.ctx.storage.sync();return new Response(null,{status:204});
    }
    return super.fetch(request);
  }
}
export class FixtureModel extends DurableObject {
  async fetch(request) {
    if(request.headers.get('upgrade')!=='websocket') { console.info({type:'fixture.metadata',path:new URL(request.url).pathname,at:Date.now()}); return Response.json({tools:[],machines:[],connections:[]}); }
    const [client,server]=Object.values(new WebSocketPair());server.accept();
    server.addEventListener('close',()=>server.close(1000));
    server.addEventListener('message',event=>{
      const body=JSON.parse(event.data);
      if(body.type!=='response.create') return;
      console.info({type:'fixture.model',body});
      server.send(JSON.stringify({type:'response.completed',response:{id:'resp_fp_'+crypto.randomUUID(),status:'completed',end_turn:true,
        output:[{type:'message',role:'assistant',content:[{type:'output_text',text:'FP_DONE'}]}],usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
    });return new Response(null,{status:101,webSocket:client});
  }
}
export default {fetch(request,env) {
  const url=new URL(request.url);const match=/^\\/v1\\/agents\\/([^/]+)(.*)$/.exec(url.pathname);
  const name=match?match[1]:url.searchParams.get('thread');
  return env.NANOCODEX_SESSIONS.getByName(name).fetch(new Request('https://session.internal'+(match?match[2]:url.pathname)+url.search,request));
}};
`;

export function firstDifference(left, right) {
  let index = 0;
  while (index < left.length && index < right.length && left[index] === right[index]) index += 1;
  return index === left.length && index === right.length ? -1 : index;
}

test(`fresh ${model} sessions of one owner share a byte-identical cacheable prefix`, { timeout: 180_000 }, async () => {
  const output = join(repo, "output/fresh-prefix-cache-journey", `${model}-${Date.now()}-${process.pid}`);
  await mkdir(output, { recursive: true });
  const records = [], runtime = [];
  const capture = line => {
    runtime.push(line.length > 2000 ? line.slice(0, 2000) + "..." : line);
    const start = line.indexOf('{"type":');
    if (start >= 0) { try { records.push(JSON.parse(line.slice(start))); } catch {} }
  };
  const assets = [];
  const bundle = await build({ stdin: { contents: source, resolveDir: root }, bundle: true, write: false,
    format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
    external: ["cloudflare:*", "node:*"],
    alias: { "nanocodex-tools/hosted": join(repo, "js/nanocodex-tools/src/hosted/index.ts"), "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
    plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /\.wasm$/ }, async args => {
      const contents = await readFile(join(args.resolveDir, args.path)), name = `fixture-${assets.length}.wasm`;
      assets.push({ type: "CompiledWasm", path: name, contents }); return { path: `./${name}`, external: true };
    }); } }], logLevel: "silent" });
  let mcpLists = 0;
  const outboundProvider = async request => {
    const input = request.method === "POST" ? await request.json().catch(() => undefined) : undefined;
    if (!input || input.id === undefined) return new Response(null, { status: 202 });
    const result = input.method === "initialize"
      ? { protocolVersion: "2025-03-26", capabilities: { tools: {} }, serverInfo: { name: "synthetic-empty-mcp", version: "1" } }
      : input.method === "tools/list" ? { tools: [{ name: "lookup_" + new URL(request.url).hostname.replace(/[^a-z]/g, "_"), description: "Synthetic lookup.", inputSchema: { type: "object", properties: { q: { type: "string" } } } }] } : undefined;
    if (input.method === "tools/list") await delay(Number(process.env.FRESH_PREFIX_MCP_DELAY_MS ?? 0) * (mcpLists++ % 2));
    return Response.json(result === undefined ? { jsonrpc: "2.0", id: input.id, error: { code: -32601, message: "unavailable" } }
      : { jsonrpc: "2.0", id: input.id, result });
  };
  const date = "2026-07-30";
  const mf = new Miniflare({ port: 0,
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
  try {
    const base = await mf.ready;
    const headers = { "x-nanocodex-owner-id": owner, "x-nanocodex-session-organization-id": organization,
      "x-nanocodex-session-team-id": team, "x-nanocodex-authorization-epoch": "1",
      "x-nanocodex-capabilities": JSON.stringify(["agents:read", "agents:write", "tools:use"]), "content-type": "application/json" };
    const request = async (path, init = {}) => {
      const response = await fetch(new URL(path, base), { ...init, headers: { ...headers, ...init.headers }, signal: AbortSignal.timeout(20_000) });
      const body = await response.text();
      try { return { status: response.status, value: body ? JSON.parse(body) : undefined }; } catch { throw new Error(`${path} ${response.status}: ${body.slice(0, 500)}`); }
    };
    let number = 200;
    for (const thread of threads) {
      assert.equal((await request(`/__seed?thread=${thread}`, { method: "POST", body: JSON.stringify({ thread }) })).status, 204);
      const id = `00000000-0000-7000-8000-${String(number++).padStart(12, "0")}`;
      const accepted = await request(`/v1/agents/${thread}/turns`, { method: "POST", body: JSON.stringify({ id, input: "yo" }) });
      assert.equal(accepted.status, 202, JSON.stringify(accepted));
      const deadline = Date.now() + 60_000;
      for (;;) {
        const turn = await request(`/v1/agents/${thread}/turns/${accepted.value.turn_id}`);
        assert.ok(!["failed", "cancelled"].includes(turn.value?.state), JSON.stringify(turn));
        if (turn.value?.state === "completed") break;
        assert.ok(Date.now() < deadline, `turn deadline ${thread}`);
        await delay(25);
      }
    }
    const bodies = records.filter(row => row.type === "fixture.model").map(row => row.body).filter(body => body.generate !== false);
    assert.ok(bodies.length >= 2, `expected two generation requests, saw ${bodies.length}`);
    const [first, second] = bodies;
    await writeFile(join(output, "first.json"), JSON.stringify(first, null, 2));
    await writeFile(join(output, "second.json"), JSON.stringify(second, null, 2));
    assert.equal(first.prompt_cache_key, second.prompt_cache_key);
    const encodedFirst = JSON.stringify(first.input), encodedSecond = JSON.stringify(second.input);
    const offset = firstDifference(encodedFirst, encodedSecond);
    const prefixLength = JSON.stringify(first.input.slice(0, 2)).length;
    const summary = { model, offset, prefix_chars: prefixLength, input_chars: encodedFirst.length,
      first_items: first.input.map(item => item.type ?? item.role), context: offset < 0 ? null : {
        first: encodedFirst.slice(Math.max(0, offset - 200), offset + 200), second: encodedSecond.slice(Math.max(0, offset - 200), offset + 200) } };
    await writeFile(join(output, "summary.json"), JSON.stringify(summary, null, 2));
    console.log(JSON.stringify({ evidence: output, ...summary, context: undefined }));
    assert.equal(first.input[0].type, "additional_tools");
    assert.equal(first.input[1].role, "developer");
    // The whole cacheable prefix (tools, instructions and their derived IDs) is identical;
    // session-variable context may only begin after it.
    assert.ok(offset < 0 || offset >= prefixLength - 1, `prefix differs at char ${offset} of ${prefixLength}: ${JSON.stringify(summary.context)}`);
  } finally {
    await mf.dispose();
    await writeFile(join(output, "runtime.log"), runtime.join("\n") + "\n");
  }
});
