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
import { fetch } from "./support/miniflare-fetch.mjs";

// Real account ingress, managed HTTP/WS, Code Mode, session ownership and
// encrypted Vault broker. Only enrollment, the external model and merchant are
// synthetic. No private broker or session handler is replaced.
const root = fileURLToPath(new URL("..", import.meta.url));
const output = join(root, "../../output/provider-vault-journey", `${Date.now()}-${process.pid}`);
const source = `
import { DurableObject } from 'cloudflare:workers';
import worker, { DurableAgentSession, AccountHostedTools, ManagedAgentOwnership } from './src/index.ts';
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey } from './src/account-auth.ts';
import { Kv } from 'accounts/server';
export { DurableAgentSession, AccountHostedTools, ManagedAgentOwnership, UserAccount, Organization, ApiKeyRecord, NonceStorage };
const info = console.info.bind(console);
console.info = (record, ...rest) => info(record && typeof record === 'object' ? JSON.stringify(record) : record, ...rest);
export class FixtureModel extends DurableObject {
  async fetch(request) {
    if (new URL(request.url).pathname === '/__configure') {
      this.calls = (await request.json()).calls; this.inputs = []; this.version=(this.version??0)+1; return new Response(null,{status:204});
    }
    if (new URL(request.url).pathname === '/__model-history') return Response.json(this.inputs ?? []);
    if (request.headers.get('upgrade') !== 'websocket') return Response.json({tools:[],machines:[],connections:[]});
    const [client, server] = Object.values(new WebSocketPair()); server.accept(); let index = 0, version=this.version;
    server.addEventListener('close', () => server.close(1000));
    server.addEventListener('message', event => {
      if(version!==this.version){index=0;version=this.version;}
      this.inputs.push(JSON.parse(event.data));
      const input = this.calls[index++];
      const output = input ? [{type:'custom_tool_call',name:'exec',call_id:'vault-'+index,input}]
        : [{type:'message',role:'assistant',content:[{type:'output_text',text:'VAULT_JOURNEY_DONE'}]}];
      server.send(JSON.stringify({type:'response.completed',response:{id:'resp_vault_'+index,status:'completed',end_turn:!input,
        output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}}}));
    });
    return new Response(null,{status:101,webSocket:client});
  }
}
export default { async fetch(request, env, ctx) {
  if (['/__configure','/__model-history'].includes(new URL(request.url).pathname)) return env.MODEL.getByName('fixture-model').fetch(request);
  if (new URL(request.url).pathname === '/__fixture') {
    const b = await request.json(); await ensureAccount(env,b.user,true);
    const auth = await (await env.NANOCODEX_USERS.getByName(b.user).fetch('https://user.internal/authorization')).json();
    const key = await createApiKey(env,{kind:'api_key',userId:b.user,...auth.grant,subjectId:'api_key:'+b.user,
      credentialId:'fixture',capabilities:b.capabilities},'Synthetic Vault journey');
    const token = 's_' + crypto.randomUUID().replaceAll('-','') + 'A'.repeat(11);
    await Kv.durableObject(env.NANOCODEX_AUTH,{name:'account'}).set('session:'+token,
      {userId:b.user,authentication:'sms_otp',issuedAt:Date.now()/1000,expiresAt:Date.now()/1000+3600});
    return Response.json({...key,cookie:'nanocodex_account='+token});
  }
  return worker.fetch(request,env,ctx);
}};
`;

async function bundle(contents, resolveDir, provenance, aliases = {}) {
  const assets = [];
  const result = await build({ stdin: { contents, resolveDir }, bundle: true, write: false,
    format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    banner: { js: 'import { createRequire } from "node:module"; const require=createRequire("/worker.mjs");' },
    external: ["cloudflare:*", "node:*"], alias: { "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs"), ...aliases },
    plugins: [{ name: "wasm", setup(builder) {
      builder.onResolve({ filter: /(?:\.wasm$|^nanocodex\/wasm$)/ }, async args => {
        const path = args.path === "nanocodex/wasm" ? join(root, "../nanocodex/pkg-web/nanocodex_bg.wasm") : join(args.resolveDir, args.path);
        const contents = await readFile(path), name = `fixture-${assets.length}.wasm`;
        assets.push({ type: "CompiledWasm", path: name, contents });
        provenance.push({ path, sha256: createHash("sha256").update(contents).digest("hex") });
        return { path: `./${name}`, external: true };
      });
    } }], logLevel: "silent",
  });
  return [{ type: "ESModule", path: "worker.mjs", contents: result.outputFiles[0].text }, ...assets];
}

// All outbound traffic is intercepted. No provider charge or real issuer request.
test("private provider capture survives pending recovery through authenticated HTTP and registered runtime tools", { timeout: 120_000 }, async () => {
  await mkdir(output, { recursive: true });
  const wire = [], trace = [], logs = [], provenance = [], mcp = [], downstream = [];
  const token = "SENTINEL_PROVIDER_TOKEN_DO_NOT_EXPOSE", pan = "4111111111111111";
  const refresh = "SENTINEL_REFRESH_TOKEN_DO_NOT_EXPOSE";
  const privateValues = [token, pan, refresh, "SENTINEL_GITHUB_TOKEN"];
  let reads = 0, socket;
  const modules = await bundle(source, root, provenance);
  const proxy = await bundle(`import {routeManaged} from '../account/worker/managedProxy.ts'; export default {async fetch(request,env){return await routeManaged(request,env,new URL(request.url)) ?? new Response(null,{status:404})}}`, root, provenance);
  const egress = await bundle(`export { default, AgentSubjectDirectory, UserCredentialBroker, UserConnectorBroker } from './src/egress.ts';`, join(root, "../egress"), provenance,
    { "@whiskeysockets/baileys": join(root, "../egress/src/whatsapp-generated/baileys.js") });
  const job = "99999999-9999-4999-8999-999999999999", cardJob = "88888888-8888-4888-8888-888888888888", unknownJob = "77777777-7777-4777-8777-777777777777";
  const outbound = async request => {
    const url = new URL(request.url);
    if (url.href === "https://github.com/login/oauth/access_token") {
      const form = new URLSearchParams(await request.text());
      assert.equal(form.get("code"), "synthetic-native-code");
      assert.ok(form.get("code_verifier")?.length >= 43);
      downstream.push({method:request.method,path:url.pathname});
      return Response.json({access_token:"SENTINEL_GITHUB_TOKEN", token_type:"bearer",scope:"repo,workflow"});
    }
    if (url.href === "https://api.github.com/applications/synthetic-client/token") {
      assert.equal(request.method,"DELETE");
      assert.equal((await request.json()).access_token,"SENTINEL_GITHUB_TOKEN");
      return new Response(null,{status:204});
    }
    if (url.href === "https://api.github.com/user") {
      assert.equal(request.headers.get("authorization"), "Bearer SENTINEL_GITHUB_TOKEN");
      return Response.json({id:123456,login:"synthetic-native",name:"Synthetic native"});
    }
    if (url.origin === "https://laso.finance") {
      if (url.pathname === "/refresh-card-data") {
        assert.equal(request.method, "POST");
        assert.equal(request.headers.get("authorization"), `Bearer ${token}`);
        downstream.push({ method: request.method, path: url.pathname });
        return Response.json({ success: true });
      }
      assert.equal(url.pathname, "/get-card-data"); assert.equal(request.method, "GET");
      assert.equal(request.headers.get("authorization"), `Bearer ${token}`);
      assert.equal(url.searchParams.get("card_id"), "synthetic-card");
      downstream.push({ method: request.method, path: url.pathname });
      if (++reads === 1) return Response.json({ card_id: "synthetic-card", status: "pending" });
      return Response.json({ card_id: "synthetic-card", status: "ready", last_updated_timestamp: Math.floor(Date.now()/1000),
        card_details: { card_number: pan, cvv: "987", exp_month: "09", exp_year: "2031", available_balance: 4.25, billing_address: { zip: "10001" } } });
    }
    assert.ok(["https://mercator.sh", "https://developers.openai.com", "https://mcp.tempo.xyz", "https://docs.mcp.cloudflare.com", "https://viem.sh", "https://vocs.dev"].includes(url.origin), "unexpected external destination: " + url.origin);
    if (request.method === "GET") return new Response(null, { status: 405 });
    if (request.method === "DELETE") return new Response(null, { status: 204 });
    const b = await request.json();
    let result;
    if (b.method === "initialize") result = { protocolVersion: b.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: "synthetic-provider", version: "1" } };
    else if (b.method === "notifications/initialized") return new Response(null, { status: 202 });
    else if (b.method === "tools/list") result = { tools: ["create_job", "get_job", "list_jobs"].map(name => ({ name, description: name, inputSchema: { type: "object", additionalProperties: true } })) };
    else if (b.method === "tools/call") {
      mcp.push({ name: b.params.name, arguments: b.params.arguments });
      const isCard = b.params.arguments.plan?.nodes?.[0]?.id === "card" || [cardJob,unknownJob].includes(b.params.arguments.job_id);
      const data = b.params.name === "quote_plan" ? {totalAmount:isCard ? "5" : "0",validUntil:new Date(Date.now()+60000).toISOString()} : isCard ? {job:{jobId:b.params.arguments.job_id ?? cardJob,status:b.params.name === "create_job" ? "pending" : "succeeded",result:{card:{user_id:"synthetic-user",auth:{id_token:token,refresh_token:refresh,expires_in:"3600"},card:{card_id:"synthetic-card"}}}}} : b.params.name === "list_jobs" ? { jobs: [{ jobId: job, status: "succeeded", summary: "ordinary-history" }] }
        : { job: { jobId: job, status: "succeeded", result: { weather: "ordinary-weather-result" } } };
      result = { content: [{ type: "text", text: JSON.stringify(data) }], structuredContent: data };
    } else throw Error("unexpected MCP method");
    return Response.json({ jsonrpc: "2.0", id: b.id, result });
  };
  const common = { compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"] };
  const options = { port: 0, durableObjectsPersist: join(output, "state/do"), r2BucketsPersist: join(output, "state/r2"), handleRuntimeStdio(stdout, stderr) {
    for (const stream of [stdout, stderr]) createInterface({ input: stream }).on("line", line => logs.push(line));
  }, workers: [
    { ...common, name: "account", modules: proxy, serviceBindings: { NANOCODEX_BACKEND: "managed" } },
    { ...common, name: "managed", modules, bindings: { MANAGED_AGENT_DIRECT_CREDENTIALS: "true" }, outboundService: outbound, durableObjects: {
      NANOCODEX_SESSIONS: { className: "DurableAgentSession", useSQLite: true }, NANOCODEX_USERS: { className: "UserAccount", useSQLite: true },
      NANOCODEX_ORGANIZATIONS: { className: "Organization", useSQLite: true }, NANOCODEX_API_KEYS: { className: "ApiKeyRecord", useSQLite: true },
      NANOCODEX_AUTH: { className: "NonceStorage", useSQLite: true }, NANOCODEX_ACCOUNT_TOOLS: { className: "AccountHostedTools", useSQLite: true },
      NANOCODEX_MEMORY: { className: "FixtureModel", useSQLite: true }, MODEL: { className: "FixtureModel", useSQLite: true },
    }, serviceBindings: { NANOCODEX: "provider" }, r2Buckets: ["NANOCODEX_HISTORY", "NANOCODEX_WORKSPACES"] },
    { ...common, name: "provider", modules: true, script: `export default {async fetch(request,env){
      if(new URL(request.url).hostname === 'broker.internal') return env.EGRESS.fetch(request);
      return env.MODEL.getByName('fixture-model').fetch(request);
    }};`, serviceBindings: { EGRESS: "egress" }, durableObjects: { MODEL: { className: "FixtureModel", scriptName: "managed", useSQLite: true } } },
    { ...common, name: "egress", modules: egress, bindings: { ENVIRONMENT: "test", GITHUB_OAUTH_CLIENT_ID: "synthetic-client", GITHUB_OAUTH_CLIENT_SECRET: "synthetic-secret" }, outboundService: outbound,
      durableObjects: { USER_CREDENTIALS: { className: "UserCredentialBroker", useSQLite: true }, AGENT_SUBJECTS: { className: "AgentSubjectDirectory", useSQLite: true }, USER_CONNECTORS: { className: "UserConnectorBroker", useSQLite: true } },
      serviceBindings: { MANAGED_AGENT_OWNERSHIP: { name: "managed", entrypoint: "ManagedAgentOwnership" } } },
  ] };
  let mf = new Miniflare(options);
  try {
    let base = await mf.ready, backend = await mf.getWorker("managed");
    const owner = "11111111-1111-4111-8111-111111111111", capabilities = ["agents:read", "agents:write", "tools:use", "api_keys:write"];
    const login = await (await backend.fetch("https://fixture.test/__fixture", { method: "POST", body: JSON.stringify({ user: owner, capabilities }) })).json();
    privateValues.push(login.token, login.cookie.split("=")[1]);
    const connectHeaders = { "x-nanocodex-connect-user": owner, "x-nanocodex-connect-grant-id": "0x"+"2".repeat(64),
      "x-nanocodex-connect-capabilities": JSON.stringify(capabilities.filter(value => value !== "api_keys:write")), "x-nanocodex-connect-connectors": JSON.stringify(["chatgpt"]), "x-nanocodex-connect-mcp-ids": "[]", "content-type": "application/json" };
    // Public ingress never accepts a caller's private broker capture route.
    for (const headers of [{ authorization: `Bearer ${login.token}` }, connectHeaders]) {
      const denied = await backend.fetch(`https://nanocodex.internal/users/${owner}/credentials/provider-capture`, { method: "POST", headers, body: "{}" });
      assert.notEqual(denied.status, 200); assert.notEqual(denied.status, 201);
      trace.push({ case: "private_capture_route_unavailable_over_public_ingress", status: denied.status });
    }
    // The exact account API used by native clients: secrets only enter the
    // private POST body, and list/save/delete receipts never echo them.
    async function native(path, input, method = "POST", auth = login) {
      const response = await fetch(new URL(path, base), { method,
        headers: { authorization: `Bearer ${auth.token}`, "content-type": "application/json" },
        ...(input === undefined ? {} : { body: JSON.stringify(input) }) });
      const body = response.status === 204 ? null : await response.json();
      trace.push({ case: "native_account_api", path, method, status: response.status, body });
      return { status: response.status, body };
    }
    const vaultEntries = [
      ["login", { name: "Synthetic login", username: "person@example.test", password: "SENTINEL_NATIVE_PASSWORD", browser_origin: "https://example.com" }],
      ["api_key", { name: "Synthetic API", api_key: "SENTINEL_NATIVE_API_KEY" }],
      ["card", { name: "Synthetic card", card_number: pan, expiry_month: "09", expiry_year: "2031", billing_zip: "10001" }],
      ["address", { name: "Synthetic address", address_line_1: "1 Example Street", city: "Example", state: "NY", zip: "10001", country: "US" }],
      ["phone", { name: "Synthetic phone", phone_number: "+12025550123" }],
    ];
    privateValues.push("SENTINEL_NATIVE_PASSWORD", "SENTINEL_NATIVE_API_KEY");
    for (const [kind, payload] of vaultEntries) {
      const created = await native(`/v1/credentials/vault/${kind}`, payload);
      assert.equal(created.status, 201, JSON.stringify(created));
      const listed = await native("/v1/credentials", undefined, "GET");
      assert.equal(listed.status, 200); assert.ok(listed.body.vault.some(entry => entry.id === created.body.id));
      if (kind === "login") {
        assert.equal((await native(`/v1/credentials/vault/login/${created.body.id}/origin`, {browser_origin:"https://example.org"}, "PUT")).status, 200);
      }
      assert.equal((await native(`/v1/credentials/vault/${kind}/${created.body.id}`, undefined, "DELETE")).status, 204);
    }
    const sshTarget = { generate: true, hostname: "example.com", port: 22, username: "synthetic", host_key_sha256: "SHA256:"+"A".repeat(43) };
    assert.equal((await native("/v1/credentials/ssh/native-example", sshTarget, "PUT")).status, 204);
    const ssh = await native("/v1/credentials", undefined, "GET");
    assert.ok(ssh.body.ssh.some(target => target.reference === "native-example" && target.public_key));
    assert.equal((await native("/v1/credentials/ssh/native-example", sshTarget, "PUT")).status, 409, "generation never silently rotates");
    assert.equal((await native("/v1/credentials/ssh/native-example", undefined, "DELETE")).status, 204);
    for (const provider of ["chatgpt", "claude"]) {
      assert.equal((await native(`/v1/credentials/${provider}`, undefined, "DELETE")).status, provider === "chatgpt" ? 204 : 200);
    }
    assert.equal((await native("/v1/connectors/catalog", undefined, "GET")).status,200);
    const started = await native("/v1/connectors/github", {return_to:"/connectors"});
    assert.equal(started.status,200,JSON.stringify(started));
    const authorizationURL = new URL(started.body.authorization_url);
    assert.equal(authorizationURL.origin,"https://github.com");
    assert.equal(authorizationURL.searchParams.get("code_challenge_method"),"S256");
    const callbackPath = "/v1/connectors/github/callback?" + new URLSearchParams({state:authorizationURL.searchParams.get("state"),code:"synthetic-native-code"});
    // Native clients complete with their account key; no browser session cookie.
    const callback = await fetch(new URL(callbackPath,base),{headers:{authorization:`Bearer ${login.token}`}});
    assert.equal(callback.status,200); assert.ok((await callback.text()).includes('"result":"success"'));
    const overview = await native("/v1/connectors",undefined,"GET");
    assert.equal(overview.status,200); assert.equal(overview.body.connectors.github.connected,true);
    const connectionID = overview.body.connectors.github.connections[0].id;
    const exchanges = downstream.length;
    const replay = await fetch(new URL(callbackPath,base),{headers:{authorization:`Bearer ${login.token}`}});
    assert.equal(replay.status,200); assert.ok((await replay.text()).includes('"result":"error"'));
    assert.equal(downstream.length,exchanges,"one-use OAuth state prevents another exchange");
    assert.equal((await native(`/v1/connectors/github/connections/${connectionID}`,undefined,"DELETE")).status,204);
    assert.equal((await native("/v1/connectors",undefined,"GET")).body.connectors.github.connected,false);
    trace.push({case:"native_connector_start_callback_list_exact_disconnect",connected:true,replay_rejected:true});
    const operation = crypto.randomUUID();
    const calls = [
      `for(let i=0;i<50;i++){const r=await tools.tool_search({query:"mercator",limit:20}); if(r.tools.some(t=>t.name==="mcp__mercator__create_job")){text(r);break;} await new Promise(resolve=>setTimeout(resolve,100));}`,
      `text(await tools.mcp__mercator__create_job({plan:{nodes:[{id:"card",serviceId:"x402-laso-finance-9ad65ae7",method:"GET",path:"/get-card",input:{amount:5,format:"json"}}]},idempotency_key:"synthetic-issuance-no-charge",approved_total:"5"}));`,
      `const r=await tools.mcp__mercator__get_job({job_id:"${cardJob}"}); text(r); store("capture",r.structuredContent ?? JSON.parse(r.content[0].text));`,
      `const saved=await tools.vault_store({capture_id:load("capture").capture_id,operation_id:"${operation}"}); text(saved); store("saved",saved);`,
      `text(await tools.provider_card({operation:"balance",vault_id:load("saved").vault_id}));`,
      `text(await tools.mcp__mercator__create_job({plan:{nodes:[{id:"weather",serviceId:"ordinary-weather",method:"GET",path:"/weather",input:{}}]},idempotency_key:"synthetic-weather-job",approved_total:"0"}));`,
      `text(await tools.mcp__mercator__get_job({job_id:"${job}"}));`,
      `text(await tools.mcp__mercator__list_jobs({}));`,
    ];
    calls.push(`text(await tools.mcp__mercator__get_job({job_id:"${unknownJob}"}));`);
    calls.push(calls[1]); // Same issuance idempotency key recovers locally without a second charge.
    const allModelHistory = [];
    async function runSession(sessionCalls, prompt, auth = login) {
      await backend.fetch("https://fixture.test/__configure", { method: "POST", body: JSON.stringify({ calls: sessionCalls }) });
      const response = await fetch(new URL("/v1/agents", base), { method: "POST", headers: { authorization: `Bearer ${auth.token}`, "content-type": "application/json" }, body: JSON.stringify({ settings: { model: "gpt-6.1-sol", thinking: "low", reasoning_mode: "standard", fast_mode: false } }) });
      assert.equal(response.status, 201); const created = await response.json();
      const offset = wire.length;
      socket = new WebSocket(new URL(`/v1/agents/${created.agent_id}/ws`, base).href.replace(/^http/, "ws"), { headers: { authorization: `Bearer ${auth.token}` } });
      socket.on("message", data => wire.push(JSON.parse(String(data)))); let socketError; socket.on("error", e => { socketError = e; });
      async function waitFor(predicate, label) { const deadline = Date.now()+60_000; while(!predicate()) { if(socketError) throw socketError; assert.ok(Date.now()<deadline, label+": "+JSON.stringify(wire.slice(-5))); await delay(20); } }
      await waitFor(() => wire.slice(offset).some(f => f.type === "ready"), "ready"); const turn = crypto.randomUUID();
      socket.send(JSON.stringify({ type: "prompt", id: turn, input: prompt }));
      await waitFor(() => wire.some(f => f.id === turn && ["turn_completed", "turn_failed"].includes(f.type)), "provider journey");
      assert.equal(wire.find(f => f.id === turn && ["turn_completed", "turn_failed"].includes(f.type)).type, "turn_completed");
      allModelHistory.push(await (await backend.fetch("https://fixture.test/__model-history")).json());
      socket.terminate(); socket = undefined;
      return created;
    }
    const original = await runSession(calls.slice(0,3), "Issue the synthetic Laso card through default Mercator and capture its result for automatic private saving.");
    assert.equal(reads, 1, "automatic save reaches issuer pending without an explicit vault_store call");
    const pending = wire.filter(f => f.event?.type === "tool.result" && f.event.payload.tool === "mcp__mercator__get_job").at(-1)?.event.payload.structured_result;
    const pendingReceipt = pending?.structuredContent ?? JSON.parse(pending?.content?.[0]?.text ?? "null");
    assert.equal(pendingReceipt?.status, "awaiting_card");
    assert.ok(pendingReceipt.capture_id);
    await mf.dispose();
    mf = new Miniflare(options);
    base = await mf.ready; backend = await mf.getWorker("managed");
    trace.push({case:"disk_persisted_runtime_restart_after_pending_auto_save",capture_id:pendingReceipt.capture_id});
    const created = await runSession([calls[0], `const saved=await tools.provider_card({operation:"status",capture_id:"${pendingReceipt.capture_id}"}); text(saved); store("saved",saved); store("capture",saved);`, ...calls.slice(3)], "In this new conversation, resume the existing synthetic Laso job, save privately, inspect its balance, and exercise ordinary jobs.");
    assert.notEqual(original.agent_id,created.agent_id);
    trace.push({case:"new_conversation_pending_recovery",original:original.agent_id,resumed:created.agent_id});
    const results = wire.filter(f => f.event?.type === "tool.result" && ["vault_store", "provider_card"].includes(f.event.payload.tool)).map(f => f.event.payload.structured_result);
    trace.push({ results }); assert.equal(results.length, 3, JSON.stringify(results));
    assert.equal(results[0].status, "saved", "status resumes pending auto-save after runtime restart");
    assert.equal(results[0].vault_id, results[1].vault_id, "explicit save recovers the automatically saved entry");
    assert.equal(results[2].balance, 4.25);
    assert.ok(JSON.stringify(wire).includes("awaiting_card"));
    assert.equal(mcp.filter(c => c.name === "create_job").length, 2, "one explicit issuance and one ordinary job; never retry issuance");
    assert.equal(mcp.filter(c => c.name === "list_jobs").length, 1);
    assert.ok(JSON.stringify(wire).includes("ordinary-weather-result"));
    assert.ok(JSON.stringify(wire).includes("mercator_private_job_unavailable"),"unbound private result must be suppressed");
    assert.ok(JSON.stringify(wire).includes("resume_existing_job"),"explicit repeat issuance recovers the same job locally");
    assert.ok(!wire.some(f=>f.event?.type === "tool.result" && f.event.payload.status === "failed"),"all intended tool actions must succeed");
    const savedByAPI = await native("/v1/vault/store", { capture_id: results[0].capture_id, operation_id: operation });
    assert.equal(savedByAPI.status, 200); assert.equal(savedByAPI.body.vault_id, results[0].vault_id);
    assert.deepEqual((await native("/v1/vault/store", {capture_id: results[0].capture_id, operation_id: operation})).body, savedByAPI.body);
    const apiBalance = await native("/v1/vault/card", {operation:"balance",vault_id:results[0].vault_id});
    assert.equal(apiBalance.status, 200); assert.equal(apiBalance.body.balance, 4.25);
    const refreshID = crypto.randomUUID();
    const refreshByAPI = await native("/v1/vault/card", {operation:"refresh",vault_id:results[0].vault_id,operation_id:refreshID});
    assert.equal(refreshByAPI.status, 200); assert.equal(refreshByAPI.body.status, "balance_pending");
    const count = downstream.length;
    assert.equal((await native("/v1/vault/card", {operation:"refresh",vault_id:results[0].vault_id,operation_id:refreshID})).body.status,"balance_pending");
    assert.equal(downstream.length, count, "replayed refresh does not dispatch twice");
    assert.equal((await native("/v1/vault/card", {operation:"refresh",vault_id:results[0].vault_id})).status,400);
    assert.equal((await native("/v1/vault/card", {operation:"balance",vault_id:results[0].vault_id,card_number:pan})).status,400);
    const anonymous = await fetch(new URL("/v1/vault/card",base),{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({operation:"status",capture_id:results[0].capture_id})});
    assert.equal(anonymous.status,401);
    const csrf = await fetch(new URL("/v1/vault/card",base),{method:"POST",headers:{cookie:login.cookie,origin:"https://wrong.example","content-type":"application/json"},body:JSON.stringify({operation:"status",capture_id:results[0].capture_id})});
    assert.equal(csrf.status,403);
    const delegated = await backend.fetch("https://nanocodex.internal/v1/vault/card",{method:"POST",headers:connectHeaders,body:JSON.stringify({operation:"status",capture_id:results[0].capture_id})});
    assert.ok([401,403].includes(delegated.status));
    trace.push({case:"public_provider_auth_boundaries",anonymous:anonymous.status,csrf:csrf.status,connect:delegated.status});
    const otherOwner = "22222222-2222-4222-8222-222222222222";
    const other = await (await backend.fetch("https://fixture.test/__fixture", {method:"POST",body:JSON.stringify({user:otherOwner,capabilities})})).json();
    privateValues.push(other.token,other.cookie.split("=")[1]);
    assert.equal((await native("/v1/vault/card", {operation:"balance",vault_id:results[0].vault_id}, "POST", other)).status,404);
    assert.equal((await native("/v1/vault/store", {capture_id:results[0].capture_id,operation_id:crypto.randomUUID()}, "POST", other)).status,404);

    const isolationWireOffset = wire.length, isolationReads = downstream.length, isolationMcp = mcp.length;
    await runSession([
      `text(await tools.vault_store({capture_id:"${results[0].capture_id}",operation_id:"${crypto.randomUUID()}"}));`,
      `text(await tools.provider_card({operation:"balance",vault_id:"${results[0].vault_id}"}));`,
      calls[0], `text(await tools.mcp__mercator__get_job({job_id:"${cardJob}"}));`,
    ], "Check references belonging to a different account without issuing anything.", other);
    const isolationResults = wire.slice(isolationWireOffset).filter(f=>f.event?.type==="tool.result" && ["vault_store","provider_card"].includes(f.event.payload.tool)).map(f=>f.event.payload.structured_result);
    assert.equal(isolationResults.length,2);
    assert.ok(isolationResults.every(r=>r.error === "capture_not_found"),JSON.stringify(isolationResults));
    assert.ok(JSON.stringify(wire.slice(isolationWireOffset)).includes("mercator_private_job_unavailable"));
    assert.equal(downstream.length,isolationReads);
    assert.deepEqual(mcp.slice(isolationMcp), [{name:"get_job",arguments:{job_id:cardJob}}], "other owner cannot issue or capture; fetched unknown job stays private");
    const otherHistory = await fetch(new URL(`/v1/agents/${created.agent_id}/events/history`,base),{headers:{authorization:`Bearer ${other.token}`}});
    assert.ok([403,404].includes(otherHistory.status));
    trace.push({case:"authenticated_other_owner_cannot_use_capture_vault_or_job_or_history",results:isolationResults,history_status:otherHistory.status});
    const beforeDenied = downstream.length, beforeMcp = mcp.length;
    await backend.fetch("https://fixture.test/__configure", {method:"POST",body:JSON.stringify({calls:[
      `try{text(await tools.vault_store({capture_id:"${results[0].capture_id}",operation_id:"${crypto.randomUUID()}"}));}catch(e){text({error:e.message});}`,
      `try{text(await tools.provider_card({operation:"balance",vault_id:"${results[0].vault_id}"}));}catch(e){text({error:e.message});}`,
    ]})});
    const deniedTurn=crypto.randomUUID();
    const denied = await backend.fetch(`https://nanocodex.internal/v1/agents/${created.agent_id}/turns`, {
      method: "POST", headers: connectHeaders, body: JSON.stringify({id:deniedTurn,input:"Use vault_store and provider_card on this account"}),
    });
    assert.equal(denied.status,202);
    const deadline=Date.now()+30000;
    for (;;) {
      const state=await (await backend.fetch(`https://nanocodex.internal/v1/agents/${created.agent_id}/turns/${deniedTurn}`,{headers:connectHeaders})).json();
      if(["completed","failed","cancelled"].includes(state.state)) break;
      assert.ok(Date.now()<deadline,JSON.stringify(state)); await delay(50);
    }
    const connectHistory=await (await fetch(new URL(`/v1/agents/${created.agent_id}/events/history`,base), {headers:{authorization:`Bearer ${login.token}`}})).json();
    const failures=connectHistory.data.filter(f=>f.turn_id===deniedTurn&&f.event?.type==="tool.result"&&["vault_store","provider_card"].includes(f.event.payload.tool));
    assert.equal(failures.length,2,JSON.stringify(connectHistory));
    assert.ok(failures.every(f=>f.event.payload.status==="failed" && /forbidden|full account/i.test(JSON.stringify(f.event.payload))),JSON.stringify(failures));
    trace.push({case:"Connect admitted but private provider tools denied",status:denied.status,failures});
    allModelHistory.push(await (await backend.fetch("https://fixture.test/__model-history")).json());
    assert.equal(downstream.length,beforeDenied); assert.equal(mcp.length,beforeMcp);
    trace.push({ modelHistory: allModelHistory });
    const history = await fetch(new URL(`/v1/agents/${created.agent_id}/events/history`,base), {headers:{authorization:`Bearer ${login.token}`}});
    assert.equal(history.status,200); trace.push({ history:await history.json() });
    console.log(JSON.stringify({ evidence: output, real_managed_and_egress: true, pending_recovered: true, balance: results[2].balance, default_mcp_capture: true, disk_restart: true, owner_isolation: true }));
  } finally {
    socket?.terminate(); await mf.dispose();
    const evidence = JSON.stringify({ command: "node --test js/managed/test/provider-vault-journey.test.mjs", provenance, trace, wire, mcp, downstream }, null, 2);
    const runtime = logs.join("\n");
    for (const value of privateValues) { assert.ok(!evidence.includes(value), "model, HTTP, history and evidence must not disclose private values"); assert.ok(!runtime.includes(value), "runtime logs must not disclose private values"); }
    await writeFile(join(output,"trace.json"),evidence+"\n"); await writeFile(join(output,"runtime.log"),runtime+"\n");
  }
});
