import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { build } from "esbuild";
import { Miniflare, Log, LogLevel } from "miniflare";
import { fetch } from "./support/miniflare-fetch.mjs";

// Real HTTP -> managed SMS/account/session routes -> encrypted credential broker
// -> Accounts SDK root signing -> Connect Accounts Handler + SQLite grant state
// -> managed DurableAgentSession. Only Twilio is an external-service fixture.
// Prerequisites: workspace dependencies, verified nanocodex WASM outputs, and
// `node js/egress/scripts/prepare-whatsapp.mjs` (the ordinary broker build step).
const root = fileURLToPath(new URL("../../../", import.meta.url));
const output = new URL("../../../output/connect-full/", import.meta.url);
const common = { compatibilityDate: "2026-08-23", compatibilityFlags: ["nodejs_compat", "enable_request_signal"] };
const object = className => ({ className, useSQLite: true });
async function bundle(entry) {
  const wasm = [];
  const bundled = await build({
    stdin: { contents: entry, resolveDir: root }, bundle: true, write: false,
    format: "esm", target: "es2022", platform: "node",
    banner: { js: 'import { createRequire as __fixtureRequire } from "node:module"; const require = __fixtureRequire("/worker.mjs");' },
    external: ["cloudflare:*", "node:*"],
    alias: { "node-rsa": root + "js/nanocodex/tools/browser/unsupportedNodeRsa.mjs",
      "@whiskeysockets/baileys": root + "js/egress/src/whatsapp-generated/baileys.js" },
    plugins: [{ name: "workerd-wasm", setup(builder) {
      builder.onResolve({ filter: /\.wasm$|^nanocodex\/wasm$/ }, async args => {
        const bytes = await readFile(args.path === "nanocodex/wasm" ? root + "js/nanocodex/pkg-web/nanocodex_bg.wasm" : fileURLToPath(new URL(args.path, "file://" + args.resolveDir + "/")));
        const name = `./module-${wasm.length}.wasm`;
        wasm.push({ type: "CompiledWasm", path: name, contents: bytes });
        return { path: name, external: true };
      });
    } }],
  });
  return [{ type: "ESModule", path: "worker.mjs", contents: bundled.outputFiles[0].text }, ...wasm];
}

test("SMS sign-in, real broker signing, Connect grant and durable app threads", { timeout: 180_000 }, async t => {
  const trace = [];
  let passed = false, mf, base, cookie, connectWorker;
  let sends = 0, checks = 0;
  const sids = new Set();
  const phone = "+12025550173", code = "654321";
  const app = { appId: "broker-journey", origin: "https://broker-journey.example" };
  const managed = await bundle('export { default, DurableAgentSession } from "./js/managed/src/index.ts"; export { UserAccount, Organization, ApiKeyRecord, NonceStorage } from "./js/managed/src/account-auth.ts";');
  const broker = await bundle('export { default, UserCredentialBroker, UserConnectorBroker, AgentSubjectDirectory, McpConnectionDirectory, SpotifyRateLimit, WhatsAppAccount, GmailPushMailbox } from "./js/egress/src/egress.ts";');
  const connect = await bundle('export { default, ConnectNonceStorage } from "./js/connect-api/src/index.ts";');
  async function outbound(request) {
    const url = new URL(request.url);
    if (url.hostname === "nanocodex.localhost" && url.pathname.startsWith("/v1/connect/auth")) {
      const response = await connectWorker.fetch(request);
      trace.push({ transport: "broker outbound to real Connect Handler", path: url.pathname, observed: response.status });
      return response;
    }
    assert.equal(url.origin, "https://verify.twilio.com", `Unexpected external request ${url.origin}${url.pathname}; no live network permitted`);
    const form = new URLSearchParams(await request.text());
    if (url.pathname.endsWith("/Verifications")) {
      assert.equal(form.get("To"), phone);
      const sid = "VE" + (++sends).toString(16).padStart(32, "0");
      sids.add(sid);
      return Response.json({ sid, status: "pending" }, { status: 201 });
    }
    assert.ok(url.pathname.endsWith("/VerificationCheck"));
    checks++;
    const approved = sids.has(form.get("VerificationSid")) && form.get("Code") === code;
    if (approved) sids.delete(form.get("VerificationSid"));
    return Response.json({ status: approved ? "approved" : "pending" });
  }
  async function call(label, path, { body, method = body === undefined ? "GET" : "POST", expected = 200, origin, token, identity, session = true } = {}) {
    const response = await fetch(new URL(path, base), { method, signal: AbortSignal.timeout(15_000), headers: {
      origin: origin ?? (identity ? identity.origin : "https://nanocodex.localhost"),
      ...(session && cookie ? { cookie } : {}),
      ...(token ? { authorization: `Bearer ${token}` } : {}),
      ...(identity ? { "x-nanocodex-app-id": identity.appId } : {}),
      ...(body === undefined ? {} : { "content-type": "application/json" }),
    }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) }).catch(error => {
      trace.push({ label, path: new URL(path, base).pathname, method, expected, observed: error.name, deadline_ms: 15_000 });
      throw error;
    });
    const raw = await response.text();
    let value; try { value = JSON.parse(raw); } catch { value = raw; }
    trace.push({ label, path: new URL(path, base).pathname, method, expected, observed: response.status,
      ...(value?.error ? { error: value.error } : {}) });
    assert.equal(response.status, expected, `${label}: ${JSON.stringify(value)}`);
    return { value, headers: response.headers };
  }
  function resources(identity, count = 11) {
    const result = ["urn:nanocodex:agent:run", "urn:nanocodex:authorization:hosted",
      "urn:nanocodex:agent:output:final", "urn:nanocodex:agent:output:actions",
      "urn:nanocodex:agent:threads:app", "urn:nanocodex:agent:history:read",
      `urn:nanocodex:app:${identity.appId}`, `urn:nanocodex:origin:${encodeURIComponent(identity.origin)}`];
    while (result.length < count) result.push(`urn:nanocodex:journey:resource:${result.length}`);
    return result;
  }
  async function sign(identity = app, count = 11, expected = 200) {
    const { value } = await call(`real broker wallet_connect with ${count} resources`, "/v1/wallet/connect", { expected, body: {
      request: { method: "wallet_connect", params: [{ chainId: "0x1079", capabilities: {
        method: "login", auth: { url: "https://nanocodex.localhost/v1/connect/auth", resources: resources(identity, count), returnToken: true },
      } }] },
    } });
    if (expected !== 200) return value;
    assert.match(value.accounts[0].address, /^0x[0-9a-f]{40}$/i);
    const auth = value.accounts[0].capabilities.auth;
    assert.match(auth.approval_id, /^[A-Za-z0-9_-]{43}$/);
    assert.equal(typeof auth.token, "string");
    assert.doesNotMatch(JSON.stringify(value), /privateKey|private_key/);
    return { auth, address: value.accounts[0].address };
  }
  async function grant(identity = app, count = 11) {
    const signed = await sign(identity, count);
    assert.equal(signed.auth.profile.linked, true);
    const body = { app_id: identity.appId, account_address: signed.address, approval_id: signed.auth.approval_id,
      permission: "agent.run", authorization_mode: "hosted", requested_connectors: [] };
    const connection = (await call("exchange signed approval for durable grant", "/v1/connections", {
      identity, body, expected: 201,
    })).value;
    assert.ok(connection.grant.capabilities.includes("agent.threads.app"));
    await call("signed approval cannot be reused", "/v1/connections", { identity, body, expected: 403 });
    return connection;
  }
  const thread = (connection, suffix, options = {}) => call(options.label ?? suffix, `/v1/grants/${connection.grant.id}${suffix}`, {
    identity: app, token: connection.grant_token, ...options,
  });
  try {
    mf = new Miniflare({ log: new Log(LogLevel.ERROR), workers: [
      { name: "edge", ...common, modules: true, script: `export default { fetch(request, env) {
        const url = new URL(request.url); url.protocol = "https:"; url.hostname = "nanocodex.localhost"; url.port = "";
        request = new Request(url, request);
        const path = url.pathname;
        return (path.startsWith('/v1/grants/') || ['/v1/connections','/v1/account-link','/v1/hosted-authorizations'].includes(path) || path.startsWith('/v1/connect/auth') ? env.CONNECT : env.MANAGED).fetch(request);
      }};`, serviceBindings: { CONNECT: "connect", MANAGED: "managed" } },
      { name: "managed", ...common, modules: managed, outboundService: outbound,
        bindings: { ENVIRONMENT: "test", NANOCODEX_OTP_HMAC_KEY: "synthetic-connect-journey-otp-key",
          TWILIO_ACCOUNT_SID: "AC" + "1".repeat(32), TWILIO_AUTH_TOKEN: "synthetic-twilio-token", TWILIO_VERIFY_SERVICE_SID: "VA" + "1".repeat(32) },
        serviceBindings: { NANOCODEX: "broker" },
        r2Buckets: ["NANOCODEX_HISTORY", "NANOCODEX_BRAIN", "NANOCODEX_WORKSPACES"],
        durableObjects: { NANOCODEX_SESSIONS: object("DurableAgentSession"), NANOCODEX_AUTH: object("NonceStorage"),
          NANOCODEX_USERS: object("UserAccount"), NANOCODEX_ORGANIZATIONS: object("Organization"), NANOCODEX_API_KEYS: object("ApiKeyRecord") } },
      { name: "broker", ...common, modules: broker, outboundService: outbound,
        bindings: { ENVIRONMENT: "test", CREDENTIAL_ENCRYPTION_KEY: "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY" },
        durableObjects: { USER_CREDENTIALS: object("UserCredentialBroker"), USER_CONNECTORS: object("UserConnectorBroker"),
          AGENT_SUBJECTS: object("AgentSubjectDirectory"), MCP_CONNECTIONS: object("McpConnectionDirectory"),
          SPOTIFY_RATE_LIMITS: object("SpotifyRateLimit"), WHATSAPP_ACCOUNTS: object("WhatsAppAccount"), GMAIL_PUSH_MAILBOXES: object("GmailPushMailbox") } },
      { name: "connect", ...common, modules: connect, outboundService: outbound,
        durableObjects: { CONNECT_STATE: object("ConnectNonceStorage") }, serviceBindings: { ACCOUNTS: "managed", EGRESS: "broker" } },
    ] });
    base = await mf.ready;
    connectWorker = await mf.getWorker("connect");
    await call("unsigned wallet bridge denied", "/v1/wallet/connect", { body: {}, expected: 401 });
    for (const path of ["/v1/wallet/link", "/v1/wallet/link/poll", "/v1/wallet/link/cancel", "/v1/wallet/unlink"]) {
      await call("anonymous wallet linking denied", path, { body: { operation_id: "cfd6e856-f945-4bc6-9cc6-2514a9a931e0" }, expected: 401 });
    }
    const challenge = (await call("SMS challenge issued", "/v1/auth/sms/start", { body: { phone }, expected: 202 })).value;
    const login = await call("SMS verification provisions real broker wallet", "/v1/auth/sms/verify", {
      body: { phone, code, challenge_id: challenge.challenge_id },
    });
    assert.equal(login.value.user.persistent, true);
    cookie = login.headers.get("set-cookie").split(";")[0];
    const me = (await call("persistent browser session and broker wallet", "/v1/me")).value;
    assert.equal(me.user.id, login.value.user.id);
    assert.equal(me.user.address, login.value.user.address);
    assert.equal(me.authentication, "account_session");
    await call("retain synthetic model credential through real account API", "/v1/credentials/openai", {
      method: "PUT", body: { api_key: "sk-synthetic-connect-journey-provider" }, expected: 204,
    });
    const first = await sign();
    assert.equal(first.address.toLowerCase(), me.user.address.toLowerCase());
    const link = (await call("start account link under signed Handler token", "/v1/account-link", { body: {}, token: first.auth.token })).value;
    const authorize = new URL(link.authorization_url);
    authorize.pathname += "/authorize";
    const authorization = (await call("authorize link under real SMS session", authorize.pathname + authorize.search, { body: {} })).value;
    await call("exchange account link", "/v1/account-link", { method: "PUT", token: first.auth.token,
      body: { state: link.state, code: authorization.code } });
    const connection = await grant();
    assert.deepEqual((await thread(connection, "/threads")).value.threads, []);
    const created = (await thread(connection, "/threads", { label: "create real managed session", body: { title: "Synthetic first thread", operation_id: crypto.randomUUID() }, expected: 201 })).value;
    const id = created.thread.id, agent = created.connection.agent_id;
    assert.equal(created.connection.grant.id, connection.grant.id);
    const state = (await thread(connection, `/agents/${agent}`, { label: "read actual managed session" })).value;
    assert.equal(state.agent_id, agent);
    const second = (await thread(connection, "/threads", { label: "create second real managed session", body: { operation_id: crypto.randomUUID() }, expected: 201 })).value;
    assert.notEqual(second.connection.agent_id, agent);
    assert.equal((await thread(connection, `/threads/${id}`, { label: "reopen first thread" })).value.connection.agent_id, agent);
    const renewed = await grant(app, 64);
    assert.equal((await thread(renewed, "/threads", { label: "renewed grant retains both threads" })).value.threads.length, 2);
    assert.equal((await thread(renewed, `/agents/${agent}`, { label: "renewed grant retains managed session" })).value.agent_id, agent);
    const denied = await sign(app, 65, 400);
    assert.equal(denied.error, "invalid_wallet_connect_request");
    const otherApp = { appId: "other-broker-journey", origin: "https://other-broker-journey.example" };
    const other = await grant(otherApp);
    assert.deepEqual((await thread(other, "/threads", { identity: otherApp, label: "other app has empty thread list" })).value.threads, []);
    await thread(other, `/threads/${id}`, { identity: otherApp, expected: 404, label: "other app cannot open first app thread" });
    await thread(other, `/agents/${agent}`, { identity: otherApp, expected: 404, label: "other app cannot read first app session" });
    await thread(connection, "/threads", { token: "z".repeat(43), expected: 401, label: "invalid grant token denied" });
    await call("foreign origin cannot invoke signing", "/v1/wallet/connect", { body: {}, origin: "https://foreign.example", expected: 403 });
    for (const path of ["/v1/wallet/link", "/v1/wallet/link/poll", "/v1/wallet/link/cancel", "/v1/wallet/unlink"]) {
      await call("foreign origin cannot change payment wallet", path, { body: { operation_id: "cfd6e856-f945-4bc6-9cc6-2514a9a931e0" }, origin: "https://foreign.example", expected: 403 });
      await call("wallet link rejects browser secret import", path, { body: { privateKey: "synthetic-rejected-secret" }, expected: 400 });
    }
    assert.equal(sends, 1); assert.equal(checks, 1);
    passed = true;
    t.diagnostic(`${trace.length} HTTP/service observations; real broker signing, Handler verification, account linking, grants and durable session isolation passed`);
  } finally {
    await mkdir(output, { recursive: true });
    await writeFile(new URL("http-trace.json", output), JSON.stringify({ command: "node --test js/managed/test/connect-signin-broker-journey.test.mjs", passed,
      inputs: "Synthetic phone, OTP and credential encryption key. Session cookies, signatures, IDs and bearer tokens omitted.",
      expected: "One SMS creates persistent account and encrypted broker wallet; real wallet_connect accepts 11/64 resources and denies 65; actual Handler verifies signatures and issues single-use approvals; real grant/session storage supports create, reopen, renewal and cross-app denial.",
      limits: "No browser UI, live SMS, chain RPC, model inference, payment or production mutation is exercised.", trace }, null, 2));
    await mf?.dispose();
  }
});
