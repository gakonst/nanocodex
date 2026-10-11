import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import WebSocket from "ws";
import { EXEC_COMMAND_PARAMETERS, EXECUTION_OUTPUT_SCHEMA } from "nanocodex-tools/execution-contract";
import { fetch } from "./support/miniflare-fetch.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const repo = fileURLToPath(new URL("../../../", import.meta.url));
const owner = "11111111-1111-4111-8111-111111111111";
const thread = "019b0000-0000-7000-8000-222222222222";
const principal = { kind: "api_key", userId: owner, organizationId: "22222222-2222-4222-8222-222222222222",
  teamId: "33333333-3333-4333-8333-333333333333", role: "owner", subjectId: `user:${owner}`,
  credentialId: "synthetic-paths", authorizationEpoch: 1, capabilities: ["agents:read", "agents:write", "tools:use"] };
// Historical rows as left by earlier deployments: a deleted workspace identity
// still holds the canonical root, so both live identities received suffixes.
const history = [["deleted-workspace", "/gak-9"], ["old-gak", "/gak-9-2"], ["mac-current", "/gak-9-3"]];
const source = `
import worker, { AccountHostedTools, DurableAgentSession } from './src/index.ts';
import { HandPaths } from './src/hand-paths.ts';
import { routeManaged } from '../account/worker/managedProxy.ts';
const rows = storage => {
  new HandPaths(storage);
  return { roots: storage.sql.exec('SELECT machine_id, root FROM managed_hand_paths ORDER BY machine_id').toArray(),
    aliases: storage.sql.exec('SELECT machine_id, root FROM managed_hand_path_aliases ORDER BY root').toArray() };
};
const seed = storage => {
  new HandPaths(storage);
  storage.sql.exec('DELETE FROM managed_hand_paths');
  for (const [id, root] of ${JSON.stringify(history)}) storage.sql.exec('INSERT INTO managed_hand_paths VALUES (?, ?)', id, root);
};
export class PathsAccount extends AccountHostedTools {
  async seedPaths() { seed(this.ctx.storage); }
  async paths() { return rows(this.ctx.storage); }
}
export class PathsSession extends DurableAgentSession {
  async seed() {
    await (await super.fetch(new Request("https://fixture.internal/__initialize"))).body?.cancel();
    this.ctx.storage.sql.exec("INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active) VALUES(1,?,?,?,?,1,'https://synthetic.example','managed',?)",
      '${thread}','${owner}','${principal.organizationId}','${principal.teamId}',Date.now());
    seed(this.ctx.storage);
  }
  async paths() { return rows(this.ctx.storage); }
}
export default {async fetch(request,env,ctx) {
  const path=new URL(request.url).pathname;
  // Fixture endpoints seed historical rows and expose durable rows as evidence only.
  if(path==='/__fixture/account-seed') { await env.NANOCODEX_ACCOUNT_TOOLS.getByName('${owner}').seedPaths(); return new Response(null,{status:204}); }
  if(path==='/__fixture/session') { await env.NANOCODEX_SESSIONS.getByName('${thread}').seed(); return new Response(null,{status:204}); }
  if(path==='/__fixture/account-paths') return Response.json(await env.NANOCODEX_ACCOUNT_TOOLS.getByName('${owner}').paths());
  if(path==='/__fixture/session-paths') return Response.json(await env.NANOCODEX_SESSIONS.getByName('${thread}').paths());
  // External account authentication is the only substituted public boundary.
  const auth=request.headers.get('authorization');
  if(!['paths','connect'].some(role=>auth==='Bearer synthetic-'+role)) return Response.json({error:'unauthorized'},{status:401});
  const actor=${JSON.stringify(principal)};
  if(auth==='Bearer synthetic-connect') actor.connectGrant={grantId:'synthetic-grant'};
  return await routeManaged(request, { NANOCODEX_BACKEND: {
    fetch: forwarded => worker.fetch(new Request(forwarded, { cf: { continent: "EU", country: "DE", longitude: "8.68" } }),env,ctx,actor),
  } }, new URL(request.url)) ?? Response.json({error:'not_found'}, {status:404});
}};
`;

for (const regional of [false, true]) test(`registry deletion releases canonical Hand roots (${regional ? "versioned" : "legacy"})`, { timeout: 90_000 }, async () => {
  const output = join(repo, "output/hand-paths-journey", `${Date.now()}-${process.pid}-${regional ? "regional" : "legacy"}`);
  await mkdir(output, { recursive: true });
  const http = [], wire = [], sockets = [], assets = [], calls = [];
  let wasmSequence = 0, mf, base, failure;
  const request = async (path, options = {}) => {
    const response = await fetch(new URL(path, base), { headers: { authorization: "Bearer synthetic-paths" },
      ...options, signal: AbortSignal.timeout(15_000) });
    const type = response.headers.get("content-type") ?? "";
    const value = type.includes("json") ? await response.json()
      : [...new Uint8Array(await response.arrayBuffer())];
    http.push({ path, method: options.method ?? "GET", actor: options.headers?.authorization ?? "default", status: response.status, value });
    return { status: response.status, value };
  };
  async function publish(id, name) {
    const socket = new WebSocket(new URL("/v1/account/tool-host", base).href.replace(/^http/, "ws"),
      { headers: { authorization: "Bearer synthetic-paths", ...(regional ? {
        "x-nanocodex-hand-machine-id": id, "x-nanocodex-hand-runtime-id": `${id}-runtime` } : {}) } });
    sockets.push(socket);
    await new Promise((resolve, reject) => { socket.once("open", resolve); socket.once("error", reject); });
    const ready = new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("catalog acknowledgement timed out")), 5_000);
      socket.once("close", (code, reason) => { clearTimeout(timer); reject(new Error(`catalog rejected: ${code} ${reason}`)); });
      socket.once("message", data => { clearTimeout(timer); resolve(JSON.parse(String(data))); });
    });
    socket.on("message", data => {
      const frame = JSON.parse(String(data));
      wire.push({ id, direction: "broker", frame });
      if (frame.type !== "call") return;
      calls.push({ device: id, name: frame.name, workdir: frame.input?.workdir, cmd: frame.input?.cmd });
      const result = "4\nAP9QSw==";
      socket.send(JSON.stringify({ type: "result", call_id: frame.call_id, outcome: { status: "completed", output: {
        output: result, success: true, structured_result: { output: result, exit_code: 0, wall_time_seconds: 0 }, metadata: null, process_trace: null } } }));
    });
    const catalog = { type: "catalog", attachment_id: id, ...(regional ? { runtime_id: `${id}-runtime` } : {}), capabilities: ["turn_metadata"],
      machines: [{ id, name, workspace: `/synthetic/${id}`, capabilities: ["native", "filesystem", "shell"] }],
      tools: [{ provider: "machine", remote_name: "exec_command", parallel_safe: true, timeout_ms: 30_000,
        definition: { type: "function", name: "exec_command", description: "Synthetic shell", strict: false,
          parameters: EXEC_COMMAND_PARAMETERS, output_schema: EXECUTION_OUTPUT_SCHEMA } }] };
    wire.push({ id, direction: "publisher", frame: catalog });
    socket.send(JSON.stringify(catalog));
    assert.equal((await ready).type, "ready");
    return socket;
  }
  const disconnect = socket => new Promise(resolve => { socket.once("close", resolve); socket.terminate(); });
  const hands = async () => (await request("/v1/account/hands")).value.data.map(({ id, workspace }) => ({ id, workspace }));
  const file = root => request(`/v1/agents/${thread}/files?${new URLSearchParams({ path: `${root}/out/a.zip` })}`);
  const accountPaths = async () => (await request("/__fixture/account-paths")).value;
  const sessionPaths = async () => (await request("/__fixture/session-paths")).value;
  const until = async (probe, accept, label) => {
    const deadline = Date.now() + 5_000;
    let value;
    do { value = await probe(); if (accept(value)) return value; await new Promise(resolve => setTimeout(resolve, 25)); }
    while (Date.now() < deadline);
    assert.fail(`${label}: ${JSON.stringify(value)}`);
  };
  try {
    const bundle = await build({ stdin: { contents: source, resolveDir: root }, bundle: true, write: false,
      format: "esm", platform: "node", conditions: ["workerd"], target: "es2022", external: ["cloudflare:*", "node:*"],
      alias: { "nanocodex-tools/hosted": join(repo, "js/nanocodex-tools/src/hosted/index.ts"),
        "node-rsa": join(repo, "js/nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
      plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /\.wasm$/ }, async args => {
        const path = join(args.resolveDir, args.path), name = `fixture-${wasmSequence++}.wasm`;
        assets.push({ type: "CompiledWasm", path: name, contents: await readFile(path) });
        return { path: `./${name}`, external: true };
      }); } }], logLevel: "silent" });
    mf = new Miniflare({ port: 0, durableObjectsPersist: join(output, "sqlite"),
      compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
      modules: [{ type: "ESModule", path: "worker.mjs", contents: bundle.outputFiles[0].text }, ...assets],
      durableObjects: { NANOCODEX_ACCOUNT_TOOLS: { className: "PathsAccount", useSQLite: true },
        NANOCODEX_SESSIONS: { className: "PathsSession", useSQLite: true } },
            r2Buckets: ["NANOCODEX_HISTORY", "NANOCODEX_WORKSPACES"],
      serviceBindings: { NANOCODEX: async request => {
        const path = new URL(request.url).pathname;
        if (path.startsWith("/subjects/")) return new Response(null, { status: 204 });
        if (path.endsWith("/catalog")) return Response.json({ connectors: {}, mcp_connections: [] });
        if (path.endsWith("/credentials/vault")) return Response.json({ vault: [] });
        return Response.json({ tools: [], machines: [], connections: [] });
      } } });
    base = await mf.ready;
    const current = await publish("mac-current", "gak-9");
    const old = await publish("old-gak", "gak-9");
    assert.equal((await request("/__fixture/account-seed", { method: "POST" })).status, 204);
    assert.equal((await request("/__fixture/session", { method: "POST" })).status, 204);
    await disconnect(old);
    // Offline is not deletion: the registered identity keeps its root while
    // the deleted workspace identity releases the canonical one.
    assert.deepEqual(await until(hands, list => list.length === 1, "old Hand offline"), [{ id: "mac-current", workspace: "/gak-9" }]);
    assert.deepEqual(await accountPaths(), { roots: [{ machine_id: "mac-current", root: "/gak-9" }, { machine_id: "old-gak", root: "/gak-9-2" }],
      aliases: [{ machine_id: "mac-current", root: "/gak-9-3" }] });
    // Restricted scope observes nothing and changes nothing.
    assert.equal((await request(`/v1/agents/${thread}/files?path=/gak-9/out/a.zip`, { headers: { authorization: "Bearer synthetic-connect" } })).status, 403);
    assert.equal((await request("/v1/account/hands/old-gak", { method: "DELETE", headers: { authorization: "Bearer synthetic-connect" } })).status, 403);
    const bytes = [0, 255, 80, 75];
    assert.deepEqual(await file("/gak-9"), { status: 200, value: bytes });
    assert.deepEqual(await file("/gak-9-3"), { status: 200, value: bytes }, "the same machine's historical path still works");
    assert.deepEqual(calls.map(call => [call.device, call.workdir]), [["mac-current", "/synthetic/mac-current"], ["mac-current", "/synthetic/mac-current"]]);
    const offline = await file("/gak-9-2");
    assert.equal(offline.status, 503, "an offline registered Hand is unavailable, never rerouted");
    // The offline identity's route fails admission; it is never unmapped or served by another device.
    assert.ok(["hand_unavailable", "file_read_failed"].includes(offline.value.error), offline.value.error);
    assert.equal(calls.length, 2);
    assert.deepEqual(await sessionPaths(), { roots: [{ machine_id: "mac-current", root: "/gak-9" }, { machine_id: "old-gak", root: "/gak-9-2" }],
      aliases: [{ machine_id: "mac-current", root: "/gak-9-3" }] });
    // Owner deletion is the authoritative release.
    assert.deepEqual(await request("/v1/account/hands/old-gak", { method: "DELETE" }), { status: 200, value: { forgotten: true } });
    assert.deepEqual(await accountPaths(), { roots: [{ machine_id: "mac-current", root: "/gak-9" }],
      aliases: [{ machine_id: "mac-current", root: "/gak-9-3" }] });
    const released = await file("/gak-9-2");
    assert.equal(released.status, 404);
    assert.equal(released.value.error, "file_path_unmapped");
    assert.deepEqual(await sessionPaths(), { roots: [{ machine_id: "mac-current", root: "/gak-9" }],
      aliases: [{ machine_id: "mac-current", root: "/gak-9-3" }] });
    // A new device reuses only the released root, never the current Hand's alias.
    await publish("new-gak", "gak-9");
    assert.deepEqual(await hands(), [{ id: "mac-current", workspace: "/gak-9" }, { id: "new-gak", workspace: "/gak-9-2" }]);
    assert.deepEqual(await file("/gak-9-2"), { status: 200, value: bytes });
    assert.deepEqual(await file("/gak-9-3"), { status: 200, value: bytes });
    assert.deepEqual(calls.slice(2).map(call => call.device), ["new-gak", "mac-current"]);
    await disconnect(current);
  } catch (error) { failure = error; throw error; }
  finally {
    for (const socket of sockets) socket.terminate();
    await mf?.dispose();
    await writeFile(join(output, "evidence.json"), JSON.stringify({
      command: "pnpm --filter nanocodex-managed-service run test:hand-paths", regional, history, http, calls, wire,
      expected: "deleted identity releases /gak-9 to the suffixed live Mac, which keeps /gak-9-3 as a working alias; offline registered Hand keeps /gak-9-2 and is unavailable, not rerouted; Connect scope is forbidden; owner deletion releases /gak-9-2 only to a new device",
      passed: !failure, error: failure?.stack }, null, 2));
    console.log(`Hand paths evidence: ${output}`);
  }
});
