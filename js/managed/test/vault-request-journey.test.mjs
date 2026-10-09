import assert from "node:assert/strict";
import { createHash, createHmac } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import WebSocket from "ws";

// Real account ingress, managed HTTP/WS, Code Mode, session ownership and
// encrypted Vault broker. Only enrollment, the external model and merchant are
// synthetic. The binding fault fixture replaces replies only for failure cases.
const root = fileURLToPath(new URL("..", import.meta.url));
const output = join(root, "../../output/vault-request-journey", `${Date.now()}-${process.pid}`);
const secret = "synthetic-vault-journey-key-DO-NOT-EXPOSE";
const responseSecret = "synthetic-merchant-response-DO-NOT-EXPOSE";
const message = "POST /charge\norder=synthetic-order-17";
const signature = createHmac("sha256", secret).update(message).digest("hex");
const source = `
import { DurableObject } from 'cloudflare:workers';
import worker, { DurableAgentSession, AccountHostedTools, ManagedAgentOwnership } from './src/index.ts';
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey, resolveChiefOfStaffPrincipal } from './src/account-auth.ts';
import { Kv } from 'accounts/server';
import { routeVaultRequest } from './src/vault-request.ts';
export { DurableAgentSession, AccountHostedTools, ManagedAgentOwnership, UserAccount, Organization, ApiKeyRecord, NonceStorage };
const info = console.info.bind(console);
console.info = (record, ...rest) => info(record && typeof record === 'object' ? JSON.stringify(record) : record, ...rest);
export class FixtureModel extends DurableObject {
  async fetch(request) {
    if (new URL(request.url).pathname === '/__configure') {
      this.calls = (await request.json()).calls; return new Response(null,{status:204});
    }
    if (request.headers.get('upgrade') !== 'websocket') return Response.json({tools:[],machines:[],connections:[]});
    const [client, server] = Object.values(new WebSocketPair()); server.accept(); let index = 0;
    server.addEventListener('close', () => server.close(1000));
    server.addEventListener('message', () => {
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
  if (new URL(request.url).pathname === '/__configure') return env.MODEL.getByName('fixture-model').fetch(request);
  // Service principals have no public HTTP authenticator for this endpoint.
  // This narrow integration fixture obtains one from the real account resolver,
  // then invokes the same route used by index.ts, without mocking its authority.
  if (new URL(request.url).pathname === '/__service-vault-request') {
    const principal = await resolveChiefOfStaffPrincipal(env, request.headers.get('x-fixture-owner'), 'chief:' + 'a'.repeat(64));
    if (!principal) throw new Error('synthetic service principal unavailable');
    const response = await routeVaultRequest(new Request(new URL('/v1/vault/request', request.url), request), env.NANOCODEX, principal);
    response.headers.set('x-fixture-principal-kind', principal.kind);
    response.headers.set('x-fixture-capabilities', JSON.stringify(principal.capabilities));
    return response;
  }
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

test("Vault requests preserve owner authority and status-only results through HTTP and Code Mode", { timeout: 120_000 }, async () => {
  await mkdir(output, { recursive: true });
  const trace = [], wire = [], logs = [], provenance = [], downstream = [], brokerCalls = [];
  const privateValues = [secret, responseSecret, signature];
  const modules = await bundle(source, root, provenance);
  const proxy = await bundle(`import {routeManaged} from '../account/worker/managedProxy.ts';
    export default {async fetch(request,env){return await routeManaged(request,env,new URL(request.url)) ?? new Response(null,{status:404})}}`, root, provenance);
  const egress = await bundle(`export { default, AgentSubjectDirectory, UserCredentialBroker } from './src/egress.ts';`, join(root, "../egress"), provenance,
    { "@whiskeysockets/baileys": join(root, "../egress/src/whatsapp-generated/baileys.js") });
  const common = { compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"] };
  const mf = new Miniflare({ port: 0, handleRuntimeStdio(stdout, stderr) {
    createInterface({ input: stdout }).on("line", line => logs.push(line));
    createInterface({ input: stderr }).on("line", line => logs.push(line));
  }, workers: [
    { ...common, name: "account", modules: proxy, serviceBindings: { NANOCODEX_BACKEND: "managed" } },
    { ...common, name: "managed", modules, bindings: { MANAGED_AGENT_DIRECT_CREDENTIALS: "true" }, durableObjects: {
      NANOCODEX_SESSIONS: { className: "DurableAgentSession", useSQLite: true },
      NANOCODEX_USERS: { className: "UserAccount", useSQLite: true },
      NANOCODEX_ORGANIZATIONS: { className: "Organization", useSQLite: true },
      NANOCODEX_API_KEYS: { className: "ApiKeyRecord", useSQLite: true },
      NANOCODEX_AUTH: { className: "NonceStorage", useSQLite: true },
      NANOCODEX_ACCOUNT_TOOLS: { className: "AccountHostedTools", useSQLite: true },
      NANOCODEX_MEMORY: { className: "FixtureModel", useSQLite: true }, MODEL: { className: "FixtureModel", useSQLite: true },
    }, serviceBindings: { NANOCODEX: "provider" }, r2Buckets: ["NANOCODEX_HISTORY", "NANOCODEX_WORKSPACES"] },
    { ...common, name: "provider", modules: true, script: `export default {async fetch(request,env){
      if(new URL(request.url).hostname === 'vault-egress.internal') {
        const fault = await env.FAULT.fetch(request.clone()); if(fault.status !== 204) return fault;
        return env.EGRESS.fetch(request);
      }
      if(new URL(request.url).hostname === 'broker.internal') return env.EGRESS.fetch(request);
      return env.MODEL.getByName('fixture-model').fetch(request);
    }};`, serviceBindings: { EGRESS: "egress", FAULT: async request => {
      const input = await request.json();
      brokerCalls.push({ endpoint: request.url, subject: request.headers.get("x-nanocodex-subject"), target: input.url });
      if (input.url.endsWith("/broker-corrupt")) return new Response(responseSecret, { status: 503 });
      if (input.url.endsWith("/broker-error")) return Response.json({ error: "vault_entry_unavailable", details: responseSecret }, { status: 409 });
      if (input.url.endsWith("/broker-oversized")) return new Response(responseSecret.repeat(200), { status: 200 });
      if (input.url.endsWith("/broker-lost")) throw new Error("synthetic binding disconnect");
      return new Response(null, { status: 204 });
    } }, durableObjects: { MODEL: { className: "FixtureModel", scriptName: "managed", useSQLite: true } } },
    { ...common, name: "egress", modules: egress, bindings: { ENVIRONMENT: "test" },
      durableObjects: { USER_CREDENTIALS: { className: "UserCredentialBroker", useSQLite: true }, AGENT_SUBJECTS: { className: "AgentSubjectDirectory", useSQLite: true } },
      serviceBindings: { MANAGED_AGENT_OWNERSHIP: { name: "managed", entrypoint: "ManagedAgentOwnership" } },
      outboundService: async request => {
        const url = new URL(request.url);
        assert.equal(url.origin, "https://merchant.example.com", "journey must never contact real services");
        const signed = url.pathname === "/signed";
        assert.ok(signed ? request.headers.get("x-signature") === signature : request.headers.get("authorization") === `Bearer ${secret}`,
          "destination must receive the broker-injected credential");
        if (signed) assert.equal(await request.text(), "order=synthetic-order-17");
        const status = url.pathname === "/redirect" ? 307 : url.pathname === "/rejected" ? 401 : 201;
        downstream.push({ path: url.pathname, method: request.method, signature_verified: signed, credential_verified: true, status });
        return new Response(responseSecret, { status, headers: { "set-cookie": `session=${responseSecret}`, "x-secret": secret,
          location: `https://merchant.example.com/never-follow/${responseSecret}` } });
      } },
  ] });
  let socket;
  try {
    const base = await mf.ready, backend = await mf.getWorker("managed"), broker = await mf.getWorker("egress");
    const owner = "11111111-1111-4111-8111-111111111111", otherOwner = "22222222-2222-4222-8222-222222222222";
    async function enroll(user, capabilities) {
      const response = await backend.fetch("https://fixture.test/__fixture", { method: "POST", body: JSON.stringify({ user, capabilities }) });
      assert.equal(response.status, 200); return response.json();
    }
    const capabilities = ["agents:read", "agents:write", "tools:use"];
    const alice = await enroll(owner, capabilities), bob = await enroll(otherOwner, capabilities);
    const noTools = await enroll(owner, ["agents:read", "agents:write"]), noWrite = await enroll(owner, ["agents:read", "tools:use"]);
    for (const login of [alice, bob, noTools, noWrite]) privateValues.push(login.token, login.cookie.split("=")[1]);
    const provisioned = await broker.fetch(`https://broker.internal/users/${owner}/credentials/vault/api_key`, {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ name: "Synthetic merchant", api_key: secret }),
    });
    assert.equal(provisioned.status, 201);
    const { id: vault_id } = await provisioned.json();
    const input = { vault_id, url: "https://merchant.example.com/charge", method: "POST", headers: { authorization: "Bearer {{NANOCODEX_VAULT_API_KEY}}" } };
    const signedInput = { ...input, url: "https://merchant.example.com/signed", headers: { "x-signature": "{{NANOCODEX_VAULT_SIGNATURE}}" },
      body: "order=synthetic-order-17", signing: { algorithm: "HMAC-SHA256", message, encoding: "hex" } };
    async function call(path, { body = input, token = alice.token, cookie, origin, expected = 200, method = "POST", headers = {} } = {}) {
      const response = await fetch(new URL(path, base), { method, headers: {
        ...(token ? { authorization: `Bearer ${token}` } : {}), ...(cookie ? { cookie } : {}),
        "content-type": "application/json", ...(origin ? { origin: origin === "same" ? base.origin : origin } : {}), ...headers,
      }, ...(["GET", "HEAD", "DELETE"].includes(method) ? {} : { body: JSON.stringify(body) }) });
      const text = await response.text(), data = text ? JSON.parse(text) : null;
      trace.push({ path, method, expected, observed: response.status,
        authentication: cookie ? "browser_session" : token ? "bearer" : "none", origin,
        ...(path === "/v1/vault/request" ? { input: typeof body.body === "string" && body.body.length > 4096
          ? { ...body, body: `[${body.body.length} bytes]` } : body } : {}),
        result: path.endsWith("share-links") ? "share URL redacted" : data });
      assert.equal(response.status, expected, `${method} ${path}: ${text}`);
      if (path === "/v1/vault/request") {
        for (const header of ["set-cookie", "x-secret", "location", "authorization"]) assert.equal(response.headers.get(header), null);
        for (const value of [secret, responseSecret, signature]) assert.ok(!text.includes(value), "HTTP response must not disclose secrets or upstream body");
      }
      return data;
    }
    const vault = options => call("/v1/vault/request", options);
    await vault({ token: null, expected: 401 });
    await vault({ token: noTools.token, expected: 403 });
    await vault({ token: noWrite.token, expected: 403 });
    await vault({ token: null, cookie: alice.cookie, expected: 403 });
    await vault({ token: null, cookie: alice.cookie, origin: "https://forged.example.com", expected: 403 });
    assert.equal(brokerCalls.length, 0, "authorization failures must not contact the broker");
    assert.deepEqual(await vault({ token: bob.token, expected: 409 }), { error: "vault_entry_unavailable" });
    assert.equal(downstream.length, 0, "foreign owner cannot use an opaque Vault ID");
    assert.deepEqual(await vault({ headers: { "x-nanocodex-user-id": otherOwner, "x-nanocodex-subject": "Z".repeat(64) } }), { status: 201, ok: true });
    assert.equal(brokerCalls.at(-1).endpoint, `https://vault-egress.internal/v1/users/${owner}/request`, "authenticated owner determines scope");
    assert.equal(brokerCalls.at(-1).subject, null, "user supplied subject is not authority");
    assert.deepEqual(await vault({ token: null, cookie: alice.cookie, origin: "same", body: signedInput }), { status: 201, ok: true });
    for (const [path, status] of [["redirect", 307], ["rejected", 401]]) {
      const before = downstream.length;
      assert.deepEqual(await vault({ body: { ...input, url: `https://merchant.example.com/${path}` } }), { status, ok: false });
      assert.equal(downstream.length, before + 1, "no redirect follow or HTTP rejection retry");
    }
    const beforeInvalid = brokerCalls.length;
    await vault({ body: { ...input, owner: otherOwner }, expected: 400 });
    await vault({ body: { ...input, vault_id: "invalid" }, expected: 400 });
    await vault({ body: { ...input, body: "x".repeat(100_000) }, expected: 400 });
    assert.equal(brokerCalls.length, beforeInvalid, "invalid envelopes fail before dispatch");
    for (const [path, expected, error] of [["broker-corrupt", 502, "vault_request_outcome_unknown"], ["broker-oversized", 502, "vault_request_outcome_unknown"],
      ["broker-lost", 502, "vault_request_outcome_unknown"], ["broker-error", 409, "vault_entry_unavailable"]]) {
      const before = brokerCalls.length;
      assert.deepEqual(await vault({ body: { ...input, url: `https://merchant.example.com/${path}` }, expected }), { error });
      assert.equal(brokerCalls.length, before + 1, "corrupt or lost broker replies must never trigger an automatic retry");
    }
    // Trusted Connect ingress authenticates a real grant, not a mocked Principal.
    const beforeConnect = brokerCalls.length;
    const connect = await backend.fetch("https://nanocodex.internal/v1/vault/request", { method: "POST", headers: {
      "x-nanocodex-connect-user": owner, "x-nanocodex-connect-grant-id": "0x" + "2".repeat(64),
      "x-nanocodex-connect-capabilities": JSON.stringify(capabilities), "x-nanocodex-connect-connectors": "[]",
      "x-nanocodex-connect-mcp-ids": "[]", "content-type": "application/json",
    }, body: JSON.stringify(input) });
    trace.push({ case: "connect_authority_rejected", expected: 403, observed: connect.status, result: await connect.json() });
    assert.equal(connect.status, 403); assert.equal(brokerCalls.length, beforeConnect);
    // The otherwise valid request has matching origin and both required scopes.
    // Its rejection must come from the service-kind boundary, before dispatch.
    const beforeService = brokerCalls.length;
    const service = await backend.fetch(new URL("/__service-vault-request", base), { method: "POST", headers: {
      "x-fixture-owner": owner, origin: base.origin, "content-type": "application/json",
    }, body: JSON.stringify(input) });
    const serviceKind = service.headers.get("x-fixture-principal-kind");
    const serviceCapabilities = JSON.parse(service.headers.get("x-fixture-capabilities"));
    const serviceResult = await service.json();
    trace.push({ case: "service_authority_rejected", boundary: "real account resolver and route integration; no public service authenticator",
      principal_kind: serviceKind, capabilities: serviceCapabilities, origin: base.origin, input,
      expected: 403, observed: service.status, result: serviceResult });
    assert.equal(serviceKind, "service");
    assert.ok(["agents:write", "tools:use"].every(capability => serviceCapabilities.includes(capability)));
    assert.equal(service.status, 403);
    assert.deepEqual(serviceResult, { error: "forbidden" });
    assert.equal(brokerCalls.length, beforeService, "service authority must never contact the broker");
    const created = await call("/v1/agents", { body: { settings: { model: "gpt-6.1-sol", thinking: "low", reasoning_mode: "standard", fast_mode: false } }, expected: 201 });
    const shared = await call(`/v1/agents/${created.agent_id}/share-links`, { body: { permission: "write" }, expected: 201 });
    const guest = new URLSearchParams(new URL(shared.url).hash.slice(1)).get("token");
    assert.ok(guest?.startsWith("nsl_")); privateValues.push(guest); await vault({ token: guest, expected: 401 });
    const configured = await backend.fetch("https://fixture.test/__configure", { method: "POST", body: JSON.stringify({ calls: [
      `text(await tools.vault_request(${JSON.stringify(signedInput)}));`,
      `text(await tools.vault_request(${JSON.stringify({ ...input, url: "https://merchant.example.com/broker-corrupt" })}));`,
    ] }) });
    assert.equal(configured.status, 204);
    socket = new WebSocket(new URL(`/v1/agents/${created.agent_id}/ws`, base).href.replace(/^http/, "ws"), { headers: { authorization: `Bearer ${alice.token}` } });
    socket.on("message", data => wire.push(JSON.parse(String(data))));
    let socketError; socket.on("error", error => { socketError = error; });
    async function waitFor(predicate, label) {
      const deadline = Date.now() + 40_000;
      while (!predicate()) {
        if (socketError) throw socketError;
        assert.ok(Date.now() < deadline, label + ": " + JSON.stringify(wire.slice(-8))); await delay(20);
      }
    }
    await waitFor(() => wire.some(frame => frame.type === "ready"), "WebSocket ready");
    const turn = crypto.randomUUID(), beforeTool = brokerCalls.length, beforeDestination = downstream.length;
    socket.send(JSON.stringify({ type: "prompt", id: turn, input: "Use the synthetic saved Vault item for the authorized journey requests." }));
    await waitFor(() => wire.some(frame => frame.id === turn && ["turn_completed", "turn_failed", "turn_cancelled"].includes(frame.type))
      || wire.some(frame => frame.type === "error"), "Code Mode Vault turn");
    const terminal = wire.find(frame => frame.id === turn && ["turn_completed", "turn_failed", "turn_cancelled"].includes(frame.type));
    assert.equal(terminal?.type, "turn_completed", JSON.stringify(terminal ?? wire.slice(-8)));
    const results = wire.filter(frame => frame.event?.type === "tool.result" && frame.event.payload.tool === "vault_request").map(frame => frame.event.payload);
    assert.equal(results.length, 2, JSON.stringify(wire.slice(-8)));
    assert.deepEqual(results.map(result => result.structured_result), [{ status: 201, ok: true }, { error: "vault_request_outcome_unknown" }]);
    assert.equal(brokerCalls.length, beforeTool + 2, "Code Mode dispatches once for each explicit invocation");
    assert.equal(downstream.length, beforeDestination + 1);
    assert.ok(brokerCalls.slice(beforeTool).every(call => call.endpoint === "https://vault-egress.internal/v1/request" && /^managed-session-v1_[0-9a-f]{64}$/.test(call.subject)),
      "Code Mode uses the session subject resolved by production ManagedAgentOwnership");
    // A socket upgraded with the live key, then revoked before its first
    // prompt, must reject that first prompt: upgrade authority is never reused.
    const fresh = new WebSocket(new URL(`/v1/agents/${created.agent_id}/ws`, base).href.replace(/^http/, "ws"), { headers: { authorization: `Bearer ${alice.token}` } });
    const freshWire = []; fresh.on("message", data => freshWire.push(JSON.parse(String(data))));
    await waitFor(() => freshWire.some(frame => frame.type === "ready"), "second WebSocket ready");
    await call(`/v1/api-keys/${alice.metadata.id}`, { method: "DELETE", token: null, cookie: alice.cookie, origin: "same", expected: 204 });
    const firstAfterRevoke = crypto.randomUUID();
    fresh.send(JSON.stringify({ type: "prompt", id: firstAfterRevoke, input: "First prompt after the key was revoked." }));
    await waitFor(() => freshWire.some(frame => frame.type === "error"), "revoked first prompt rejected");
    assert.equal(freshWire.find(frame => frame.type === "error").code, "login_unavailable");
    assert.ok(!freshWire.some(frame => frame.type === "turn_accepted" && frame.id === firstAfterRevoke));
    fresh.terminate();
    const beforeRevoked = brokerCalls.length; await vault({ expected: 401 }); assert.equal(brokerCalls.length, beforeRevoked);
    const revokedTurn = crypto.randomUUID(), beforeFrames = wire.length;
    socket.send(JSON.stringify({ type: "prompt", id: revokedTurn, input: "Repeat the synthetic journey after revocation." }));
    await waitFor(() => wire.slice(beforeFrames).some(frame => frame.type === "error"), "revoked socket rejected");
    assert.equal(wire.slice(beforeFrames).find(frame => frame.type === "error").code, "login_unavailable");
    assert.ok(!wire.some(frame => frame.type === "turn_accepted" && frame.id === revokedTurn)); assert.equal(brokerCalls.length, beforeRevoked);
    trace.push({ case: "code_mode", signed_request_verified: true, results: results.map(result => result.structured_result), revoked_socket_rejected: true });
    console.log(JSON.stringify({ evidence: output, real_managed_and_egress: true, signed_requests: downstream.filter(call => call.signature_verified).length,
      http_and_code_mode: true, service_principal_rejected: true, automatic_retries: 0, revoked_key_rejected: true }));
  } finally {
    socket?.terminate(); await mf.dispose();
    const wasmBuild = JSON.parse(await readFile(join(root, "../nanocodex/pkg-web/nanocodex-build.json"), "utf8"));
    const evidence = JSON.stringify({ command: "pnpm --dir js/managed test:vault-requests", provenance, wasmBuild, trace, wire, downstream, brokerCalls }, null, 2);
    const runtime = logs.join("\n");
    for (const value of privateValues) {
      assert.ok(!evidence.includes(value), "transcript must not contain secrets, signatures or merchant responses");
      assert.ok(!runtime.includes(value), "runtime logs must not contain secrets, signatures or merchant responses");
    }
    await writeFile(join(output, "trace.json"), evidence + "\n"); await writeFile(join(output, "runtime.log"), runtime + "\n");
  }
});
