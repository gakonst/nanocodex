import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline";
import { setTimeout as delay } from "node:timers/promises";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import { readD1Migrations } from "@cloudflare/vitest-pool-workers";
import WebSocket from "ws";
import { fetch } from "./support/miniflare-fetch.mjs";

// Observable journey: a synthetic Greek contact is found from the phone's HTTP
// endpoint and real managed tools with Latin/Greek queries. Pagination, literal
// punctuation, edits, and another account must retain their normal semantics.
// Only identity enrollment and the outbound model are fixtures.
const root = fileURLToPath(new URL("..", import.meta.url));
const output = join(root, "../../output/crm-greek-search", `${Date.now()}-${process.pid}`);
const queries = ["Giannis", "yiannis", "Yannis", "Chalkidis", "xalkidis", "khalkidis", "chal", "ΧΑΛΚΙΔΗΣ", "γιαννης", "Χαλκίδης", "Γιάννης Χαλκίδης"];
const codes = {
  CRM_SETUP: `text(await tools.crm_save({kind:"person",name:"Γιάννης Χαλκίδης",tags:["fixture"]})); text(await tools.crm_save({kind:"person",name:"GIANNIS CHALKIDIS",tags:["fixture"]})); text(await tools.crm_graph({operation:"save",text:"Γιάννης Χαλκίδης",metadata:{fixture:true}})); text(await tools.crm_save({kind:"person",name:"Literal [%_*?] \\\\ fixture"})); text(await tools.crm_save({kind:"person",name:"Μαΐος "+"α".repeat(90)}));for(const name of ["Παύλος","Ευάγγελος","Σπύρος"])text(await tools.crm_save({kind:"person",name}));`,
  CRM_SEARCH: `for (const q of ${JSON.stringify(queries)}) { text(await tools.crm_search({q,tag:"fixture",limit:1})); text(await tools.crm_graph({operation:"search",q})); } const p=await tools.crm_graph({operation:"search",q:"giannis",limit:1});text(p);text(await tools.crm_graph({operation:"search",q:"giannis",limit:1,cursor:p.next_cursor}));`,
  CRM_EDIT: `const p=(await tools.crm_search({q:"Γιάννης Χαλκίδης",tag:"fixture"})).records;for(const r of p)text(await tools.crm_save({id:r.id,name:"Νίκος Παπαδόπουλος"})); const n=(await tools.crm_graph({operation:"search",q:"giannis"})).nodes;for(const r of n){if(!r.source_managed)text(await tools.crm_graph({operation:"save",id:r.id,text:"Νίκος Παπαδόπουλος"}));}`,
};
const source = `
import {DurableObject} from 'cloudflare:workers';
import worker,{DurableAgentSession,AccountHostedTools} from './src/index.ts';
import {UserAccount,Organization,ApiKeyRecord,NonceStorage,ensureAccount,createApiKey} from './src/account-auth.ts';
export {DurableAgentSession,AccountHostedTools,UserAccount,Organization,ApiKeyRecord,NonceStorage};
export class FixtureModel extends DurableObject {
 async fetch(request) {
  if(request.headers.get('upgrade')!=='websocket')return Response.json({tools:[],machines:[],connections:[]});
  const [client,server]=Object.values(new WebSocketPair());server.accept();let marker='',step=0;
  server.addEventListener('close',()=>server.close(1000));
  server.addEventListener('message',event=>{
   const body=JSON.parse(event.data),user=JSON.stringify((body.input??[]).filter(i=>i.role==='user').at(-1));
   const codes=${JSON.stringify(codes)};
   const next=Object.keys(codes).find(k=>user?.includes(k));if(next&&next!==marker){marker=next;step=0;}
   const input=step++===0?codes[marker]:null;
   server.send(JSON.stringify({type:'response.completed',response:{id:'crm-'+marker+'-'+step,status:'completed',end_turn:!input,
    output:input?[{type:'custom_tool_call',name:'exec',call_id:marker+'-'+step,input}]:[{type:'message',role:'assistant',content:[{type:'output_text',text:marker+'_DONE'}]}],usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
  });
  return new Response(null,{status:101,webSocket:client});
 }
}
export default {async fetch(request,env,ctx){
 if(new URL(request.url).pathname==='/__fixture'){
  const b=await request.json();await ensureAccount(env,b.user,true);
  const auth=await(await env.NANOCODEX_USERS.getByName(b.user).fetch('https://user.internal/authorization')).json();
  return Response.json(await createApiKey(env,{kind:'api_key',userId:b.user,...auth.grant,subjectId:'api_key:'+b.user,credentialId:'fixture',capabilities:b.capabilities??['agents:read','agents:write','tools:use']},'Synthetic CRM journey'));
 }
 return worker.fetch(request,env,ctx);
}};`;

test("Greeklish CRM search through public HTTP and managed tool transport", { timeout: 120_000 }, async () => {
  await mkdir(output, { recursive: true });
  const trace = [], wire = [], logs = [], assets = [];
  let assetIndex = 0;
  const bundle = await build({ stdin: { contents: source, resolveDir: root }, bundle: true, write: false,
    format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    banner: { js: 'import {createRequire} from "node:module";const require=createRequire("/worker.mjs");' },
    external: ["cloudflare:*", "node:*"], alias: { "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
    plugins: [{ name: "wasm", setup(b) { b.onResolve({ filter: /\.wasm$/ }, async args => {
      const name = `fixture-${assetIndex++}.wasm`;
      assets.push({ type: "CompiledWasm", path: name, contents: await readFile(join(args.resolveDir, args.path)) });
      return { path: `./${name}`, external: true };
    }); } }], logLevel: "silent" });
  const common = { compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"] };
  const mf = new Miniflare({ port: 0, handleRuntimeStdio(stdout, stderr) {
    for (const input of [stdout, stderr]) createInterface({ input }).on("line", line => logs.push(line));
  }, workers: [{ ...common, name: "managed",
    modules: [{ type: "ESModule", path: "worker.mjs", contents: bundle.outputFiles[0].text }, ...assets],
    durableObjects: Object.fromEntries([
      ["NANOCODEX_SESSIONS", "DurableAgentSession"], ["NANOCODEX_USERS", "UserAccount"], ["NANOCODEX_ORGANIZATIONS", "Organization"],
      ["NANOCODEX_API_KEYS", "ApiKeyRecord"], ["NANOCODEX_AUTH", "NonceStorage"], ["NANOCODEX_ACCOUNT_TOOLS", "AccountHostedTools"],
      ["NANOCODEX_MEMORY", "FixtureModel"], ["MODEL", "FixtureModel"],
    ].map(([name, className]) => [name, { className, useSQLite: true }])),
    d1Databases: ["NANOCODEX_CRM"], r2Buckets: ["NANOCODEX_HISTORY", "NANOCODEX_WORKSPACES"], serviceBindings: { NANOCODEX: "provider" },
  }, { ...common, name: "provider", modules: true,
    script: "export default {fetch(r,e){return e.MODEL.getByName('fixture-model').fetch(r)}};",
    durableObjects: { MODEL: { className: "FixtureModel", scriptName: "managed", useSQLite: true } },
  }] });
  let socket;
  try {
    const base = await mf.ready, db = await mf.getD1Database("NANOCODEX_CRM", "managed");
    for (const migration of await readD1Migrations(join(root, "migrations"))) {
      await db.batch(migration.queries.map(sql => db.prepare(sql)));
    }
    const owner = crypto.randomUUID(), other = crypto.randomUUID();
    const issue = async (user, capabilities) => {
      const r = await fetch(new URL("/__fixture", base), { method: "POST", body: JSON.stringify({ user, capabilities }) });
      assert.equal(r.status, 200); return (await r.json()).token;
    };
    const token = await issue(owner), otherToken = await issue(other), deniedToken = await issue(owner, ["agents:read"]);
    // Preexisting synthetic data, including thousands of nonmatches before the
    // new contact. Exact/expanded result paging must never depend on scan chunks.
    await db.prepare("INSERT INTO crm_records(owner_id,id,kind,name,created_at,updated_at) SELECT ?,printf('seed-%05d',value),'person','Synthetic Example '||value,value,value FROM json_each(?)")
      .bind(owner, JSON.stringify(Array.from({ length: 2000 }, (_, i) => i))).run();
    await db.prepare("INSERT INTO crm_nodes(owner_id,id,text,metadata,created_at,updated_at) SELECT ?,printf('node-%05d',value),'Synthetic graph text '||value,'{}',1,1 FROM json_each(?)")
      .bind(owner, JSON.stringify(Array.from({ length: 8000 }, (_, i) => i))).run();
    async function call(path, expected = 200, selected = token, body) {
      const started = performance.now();
      const r = await fetch(new URL(path, base), { method: body ? "POST" : "GET",
        headers: { authorization: selected ? `Bearer ${selected}` : "", "content-type": "application/json" },
        ...(body ? { body: JSON.stringify(body) } : {}) });
      const data = await r.json();
      trace.push({ path, expected, status: r.status, milliseconds: performance.now() - started, data });
      assert.equal(r.status, expected, JSON.stringify(data)); return data;
    }
    const created = await call("/v1/agents", 201, token, { settings: { model: "gpt-6.1-sol", thinking: "low", reasoning_mode: "standard", fast_mode: false } });
    socket = new WebSocket(new URL(`/v1/agents/${created.agent_id}/ws`, base).href.replace(/^http/, "ws"), { headers: { authorization: `Bearer ${token}` } });
    socket.on("message", data => wire.push(JSON.parse(String(data))));
    let socketError; socket.on("error", error => { socketError = error; });
    async function waitFor(predicate, label) {
      const deadline = Date.now() + 45_000;
      while (!predicate()) { if (socketError) throw socketError; assert.ok(Date.now() < deadline, label + JSON.stringify(wire.slice(-5))); await delay(20); }
    }
    await waitFor(() => wire.some(f => f.type === "ready"), "socket ready");
    async function turn(marker) {
      const id = crypto.randomUUID(), start = wire.length;
      socket.send(JSON.stringify({ type: "prompt", id, input: marker }));
      await waitFor(() => wire.slice(start).some(f => f.id === id && ["turn_completed", "turn_failed", "turn_cancelled"].includes(f.type)), marker);
      assert.equal(wire.find(f => f.id === id && f.type.startsWith("turn_") && f.type !== "turn_accepted")?.type, "turn_completed");
      const results = wire.slice(start).filter(f => f.event?.type === "tool.result").map(f => f.event.payload);
      assert.ok(results.every(r => r.status === "completed"), JSON.stringify(results));
      trace.push({ marker, turn_id: id, results }); return results;
    }
    const setup = await turn("CRM_SETUP");
    const saved = setup.filter(r => r.tool === "crm_save").map(r => r.structured_result.record);
    assert.equal(saved.length, 7);
    const expectedIds = saved.slice(0, 2).map(r => r.id).sort();
    const graphId = setup.find(r => r.tool === "crm_graph").structured_result.node.id;
    for (const q of queries) {
      const path = `/v1/crm?q=${encodeURIComponent(q)}&tag=fixture&limit=1`;
      const first = await call(path), second = await call(path + `&cursor=${first.next_cursor}`);
      assert.deepEqual([first.records[0].id, second.records[0].id].sort(), expectedIds, q);
      assert.equal(second.next_cursor, null);
      assert.ok(first.records[0].name.includes("Χ"), "stored spelling is unchanged");
    }
    const page = await call("/v1/crm?q=giannis&limit=1");
    await call(`/v1/crm?q=nikos&cursor=${page.next_cursor}`, 400);
    await call(`/v1/crm?q=giannis&cursor=${page.next_cursor}`, 400, otherToken);
    assert.deepEqual((await call("/v1/crm?q=giannis", 200, otherToken)).records, []);
    await call("/v1/crm?q=giannis", 401, "");
    await call("/v1/crm?q=giannis", 403, deniedToken);
    await call("/v1/crm?q=giannis&owner_id=someone", 400);
    for (const q of ["[%_*?]", "\\"]) assert.deepEqual((await call(`/v1/crm?q=${encodeURIComponent(q)}`)).records.map(r => r.id), [saved[2].id]);
    for (const q of ["literal not present", "\u0301", "[%_*?]x"]) assert.deepEqual((await call(`/v1/crm?q=${encodeURIComponent(q)}`)).records, []);
    for (const q of ["ΜΑΙΟΣ", "maios", "α".repeat(90), "a".repeat(90)]) assert.deepEqual((await call(`/v1/crm?q=${encodeURIComponent(q)}`)).records.map(r => r.id), [saved[3].id]);
    for (const [q, index] of [["Pavlos", 4], ["Evangelos", 5], ["Spiros", 6], ["Spyros", 6]]) {
      assert.deepEqual((await call(`/v1/crm?q=${q}`)).records.map(r => r.id), [saved[index].id]);
    }
    const toolSearch = await turn("CRM_SEARCH");
    assert.equal(toolSearch.filter(r => r.tool === "crm_search").length, queries.length);
    for (const result of toolSearch.filter(r => r.tool === "crm_search")) assert.ok(expectedIds.includes(result.structured_result.records[0].id));
    const graphs = toolSearch.filter(r => r.tool === "crm_graph");
    for (const result of graphs.slice(0, queries.length)) assert.ok(result.structured_result.nodes.some(n => n.id === saved[0].graph_node_id));
    assert.ok(!graphs[0].structured_result.nodes.some(n => n.id === graphId), "freeform graph text keeps literal search; normalization expands record anchors only");
    assert.notEqual(graphs.at(-1).structured_result.nodes[0].id, graphs.at(-2).structured_result.nodes[0].id);
    for (let i = 0; i < 5; i++) await call("/v1/crm?q=zzzz-no-such-contact");
    await turn("CRM_EDIT");
    assert.deepEqual((await call("/v1/crm?q=giannis")).records, []);
    assert.deepEqual((await call("/v1/crm?q=nikos")).records.map(r => r.id).sort(), expectedIds);
    assert.deepEqual((await call("/v1/crm?q=papadopoulos")).records.map(r => r.id).sort(), expectedIds);
    const detail = await call(`/v1/crm/${expectedIds[0]}`);
    assert.deepEqual(detail.identities, [], "normalization must not create aliases");
    const timings = trace.filter(t => t.path === "/v1/crm?q=zzzz-no-such-contact").map(t => t.milliseconds);
    console.log(JSON.stringify({ evidence: output, records: 2007, graph_nodes: 10008, miss_latency_ms: timings, max_ms: Math.max(...timings) }));
  } finally {
    socket?.terminate(); await mf.dispose();
    await writeFile(join(output, "trace.json"), JSON.stringify({ command: "node --test test/crm-greek-search-journey.test.mjs", trace, wire }, null, 2) + "\n");
    await writeFile(join(output, "runtime.log"), logs.join("\n") + "\n");
  }
});
