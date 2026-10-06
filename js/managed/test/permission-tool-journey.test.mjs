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
import WebSocket from "ws";

// Only first identity enrollment and the external model are fixtures. Account
// ingress, key validation, session admission, WASM/Code Mode, permission consent,
// and user data all run their shipped implementation over actual HTTP and WS.
const root = fileURLToPath(new URL("..", import.meta.url));
const evidenceRoot = join(root, "../../output/permission-tool-journey", `${Date.now()}-${process.pid}`);
const operationId = "00000000-0000-4000-8000-000000000061";
const dataKey = "com.example.permission-journey/proof";
const source = `
import { DurableObject } from 'cloudflare:workers';
import worker, { DurableAgentSession, AccountHostedTools } from './src/index.ts';
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey } from './src/account-auth.ts';
import { Kv } from 'accounts/server';
export { DurableAgentSession, AccountHostedTools, UserAccount, Organization, ApiKeyRecord, NonceStorage };
export { UserDataScope } from './src/user-data-scope.ts';
const info = console.info.bind(console);
console.info = (record, ...rest) => info(record && typeof record === 'object' ? JSON.stringify(record) : record, ...rest);
export class FixtureModel extends DurableObject {
  async fetch(request) {
    if (new URL(request.url).pathname === '/__resume') {
      this.released = true; this.resume?.(); this.resume = undefined;
      return new Response(null,{status:204});
    }
    if (request.headers.get('upgrade') !== 'websocket') return Response.json({tools:[],machines:[],connections:[]});
    const [client, server] = Object.values(new WebSocketPair()); server.accept();
    let scenario = '', index = 0;
    server.addEventListener('close', () => server.close(1000));
    server.addEventListener('message', event => {
      const body = JSON.parse(event.data);
      const user = JSON.stringify((body.input ?? []).filter(item => item.role === 'user').at(-1));
      const next = ['PERMISSION_BEFORE', 'PERMISSION_AFTER', 'PERMISSION_ACTIVE'].find(marker => user?.includes(marker));
      if (next && next !== scenario) { scenario = next; index = 0; this.released = false; }
      const before = scenario === 'PERMISSION_BEFORE';
      const beforeCalls = [
        'text(await tools.user_data({operation:"document_put",key:"${dataKey}",value:"must not be written"}));',
        'text(await tools.request_permissions({operation:"request",operation_id:"${operationId}",capabilities:["data:read","data:write"],reason:"Save and retrieve the synthetic permission journey document"}));',
        'text(await tools.user_data({operation:"document_put",key:"${dataKey}",value:"still forbidden while pending"}));',
      ];
      const afterCalls = [
        'text(await tools.user_data({operation:"document_put",key:"${dataKey}",value:{approved:true}}));',
        'text(await tools.user_data({operation:"document_get",key:"${dataKey}"}));',
      ];
      const calls = scenario === 'PERMISSION_ACTIVE' ? [...beforeCalls,
        'text(await tools.request_permissions({operation:"status",request_id:"${operationId}"}));', ...afterCalls]
        : before ? beforeCalls : afterCalls;
      const input = calls[index++];
      const responseOutput = input ? [{type:'custom_tool_call',name:'exec',call_id:scenario+'-'+index,input}]
        : [{type:'message',role:'assistant',content:[{type:'output_text',text:scenario+'_DONE'}]}];
      const send = () => server.send(JSON.stringify({type:'response.completed',response:{id:'resp_'+scenario+'_'+index,status:'completed',end_turn:!input,
        output:responseOutput,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
      // Hold only the external model reply; the real managed turn stays active
      // across browser approval, then executes its actual status tool call.
      if (scenario === 'PERMISSION_ACTIVE' && index === 4 && !this.released) this.resume = send;
      else send();
    });
    return new Response(null,{status:101,webSocket:client});
  }
}
export default { async fetch(request, env, ctx) {
  if (new URL(request.url).pathname === '/__resume') return env.MODEL.getByName('fixture-model').fetch(request);
  if (new URL(request.url).pathname === '/__fixture') {
    const b = await request.json(); await ensureAccount(env,b.user,true);
    const auth = await (await env.NANOCODEX_USERS.getByName(b.user).fetch('https://user.internal/authorization')).json();
    const key = await createApiKey(env,{kind:'api_key',userId:b.user,...auth.grant,subjectId:'api_key:'+b.user,
      credentialId:'fixture',capabilities:['agents:read','agents:write','tools:use',...(b.manageKeys ? ['api_keys:write'] : [])]},'Synthetic permission journey');
    const token = 's_' + crypto.randomUUID().replaceAll('-','') + 'A'.repeat(11);
    await Kv.durableObject(env.NANOCODEX_AUTH,{name:'account'}).set('session:'+token,
      {userId:b.user,authentication:'sms_otp',issuedAt:Date.now()/1000,expiresAt:Date.now()/1000+3600});
    return Response.json({...key,cookie:'nanocodex_account='+token});
  }
  return worker.fetch(request,env,ctx);
}};
`;

for (const keyKind of ["key-management", "legacy", "active-status"]) test(`${keyKind} login uses owner browser consent on the same open WebSocket`, { timeout: 90_000 }, async () => {
  const activeStatus = keyKind === "active-status";
  const output = join(evidenceRoot, keyKind);
  await mkdir(output, { recursive: true });
  const trace = [], wire = [], logs = [], assets = [], provenance = [];
  const bundle = await build({ stdin: { contents: source, resolveDir: root }, bundle: true, write: false,
    format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
    external: ["cloudflare:*", "node:*"], alias: { "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
    plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /\.wasm$/ }, async args => {
      const path = join(args.resolveDir, args.path), contents = await readFile(path);
      const name = `fixture-${assets.length}.wasm`;
      assets.push({ type: "CompiledWasm", path: name, contents });
      provenance.push({ path, sha256: createHash("sha256").update(contents).digest("hex") });
      return { path: `./${name}`, external: true };
    }); } }], logLevel: "silent",
  });
  const proxy = await build({ stdin: { contents: `import {routeManaged} from '../account/worker/managedProxy.ts';
    export default {async fetch(request,env){return await routeManaged(request,env,new URL(request.url)) ?? new Response(null,{status:404})}}`, resolveDir: root },
    bundle: true, write: false, format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    external: ["cloudflare:*", "node:*"], logLevel: "silent" });
  const common = { compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"] };
  const mf = new Miniflare({ port: 0, handleRuntimeStdio(stdout, stderr) {
    createInterface({ input: stdout }).on("line", line => logs.push(line));
    createInterface({ input: stderr }).on("line", line => logs.push(line));
  }, workers: [
    { ...common, name: "account", modules: true, script: proxy.outputFiles[0].text, serviceBindings: { NANOCODEX_BACKEND: "managed" } },
    { ...common, name: "managed", modules: [{ type: "ESModule", path: "worker.mjs", contents: bundle.outputFiles[0].text }, ...assets],
      durableObjects: {
        NANOCODEX_SESSIONS: { className: "DurableAgentSession", useSQLite: true },
        NANOCODEX_USERS: { className: "UserAccount", useSQLite: true },
        NANOCODEX_ORGANIZATIONS: { className: "Organization", useSQLite: true },
        NANOCODEX_API_KEYS: { className: "ApiKeyRecord", useSQLite: true },
        NANOCODEX_AUTH: { className: "NonceStorage", useSQLite: true },
        NANOCODEX_USER_DATA: { className: "UserDataScope", useSQLite: true },
        NANOCODEX_ACCOUNT_TOOLS: { className: "AccountHostedTools", useSQLite: true },
        NANOCODEX_MEMORY: { className: "FixtureModel", useSQLite: true },
        MODEL: { className: "FixtureModel", useSQLite: true },
      }, serviceBindings: { NANOCODEX: "provider" },
      r2Buckets: ["NANOCODEX_HISTORY", "NANOCODEX_WORKSPACES", "NANOCODEX_USER_DATA_OBJECTS"] },
    { ...common, name: "provider", modules: true,
      script: "export default {fetch(request,env){return env.MODEL.getByName('fixture-model').fetch(request)}};",
      durableObjects: { MODEL: { className: "FixtureModel", scriptName: "managed", useSQLite: true } } },
  ] });
  let socket;
  try {
    const base = await mf.ready, backend = await mf.getWorker("managed");
    const enrolled = await backend.fetch("https://fixture.test/__fixture", { method: "POST", body: JSON.stringify({ user: crypto.randomUUID(), manageKeys: keyKind === "key-management" }) });
    assert.equal(enrolled.status, 200, await enrolled.clone().text());
    const { token, cookie, metadata } = await enrolled.json();
    async function call(path, method = "GET", body, expected = 200, browser = false) {
      const response = await fetch(new URL(path, base), { method, headers: {
        "content-type": "application/json", ...(browser ? { cookie, origin: base.origin } : { authorization: "Bearer " + token }),
      }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
      const text = await response.text(); let data; try { data = JSON.parse(text); } catch { data = text; }
      trace.push({ path, method, expected, status: response.status, data });
      assert.equal(response.status, expected, method + " " + path + ": " + text); return data;
    }
    await call("/v1/data", "POST", { operation: "document_get", key: dataKey }, 403);
    const created = await call("/v1/agents", "POST", { settings: { model: "gpt-6.1-sol", thinking: "low", reasoning_mode: "standard", fast_mode: false } }, 201);
    socket = new WebSocket(new URL(`/v1/agents/${created.agent_id}/ws`, base).href.replace(/^http/, "ws"), { headers: { authorization: "Bearer " + token, "x-nanocodex-api-key-id": "spoofedKey99", "x-nanocodex-api-key-object-id": "a".repeat(64) } });
    socket.on("message", data => wire.push(JSON.parse(String(data))));
    let socketError;
    socket.on("error", error => { socketError = error; });
    async function waitFor(predicate, label) {
      const deadline = Date.now() + 30_000;
      while (!predicate()) {
        if (socketError) throw socketError;
        assert.ok(Date.now() < deadline, label + ": " + JSON.stringify(wire.slice(-10)));
        await delay(20);
      }
    }
    await waitFor(() => wire.some(frame => frame.type === "ready"), "WebSocket ready");
    function startTurn(marker) {
      const id = crypto.randomUUID();
      socket.send(JSON.stringify({ type: "prompt", id, input: marker }));
      return id;
    }
    async function completeTurn(id, marker) {
      await waitFor(() => wire.some(frame => frame.id === id && ["turn_completed", "turn_failed", "turn_cancelled"].includes(frame.type))
        || wire.some(frame => frame.type === "error"), marker);
      const terminal = wire.find(frame => frame.id === id && ["turn_completed", "turn_failed", "turn_cancelled"].includes(frame.type));
      assert.equal(terminal?.type, "turn_completed", JSON.stringify(terminal ?? wire.slice(-5)));
      trace.push({ marker, final_message: terminal.final_message });
      return terminal.final_message;
    }
    const toolResults = tool => wire.filter(frame => frame.event?.type === "tool.result"
      && frame.event.payload.tool === tool).map(frame => frame.event.payload);
    const initialMarker = activeStatus ? "PERMISSION_ACTIVE" : "PERMISSION_BEFORE";
    const initialTurn = startTurn(initialMarker);
    if (activeStatus) {
      await waitFor(() => toolResults("user_data").length === 2 && toolResults("request_permissions").length === 1,
        "active turn waits for owner approval");
      assert.ok(!wire.some(frame => frame.type === "turn_completed"));
    } else await completeTurn(initialTurn, initialMarker);
    const denied = toolResults("user_data");
    assert.deepEqual(denied.map(result => result.status), ["failed", "failed"]);
    assert.ok(denied.every(result => /data:write/.test(result.result)), JSON.stringify(denied));
    const requests = toolResults("request_permissions");
    assert.equal(requests.length, 1);
    assert.equal(requests[0].status, "completed");
    const requested = requests[0].structured_result;
    assert.equal(requested.status, "pending");
    assert.equal(requested.request_id, operationId);
    assert.equal(requested.key_id, metadata.id, "untrusted key-ID assertion must be overwritten by authenticated provenance");
    assert.deepEqual(requested.capabilities, ["data:read", "data:write"]);
    assert.equal(new URL(requested.approval_url).origin, base.origin);
    // Read the real request from the public tool event, then approve through
    // account HTTP. No test-only mutation changes key capabilities.
    const requestPath = `/v1/permission-requests/${requested.key_id}/${requested.request_id}`;
    const pending = await call(requestPath);
    assert.equal(pending.status, "pending");
    assert.equal(pending.can_decide, false, "API keys cannot approve their own expansion");
    const browserPending = await call(requestPath, "GET", undefined, 200, true);
    assert.equal(browserPending.can_decide, true, "the signed-in owner can approve");
    assert.deepEqual(Object.keys(browserPending.capability_descriptions).sort(), ["data:read", "data:write"]);
    await call("/v1/data", "POST", { operation: "document_get", key: dataKey }, 403);
    // Even an API key carrying api_keys:write must use the owner approval page.
    await call(requestPath + "/approve", "POST", {}, 403);
    const approved = await call(requestPath + "/approve", "POST", {}, 200, true);
    assert.equal(approved.status, "approved");
    assert.equal((await call(requestPath)).status, "approved");
    assert.equal(socket.readyState, WebSocket.OPEN);
    if (activeStatus) {
      const resumed = await backend.fetch("https://fixture.test/__resume", { method: "POST" });
      assert.equal(resumed.status, 204);
      await completeTurn(initialTurn, initialMarker);
      const status = toolResults("request_permissions")[1];
      assert.equal(status.status, "completed");
      assert.equal(status.structured_result.status, "approved");
      assert.ok(wire.filter(frame => frame.event?.type === "tool.result").every(frame => frame.turn_id === initialTurn),
        "status refresh and successful tools must belong to the same admitted turn");
    } else {
      // The socket's original authorization attachment survives approval;
      // admission refreshes the exact key only for this next fresh user turn.
      await completeTurn(startTurn("PERMISSION_AFTER"), "PERMISSION_AFTER");
    }
    const successful = toolResults("user_data").slice(2);
    assert.deepEqual(successful.map(result => result.status), ["completed", "completed"]);
    assert.deepEqual(successful.map(result => result.structured_result.operation), ["document_put", "document_get"]);
    assert.ok(successful.every(result => result.structured_result.document.value.approved === true));
    const stored = await call("/v1/data", "POST", { operation: "document_get", key: dataKey });
    assert.deepEqual(stored.document.value, { approved: true });
    assert.equal(stored.document.version, 1, "both denied writes must leave storage unchanged");
    assert.equal(wire.filter(frame => frame.type === "ready").length, 1, "approval must not reconnect the client");
    assert.equal(wire.filter(frame => frame.type === "turn_completed").length, activeStatus ? 1 : 2);
    await call(`/v1/api-keys/${requested.key_id}`, "DELETE", undefined, 204, true);
    await call("/v1/data", "POST", { operation: "document_get", key: dataKey }, 401);
    const revokedTurn = crypto.randomUUID(), beforeRevoke = wire.length;
    socket.send(JSON.stringify({ type: "prompt", id: revokedTurn, input: "PERMISSION_AFTER revocation" }));
    await waitFor(() => wire.slice(beforeRevoke).some(frame => frame.type === "error"), "revoked socket rejected");
    const rejection = wire.slice(beforeRevoke).find(frame => frame.type === "error");
    assert.equal(rejection.code, "login_unavailable");
    assert.ok(!wire.some(frame => frame.type === "turn_accepted" && frame.id === revokedTurn));
    assert.equal(toolResults("user_data").length, 4, "revoked key must not invoke a new tool");
    console.log(JSON.stringify({ evidence: output, same_websocket: true, same_api_key: true,
      active_turn_status_refresh: activeStatus, spoofed_key_id_overwritten: true, revoked_key_rejected: true, document: stored.document }));
  } finally {
    socket?.terminate();
    await mf.dispose();
    let wasmBuild;
    try { wasmBuild = JSON.parse(await readFile(join(root, "../nanocodex/pkg-web/nanocodex-build.json"), "utf8")); } catch {}
    await writeFile(join(output, "trace.json"), JSON.stringify({ command: "node --test test/permission-tool-journey.test.mjs", provenance, wasmBuild, trace, wire }, null, 2) + "\n");
    await writeFile(join(output, "runtime.log"), logs.join("\n") + "\n");
  }
});
