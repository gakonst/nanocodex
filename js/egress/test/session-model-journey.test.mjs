import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { request as httpRequest } from "node:http";
import { join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

// Production service entrypoints, credential DO, encrypted storage, and RPC run
// in workerd. Only the external OAuth and model providers are synthetic.
const require = createRequire(import.meta.url);
const wranglerRequire = createRequire(require.resolve("wrangler/package.json"));
const { build } = wranglerRequire("esbuild");
const WebSocket = require("ws");
const { Miniflare, convertV4MiniflareOptions } = wranglerRequire("miniflare");
const directory = fileURLToPath(new URL("..", import.meta.url));
const repository = resolve(directory, "../..");
const output = join(repository, "output/egress-session-model-journey", `${Date.now()}-${process.pid}`);
const owner = "11111111-1111-4111-8111-111111111111";
const subject = `managed-session-v1_${"a".repeat(64)}`;
const jwt = value => `${Buffer.from('{"alg":"none"}').toString("base64url")}.${Buffer.from(JSON.stringify(value)).toString("base64url")}.fixture`;
function within(promise, ms, label) {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${label} exceeded ${ms}ms`)), ms);
  })]).finally(() => clearTimeout(timer));
}

test("Session model upload overlaps live refresh and preserves recovery, revocation, and private authority", { timeout: 45_000 }, async t => {
  await mkdir(output, { recursive: true });
  const bundle = await build({
    stdin: { contents: await readFile(process.env.EGRESS_JOURNEY_ENTRY ?? join(directory, "src/egress.ts"), "utf8"),
      resolveDir: join(directory, "src"), loader: "ts" }, bundle: true, write: false,
    format: "esm", platform: "node", target: "es2022", external: ["cloudflare:*"], logLevel: "warning",
    alias: {
      "node-rsa": join(repository, "js/nanocodex/tools/browser/unsupportedNodeRsa.mjs"),
      "@whiskeysockets/baileys": join(directory, "src/whatsapp-generated/baileys.js"),
    },
    plugins: [{ name: "static-wasm", setup(builder) {
      builder.onResolve({ filter: /^nanocodex\/wasm$/ }, () => ({ path: "./nanocodex.wasm", external: true }));
      builder.onResolve({ filter: /bridge\.wasm$/ }, () => ({ path: "./bridge.wasm", external: true }));
    } }],
  });
  const modules = [{ type: "ESModule", path: join(output, "egress.js"), contents: bundle.outputFiles[0].text },
    ...await Promise.all([
      ["nanocodex.wasm", join(repository, "js/nanocodex/pkg-web/nanocodex_bg.wasm")],
      ["bridge.wasm", join(directory, "src/whatsapp-generated/bridge.wasm")],
    ].map(async ([name, path]) => ({ type: "CompiledWasm", path: join(output, name), contents: await readFile(path) })))];
  const trace = [];
  const began = performance.now();
  const record = (kind, detail = {}) => trace.push({ kind, ...detail, elapsed_ms: Math.round(performance.now() - began) });
  let releaseRefresh, enteredRefresh, finishUpload;
  const refreshGate = new Promise(resolve => { releaseRefresh = resolve; });
  const refreshEntered = new Promise(resolve => { enteredRefresh = resolve; });
  const uploadFinished = new Promise(resolve => { finishUpload = resolve; });
  let refreshes = 0, dispatches = 0, rejectNext = false, rejectAlways = false;
  let expectedBody;
  const provider = async request => {
    const url = new URL(request.url);
    if (url.pathname === "/oauth/token") {
      refreshes++;
      const body = await request.json();
      assert.equal(body.grant_type, "refresh_token");
      assert.match(body.refresh_token, /^synthetic-refresh/);
      record("refresh_entered", { attempt: refreshes });
      enteredRefresh();
      if (refreshes === 1) await refreshGate;
      record("refresh_released", { attempt: refreshes });
      return Response.json({ access_token: jwt({ exp: Math.ceil(Date.now() / 1000) + 3600, generation: refreshes }),
        refresh_token: `synthetic-refresh-${refreshes}` });
    }
    const openai = url.hostname === "api.openai.com";
    assert.equal(url.href, openai ? "https://api.openai.com/v1/responses" : "https://chatgpt.com/backend-api/codex/responses");
    assert.equal(request.method, "POST");
    assert.equal(request.headers.get("chatgpt-account-id"), openai ? null : "synthetic-account");
    assert.equal(request.headers.get("originator"), openai ? null : "codex_cli_rs");
    if (openai) assert.equal(request.headers.get("authorization"), "Bearer sk-synthetic-openai");
    else assert.ok(request.headers.get("authorization")?.startsWith("Bearer ey"));
    for (const header of ["x-nanocodex-session-model-owner", "x-nanocodex-subject", "x-nanocodex-model-region"]) {
      assert.equal(request.headers.has(header), false);
    }
    const body = await request.text();
    if (expectedBody !== undefined) assert.equal(body, expectedBody, "401 replay changed the request body");
    dispatches++;
    record("provider_dispatch", { attempt: dispatches, bytes: body.length });
    if (rejectNext || rejectAlways) {
      rejectNext = false;
      return Response.json({ error: { code: "invalid_api_key", message: "synthetic-private-token" } }, { status: 401 });
    }
    return new Response('data: {"type":"response.output_text.delta","delta":"synthetic hello"}\n\n', {
      headers: { "content-type": "text/event-stream", authorization: "synthetic-private-token", "set-cookie": "private=fixture" },
    });
  };
  const runtime = { modules: true, compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"] };
  const options = convertV4MiniflareOptions({ workers: [
    { ...runtime, name: "gateway", script: `export default { fetch(request, env) {
      const path = new URL(request.url).pathname;
      if (path.startsWith('/users/')) return env.EGRESS.fetch(new Request('https://broker.internal' + path, request));
      if (path === '/generic') return env.EGRESS.fetch(new Request('https://nanocodex.internal/v1/responses', request));
      return env.SESSION.fetch(new Request('https://nanocodex.internal' + path, request));
    } };`, serviceBindings: { EGRESS: "egress", SESSION: { name: "egress", entrypoint: "SessionModelEgress" } } },
    { ...runtime, name: "egress", modules, bindings: { ENVIRONMENT: "test", CREDENTIAL_ENCRYPTION_KEY: "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY" },
      durableObjects: { USER_CREDENTIALS: { className: "UserCredentialBroker", useSQLite: true } }, outboundService: "provider" },
    { ...runtime, name: "provider", script: `export default { fetch(request, env) {
      if (request.headers.get('upgrade')?.toLowerCase() !== 'websocket') return env.HTTP.fetch(request);
      if (request.url !== 'https://api.openai.com/v1/responses'
        || request.headers.get('authorization') !== 'Bearer sk-synthetic-openai'
        || request.headers.has('x-nanocodex-session-model-owner')
        || request.headers.has('x-nanocodex-subject')) return new Response(null, {status:403});
      const [client, server] = Object.values(new WebSocketPair());
      server.accept();
      server.addEventListener('message', event => {
        const frame = JSON.parse(event.data);
        server.send(JSON.stringify({type:'response.output_text.delta',delta:'synthetic websocket hello'}));
        server.send(JSON.stringify({type:'response.completed',response:{id:'resp_synthetic',input:frame.input}}));
      });
      server.addEventListener('close', () => server.close(1000, 'fixture closed'));
      return new Response(null, {status:101,webSocket:client,headers:{authorization:'synthetic-private-token','set-cookie':'private=fixture'}});
    } };`, serviceBindings: { HTTP: provider } },
  ] });
  options.resourcePersistencePath = join(output, "state");
  let mf = new Miniflare(options);
  t.after(async () => {
    releaseRefresh();
    await mf.dispose();
    await writeFile(join(output, "trace.json"), JSON.stringify({
      command: "node --test js/egress/test/session-model-journey.test.mjs", refreshes, dispatches, trace,
    }, null, 2) + "\n");
    t.diagnostic(`Public transport evidence: ${output}/trace.json`);
  });
  let endpoint = await mf.ready;
  async function control(method, kind, value) {
    const response = await fetch(new URL(`/users/${owner}/credentials/${kind}`, endpoint), { method,
      ...(value === undefined ? {} : { headers: { "content-type": "application/json" }, body: JSON.stringify(value) }) });
    assert.equal(response.status, 204, await response.text());
  }
  const headers = { "content-type": "application/json", authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
    "x-nanocodex-session-model-owner": owner, "x-nanocodex-subject": subject, "x-nanocodex-model-region": "weur" };
  const expiresAt = (Math.ceil(Date.now() / 1000) + 3600) * 1000;
  await control("PUT", "chatgpt", { access_token: jwt({ exp: expiresAt / 1000,
    "https://api.openai.com/auth": { chatgpt_account_id: "synthetic-account", chatgpt_account_is_fedramp: false } }),
    refresh_token: "synthetic-refresh", account_id: "synthetic-account", expires_at: expiresAt, fedramp: false });
  // A concurrent connection's explicit 401 starts the owner's serialized refresh.
  // The uploading connection must read its input while its credential RPC waits.
  rejectNext = true;
  const refreshing = fetch(new URL("/v1/responses", endpoint), { method: "POST", headers, body: "{}" });
  await within(refreshEntered, 5000, "live credential refresh");
  // 16 MiB exceeds the public HTTP/service-binding buffering window. Upload EOF
  // must be observable while the live OAuth refresh is still deliberately held.
  let index = 0;
  const input = new ReadableStream({ pull(controller) {
    if (index === 0) controller.enqueue(new TextEncoder().encode('{"stream":true,"input":"'));
    else if (index <= 64) controller.enqueue(new Uint8Array(256 * 1024).fill(97));
    else { controller.enqueue(new TextEncoder().encode('"}')); controller.close(); record("upload_eof"); finishUpload(); }
    index++;
  } });
  const pending = fetch(new URL("/v1/responses", endpoint), { method: "POST", headers, body: input, duplex: "half" });
  await within(uploadFinished, 3000, "upload while refresh is pending");
  assert.equal(dispatches, 1, "upload reached provider before live credentials resolved");
  releaseRefresh();
  const recovered = await within(refreshing, 5000, "concurrent refresh recovery");
  assert.equal(recovered.status, 200); await recovered.text();
  let response = await within(pending, 5000, "model response");
  assert.equal(response.status, 200, await response.clone().text());
  assert.match(await response.text(), /synthetic hello/);
  assert.equal(response.headers.has("authorization"), false);
  assert.equal(response.headers.has("set-cookie"), false);
  record("overlap_completed");

  expectedBody = '{"stream":true,"input":[]}';
  const post = (path = "/v1/responses", override = {}) => fetch(new URL(path, endpoint), { method: "POST", headers: { ...headers, ...override }, body: expectedBody });
  // Deliberately never finish the upload. Early admission failures must still
  // produce a complete HTTP response while its body reader is awaiting input.
  async function unfinishedUpload(override = {}) {
    let request;
    const response = new Promise((resolve, reject) => {
      request = httpRequest(new URL("/v1/responses", endpoint), { method: "POST", headers: { ...headers, ...override } }, response => {
        let body = "";
        response.setEncoding("utf8");
        response.on("data", chunk => { body += chunk; });
        response.on("end", () => resolve({ status: response.statusCode, body }));
        response.on("error", reject);
      });
      request.on("error", reject);
      request.write('{"stream":true,"input":"unfinished');
    });
    try { return await within(response, 1500, "rejection before unfinished upload EOF"); }
    finally { request.destroy(); }
  }
  const oversized = await unfinishedUpload({ "content-length": String(32 * 1024 * 1024 + 1) });
  assert.equal(oversized.status, 413);
  record("oversized_upload_denied_before_eof");
  rejectNext = true;
  const beforeRecovery = { refreshes, dispatches };
  response = await post();
  assert.equal(response.status, 200);
  assert.match(await response.text(), /synthetic hello/);
  assert.equal(refreshes, beforeRecovery.refreshes + 1);
  assert.equal(dispatches, beforeRecovery.dispatches + 2);
  record("once401_recovered");
  rejectAlways = true;
  const beforeDenied = { refreshes, dispatches };
  response = await post();
  assert.equal(response.status, 401);
  assert.ok(!(await response.text()).includes("synthetic-private-token"));
  assert.equal(refreshes, beforeDenied.refreshes + 1);
  assert.equal(dispatches, beforeDenied.dispatches + 2, "401 must not retry indefinitely");
  rejectAlways = false;
  const beforeBoundary = dispatches;
  response = await post("/generic");
  assert.equal(response.status, 403); await response.text();
  response = await post("/v1/responses", { authorization: "Bearer caller-secret" });
  assert.equal(response.status, 403); await response.text();
  response = await post("/v1/responses", { "x-nanocodex-chatgpt-account-id": "missing-account" });
  assert.equal(response.status, 409); await response.text();
  assert.equal(dispatches, beforeBoundary);
  await control("DELETE", "chatgpt");
  response = await post();
  assert.equal(response.status, 409); await response.text();
  assert.equal(dispatches, beforeBoundary, "revoked credential still reached provider");
  const unfinishedDenied = await unfinishedUpload();
  assert.equal(unfinishedDenied.status, 409);
  assert.match(unfinishedDenied.body, /user_credential_unavailable/);
  assert.equal(dispatches, beforeBoundary);
  record("revocation_and_authority_denied");
  record("revoked_unfinished_upload_cancelled");

  // Exercise the no-refresh cold activation path through persisted encrypted
  // state and a fresh workerd process, without inspecting a DO's internals.
  await control("PUT", "openai", { api_key: "sk-synthetic-openai" });
  await mf.dispose();
  mf = new Miniflare(options);
  endpoint = await mf.ready;
  response = await post();
  assert.equal(response.status, 200, await response.clone().text());
  assert.match(await response.text(), /synthetic hello/);
  assert.equal(dispatches, beforeBoundary + 1);
  record("cold_openai_restored");
  const socketUrl = new URL("/v1/responses", endpoint); socketUrl.protocol = "ws:";
  const socket = new WebSocket(socketUrl, { headers: { ...headers, "openai-beta": "responses_websockets=2026-02-06" } });
  const frames = [];
  try {
    await within(new Promise((resolve, reject) => {
      socket.on("error", reject);
      socket.on("unexpected-response", (_request, response) => reject(new Error(`WebSocket upgrade rejected: ${response.statusCode}`)));
      socket.on("upgrade", response => {
        try {
          assert.equal(response.statusCode, 101);
          assert.match(response.headers["x-nanocodex-egress-request-id"], /^[0-9a-f-]{36}$/);
          assert.equal(response.headers.authorization, undefined);
          assert.equal(response.headers["set-cookie"], undefined);
          record("openai_websocket_upgrade", { status: response.statusCode });
        } catch (error) { reject(error); }
      });
      socket.on("open", () => socket.send(JSON.stringify({ type: "response.create", input: "synthetic user prompt" })));
      socket.on("message", data => {
        const frame = JSON.parse(String(data)); frames.push(frame);
        if (frame.type === "response.completed") resolve();
      });
    }), 3000, "OpenAI WebSocket first token and completion");
    assert.deepEqual(frames, [
      { type: "response.output_text.delta", delta: "synthetic websocket hello" },
      { type: "response.completed", response: { id: "resp_synthetic", input: "synthetic user prompt" } },
    ]);
    record("openai_websocket_frames", { frames });
    const closed = new Promise(resolve => socket.once("close", resolve));
    socket.close(1000);
    await within(closed, 1500, "OpenAI WebSocket close");
  } finally { socket.terminate(); }
  await control("DELETE", "openai");
  response = await post();
  assert.equal(response.status, 409); await response.text();
  assert.equal(dispatches, beforeBoundary + 1);
  record("cold_openai_revoked");
});
