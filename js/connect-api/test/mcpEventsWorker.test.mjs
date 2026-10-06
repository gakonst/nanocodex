import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash, createHmac, randomBytes, randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { createTransportFixture, runInNetworkNamespace } from "./mcpEventsTransportFixture.mjs";

const require = createRequire(import.meta.url);
const accountOrigin = "https://nanocodex.gakonst.workers.dev";
const accountAddress = `0x${"1".repeat(40)}`;
const brokerUser = "11111111-1111-4111-8111-111111111111";
const callback = "https://mcp-events-client.example/oauth/callback";
const opaque = () => randomBytes(32).toString("base64url");

// This journey uses the public OAuth and MCP HTTP boundaries of the shipped
// Worker and its actual SQLite Durable Object alarms. Only the remote account,
// DNS and webhook receivers are fixtures. Never invoke storage or alarm APIs.
test("MCP Events public OAuth, durable turn observation and signed callbacks", { timeout: 180_000 }, async t => {
  if (await runInNetworkNamespace(t, fileURLToPath(import.meta.url))) return;
  t.diagnostic("Reproduce: node --test js/connect-api/test/mcpEventsWorker.test.mjs (Node 24+, installed workspace dependencies). Synthetic identities and prompts only.");
  const outdir = await mkdtemp(path.join(os.tmpdir(), "nanocodex-mcp-events-"));
  t.after(() => rm(outdir, { recursive: true, force: true }));
  await promisify(execFile)(process.execPath, [path.join(path.dirname(require.resolve("wrangler/package.json")), "bin/wrangler.js"),
    "deploy", "--dry-run", "--env=", "--config", "wrangler.jsonc", "--outdir", outdir],
  { cwd: new URL("..", import.meta.url) });
  const approvals = new Map(), agents = new Map(), upstream = [], deliveries = [], transcript = [];
  const receivers = new Map();
  const account = async request => {
    const url = new URL(request.url);
    const encoded = await request.text(), body = encoded ? JSON.parse(encoded) : undefined;
    if (url.pathname === "/connect/hosted-authorizations/exchange") {
      const signed = approvals.get(body.code);
      assert.deepEqual(body, signed, "hosted approval is exchanged with exact signed resources");
      approvals.delete(body.code);
      return Response.json({ linked: true, user_id: brokerUser, account_address: body.account_address, resources: body.resources });
    }
    const row = { path: url.pathname, method: request.method, body,
      grant: request.headers.get("x-nanocodex-connect-grant-id"), user: request.headers.get("x-nanocodex-connect-user"),
      authorization: request.headers.get("authorization") };
    upstream.push(row);
    if (url.pathname === "/v1/agents" && request.method === "POST") {
      const agent_id = randomUUID(); agents.set(agent_id, new Map()); return Response.json({ agent_id });
    }
    const [, id, suffix] = url.pathname.match(/^\/v1\/agents\/([^/]+)(.*)$/) ?? [];
    assert.ok(id && agents.has(id), `Unexpected account service path ${url.pathname}`);
    if (suffix === "/_connect-existence") return new Response(null, { status: 204 });
    if (suffix === "/turns" && request.method === "POST") {
      assert.ok(body.id, "turn start carries the user's stable operation ID");
      agents.get(id).set(body.id, { turn_id: body.id, state: "running", input: body.input });
      return Response.json({ turn_id: body.id, status: "accepted" }, { status: 202 });
    }
    if (suffix.startsWith("/turns/") && request.method === "GET") {
      const turn = agents.get(id).get(decodeURIComponent(suffix.slice(7)));
      row.responseState = turn?.state; row.responseUpdatedAt = turn?.updated_at;
      return turn ? Response.json(turn) : Response.json({ error: "turn_not_found" }, { status: 404 });
    }
    if (!suffix) return Response.json({ agent_id: id, active_turns: [...agents.get(id).values()] });
    assert.fail(`Unexpected managed resource ${suffix}`);
  };
  const egress = async request => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/credentials")) return Response.json({ chatgpt: { connected: true } });
    if (url.pathname.endsWith("/connectors")) return Response.json({ connectors: {} });
    if (url.pathname.startsWith("/subjects/")) return new Response(null, { status: 204 });
    assert.fail(`Unexpected egress ${request.method} ${url.href}`);
  };
  const webhook = async request => {
    const url = new URL(request.url), raw = await request.text(), body = JSON.parse(raw);
    const receiver = receivers.get(url.pathname) ?? {};
    const delivery = { path: url.pathname, raw, body, id: request.headers.get("webhook-id"),
      timestamp: request.headers.get("webhook-timestamp"), signature: request.headers.get("webhook-signature"),
      subscription: request.headers.get("x-mcp-subscription-id"), authorization: request.headers.get("authorization") };
    deliveries.push(delivery);
    if (body.type === "verification") {
      if (receiver.verificationStatus) return new Response(null, { status: receiver.verificationStatus, headers: { location: receiver.location ?? callbackUrl("redirect-target") } });
      if (receiver.stalledBody) return new Response(new ReadableStream({
        start(controller) { controller.enqueue(new TextEncoder().encode('{"challenge":"')); },
      }));
      if (receiver.verificationBody) return new Response(receiver.verificationBody);
      return Response.json({ challenge: receiver.challenge ?? body.challenge });
    }
    const status = receiver.statuses?.shift() ?? 204;
    return new Response([410, 413].includes(status) ? "x".repeat(20000) : null, { status });
  };
  const transport = await createTransportFixture({ onRequest: webhook });
  t.after(() => transport.close());
  const { Miniflare, Log, LogLevel } = require("miniflare");
  const options = {
    ...transport.miniflareOptions,
    modules: [{ type: "ESModule", path: path.join(outdir, "index.js") }], modulesRoot: outdir,
    compatibilityDate: "2026-08-23", compatibilityFlags: ["nodejs_compat", "global_fetch_strictly_public"],
    durableObjects: { CONNECT_STATE: { className: "ConnectNonceStorage", useSQLite: true }, MCP_EVENTS: { className: "McpEvents", useSQLite: true } },
    durableObjectsPersist: path.join(outdir, "state"), log: new Log(LogLevel.ERROR),
    serviceBindings: { ACCOUNTS: account, EGRESS: egress },
  };
  let mf = new Miniflare(options);
  t.after(() => mf.dispose());
  let origin = (await mf.ready).origin;
  const resource = `${origin}/mcp`;
  async function call(route, { method = "GET", json, form, token, headers = {} } = {}) {
    const response = await fetch(`${origin}${route}`, { method, redirect: "manual", headers: {
      ...(json === undefined ? {} : { "content-type": "application/json" }),
      ...(form === undefined ? {} : { "content-type": "application/x-www-form-urlencoded" }),
      ...(token ? { authorization: `Bearer ${token}` } : {}), ...headers,
    }, ...(json === undefined ? {} : { body: JSON.stringify(json) }), ...(form === undefined ? {} : { body: new URLSearchParams(form) }) });
    transcript.push(`${method} ${route.split("?")[0]} -> ${response.status}`);
    return response;
  }
  async function json(response, status = 200) {
    const body = await response.json(); assert.equal(response.status, status, JSON.stringify(body)); return body;
  }
  let rpcId = 0;
  async function rpc(token, method, params = {}, overrides = {}, status = 200) {
    const reply = await json(await call("/mcp", { method: "POST", token, json: { jsonrpc: "2.0", id: ++rpcId, method, params: { ...params, _meta: { "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, ...params._meta } } },
      headers: { accept: "application/json, text/event-stream", "mcp-protocol-version": "2026-07-28", "mcp-method": method, ...(method === "tools/call" ? { "mcp-name": params.name } : {}), ...overrides } }), status);
    transcript.push(`${method} -> ${reply.error ? `RPC ${reply.error.code}` : "result"}`);
    return reply;
  }
  async function result(token, method, params = {}) {
    const reply = await rpc(token, method, params); assert.equal(reply.error, undefined, JSON.stringify(reply)); return reply.result;
  }
  async function tool(token, name, args = {}) {
    const reply = await result(token, "tools/call", { name, arguments: args });
    assert.equal(reply.isError, undefined, JSON.stringify(reply)); return reply.structuredContent ?? JSON.parse(reply.content[0].text);
  }
  const client = await json(await call("/oauth/register", { method: "POST", json: {
    client_name: "Synthetic MCP Events client", redirect_uris: [callback], token_endpoint_auth_method: "none",
  } }), 201);
  async function authorize(scope = "agent:run") {
    const verifier = opaque();
    const params = { response_type: "code", client_id: client.client_id, redirect_uri: callback, resource, scope,
      state: opaque(), code_challenge: createHash("sha256").update(verifier).digest("base64url"), code_challenge_method: "S256" };
    const pending = await call(`/oauth/authorize?${new URLSearchParams(params)}`);
    assert.equal(pending.status, 302);
    const requestId = new URL(pending.headers.get("location")).searchParams.get("oauth_request");
    const route = `/oauth/requests/${requestId}`;
    const detail = await json(await call(route, { headers: { origin: accountOrigin } }));
    const code = opaque();
    const approved = { account_address: accountAddress, app_id: detail.app_id, app_origin: detail.app_origin, code, resources: detail.resources };
    approvals.set(code, approved);
    const consent = await json(await call(`${route}/approve`, { method: "POST", headers: { origin: accountOrigin },
      json: { account_address: accountAddress, code, resources: detail.resources } }));
    const authorizationCode = new URL(consent.redirect_uri).searchParams.get("code");
    return json(await call("/oauth/token", { method: "POST", form: { grant_type: "authorization_code", code: authorizationCode,
      client_id: client.client_id, redirect_uri: callback, resource, code_verifier: verifier } }));
  }
  const refresh = async refresh_token => json(await call("/oauth/token", { method: "POST", form: { grant_type: "refresh_token", client_id: client.client_id,
    refresh_token, resource } }));
  async function eventually(label, predicate, timeout = 20_000) {
    const until = Date.now() + timeout;
    while (Date.now() < until) { const value = await predicate(); if (value) { t.diagnostic(label); return value; } await new Promise(resolve => setTimeout(resolve, 50)); }
    assert.fail(`${label}: timed out. Deliveries=${JSON.stringify(deliveries)}; managed=${JSON.stringify(upstream)}`);
  }
  function finish(turn_id, status = "completed", updatedAt = Date.now()) {
    for (const turns of agents.values()) if (turns.has(turn_id)) Object.assign(turns.get(turn_id), { state: status, updated_at: updatedAt });
  }
  t.after(() => t.diagnostic(`Public HTTP/RPC transcript (credentials omitted):\n${transcript.join("\n")}\nManaged polling evidence: ${JSON.stringify(upstream)}\nCallback evidence: ${JSON.stringify(deliveries)}\nPublic-only network/TCP/TLS evidence: ${JSON.stringify(transport.trace)}`));
  let tokens = await authorize();
  const discovery = await result(tokens.access_token, "server/discover");
  assert.equal(discovery.resultType, "complete");
  assert.ok(discovery.supportedVersions.includes("2026-07-28"));
  assert.equal(discovery._meta["io.modelcontextprotocol/serverInfo"].name, "nanocodex");
  assert.ok(discovery.capabilities.events);
  assert.equal(discovery.ttlMs, 0);
  assert.equal(discovery.cacheScope, "private");
  assert.deepEqual(await result(tokens.access_token, "server/discover"), discovery, "discovery is deterministic for the same grant");
  const toolsCatalog = await result(tokens.access_token, "tools/list");
  assert.equal(toolsCatalog.ttlMs, 0);
  assert.equal(toolsCatalog.cacheScope, "private");
  assert.deepEqual(await result(tokens.access_token, "tools/list"), toolsCatalog, "tool catalog is deterministic for the same grant");
  const catalog = await result(tokens.access_token, "events/list");
  assert.deepEqual(catalog.events.map(event => event.name), ["agent.turn.completed"]);
  assert.equal((await rpc(tokens.access_token, "server/discover", {}, { "mcp-method": "tools/list" }, 400)).error.code, -32020);
  assert.equal((await rpc(tokens.access_token, "server/discover", { _meta: { "io.modelcontextprotocol/protocolVersion": "2099-01-01" } }, { "mcp-protocol-version": "2099-01-01" }, 400)).error.code, -32022);
  assert.equal((await rpc(tokens.access_token, "unknown/method", {}, {}, 404)).error.code, -32601);
  const legacy = await json(await call("/mcp", { method: "POST", token: tokens.access_token, headers: {
    accept: "application/json, text/event-stream", "mcp-protocol-version": "2025-06-18",
  }, json: { jsonrpc: "2.0", id: "legacy", method: "initialize", params: {
    protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "Synthetic legacy client", version: "1" },
  } } }));
  assert.equal(legacy.result.protocolVersion, "2025-06-18");
  assert.equal(legacy.result.resultType, undefined);
  const dataOnly = await authorize("data:read");
  assert.deepEqual((await result(dataOnly.access_token, "events/list")).events, []);
  assert.equal((await result(dataOnly.access_token, "server/discover")).capabilities.events, undefined);
  const secret = `whsec_${randomBytes(32).toString("base64")}`;
  const rotatedSecret = `whsec_${randomBytes(32).toString("base64")}`;
  const eventName = "agent.turn.completed";
  const callbackUrl = name => `https://callbacks.mcp-events.example/${name}`;
  function subscription(name, extra = {}) { return { name: eventName, delivery: { mode: "webhook", url: callbackUrl(name), secret }, ...extra }; }
  function unsubscribe(name, args) { return result(tokens.access_token, "events/unsubscribe", { name: eventName, arguments: args ?? {}, delivery: { mode: "webhook", url: callbackUrl(name) } }); }
  const eventsAt = name => deliveries.filter(delivery => delivery.path === `/${name}` && delivery.body.type !== "verification");
  const verifyAt = name => deliveries.filter(delivery => delivery.path === `/${name}` && delivery.body.type === "verification");
  const subscribe = (name, extra) => result(tokens.access_token, "events/subscribe", subscription(name, extra));
  const start = async (turn_id = randomUUID()) => {
    await tool(tokens.access_token, "nanocodex_agent_start", { operation_id: turn_id, prompt: "Synthetic MCP Events completion canary" });
    return turn_id;
  };
  function checkSignature(delivery, key = secret) {
    const signed = `${delivery.id}.${delivery.timestamp}.${delivery.raw}`;
    const expected = `v1,${createHmac("sha256", Buffer.from(key.slice(6), "base64")).update(signed).digest("base64")}`;
    assert.ok(delivery.signature.split(" ").includes(expected), "receiver independently verifies Standard Webhooks signature over the exact raw body");
    assert.ok(Math.abs(Date.now() / 1000 - Number(delivery.timestamp)) < 30);
    assert.equal(delivery.authorization, null, "callback never receives OAuth or Connect credentials");
  }
  for (const url of ["http://callbacks.example/events", "https://localhost/events", "https://127.0.0.1/events", "https://[::1]/events", "https://user:pass@callbacks.example/events", "https://callbacks.example/events#fragment"]) {
    const before = deliveries.length;
    assert.equal((await rpc(tokens.access_token, "events/subscribe", { ...subscription("unsafe"), delivery: { mode: "webhook", url, secret } })).error.code, -32602);
    assert.equal(deliveries.length, before);
  }
  assert.equal((await rpc(tokens.access_token, "events/subscribe", subscription("missing", { name: "agent.unknown" }))).error.code, -32011);
  assert.equal((await rpc(tokens.access_token, "events/subscribe", subscription("bad-filter", { arguments: { agent_id: "unexpected" } }))).error.code, -32602);
  assert.equal((await rpc(dataOnly.access_token, "events/subscribe", subscription("unapproved"))).error.code, -32012);
  assert.equal((await rpc(tokens.access_token, "events/subscribe", subscription("mode", { delivery: { mode: "stream", url: callbackUrl("mode"), secret } }))).error.code, -32014);
  assert.equal((await rpc(tokens.access_token, "events/subscribe", subscription("secret", { delivery: { mode: "webhook", url: callbackUrl("secret"), secret: "whsec_short" } }))).error.code, -32602);
  for (const hostname of ["private.mcp-events.example", "rfc1918.mcp-events.example", "metadata.mcp-events.example", "mismatch.mcp-events.example"]) {
    const before = deliveries.length;
    const connections = transport.trace.filter(row => row.kind === "connect").length;
    assert.equal((await rpc(tokens.access_token, "events/subscribe", { ...subscription("transport-denied"),
      delivery: { mode: "webhook", url: `https://${hostname}/transport-denied`, secret } })).error.code, -32015);
    assert.equal(deliveries.length, before, "private DNS and TLS hostname mismatch fail before callback HTTP");
    assert.equal(transport.trace.filter(row => row.kind === "connect").length - connections, hostname.startsWith("mismatch.") ? 1 : 0,
      "private DNS is rejected before connect; wrong certificate hostname is rejected after real TCP connect");
  }
  const foreign = await authorize();
  await result(foreign.access_token, "events/subscribe", subscription("foreign"));
  receivers.set("/bad-challenge", { challenge: "wrong" });
  assert.equal((await rpc(tokens.access_token, "events/subscribe", subscription("bad-challenge"))).error.code, -32015);
  receivers.set("/redirect", { verificationStatus: 302 });
  assert.equal((await rpc(tokens.access_token, "events/subscribe", subscription("redirect"))).error.code, -32015);
  assert.equal(verifyAt("redirect-target").length, 0, "verification never follows redirects");
  receivers.set("/private-redirect", { verificationStatus: 307, location: "https://private.mcp-events.example/redirect-target" });
  const beforePrivateRedirect = transport.trace.filter(row => row.kind === "connect" && !row.target.startsWith("93.184.216.34:")).length;
  assert.equal((await rpc(tokens.access_token, "events/subscribe", subscription("private-redirect", { delivery: { mode: "webhook", url: "https://transport-checks.mcp-events.example/private-redirect", secret } }))).error.code, -32015);
  assert.equal(transport.trace.filter(row => row.kind === "connect" && !row.target.startsWith("93.184.216.34:")).length, beforePrivateRedirect,
    "a redirect to private DNS never connects to the target");
  receivers.set("/stalled", { stalledBody: true });
  const stalledAt = Date.now();
  assert.equal((await rpc(tokens.access_token, "events/subscribe", subscription("stalled", { delivery: { mode: "webhook", url: "https://transport-checks.mcp-events.example/stalled", secret } }))).error.code, -32015);
  assert.ok(Date.now() - stalledAt >= 9_000 && Date.now() - stalledAt < 15_000, "deadline includes verification response streaming");
  t.diagnostic("Stalled verification response body rejected within the 10-second transport deadline");
  receivers.set("/oversized", { verificationBody: "x".repeat(5000) });
  assert.equal((await rpc(tokens.access_token, "events/subscribe", subscription("oversized"))).error.code, -32015);
  const main = await subscribe("main");
  assert.match(main.id, /^sub_/); assert.equal(main.cursor, null); assert.equal(main.truncated, false);
  assert.ok(Date.parse(main.refreshBefore) > Date.now());
  assert.equal(verifyAt("main").length, 1); checkSignature(verifyAt("main")[0]);
  assert.equal((await subscribe("main")).id, main.id, "same identity refreshes existing subscription");
  assert.equal(verifyAt("main").length, 1, "successful verification is cached within its TTL");
  const running = randomUUID(), otherTurn = randomUUID();
  const filtered = await subscribe("filtered", { arguments: { turn_id: running } });
  const excluded = await subscribe("filtered", { arguments: { turn_id: otherTurn } });
  assert.notEqual(filtered.id, excluded.id, "filter arguments form part of subscription identity");
  await start(running);
  await eventually("Durable alarm polls the actual MCP-started turn", () => upstream.some(row => row.path.endsWith(`/turns/${running}`)));
  assert.equal(eventsAt("main").length, 0, "nonterminal status does not emit completion");
  // Restart the actual process with the same persisted SQLite directory and
  // issuer port while the turn is retained. No private DO inspection or alarm
  // triggering is used to resume observation.
  const port = new URL(origin).port;
  await mf.dispose();
  mf = new Miniflare({ ...options, port: Number(port) });
  origin = (await mf.ready).origin;
  assert.equal(`${origin}/mcp`, resource, "restart preserves OAuth resource audience");
  await result(tokens.access_token, "events/list");
  const invalidTimestamp = 8_640_000_000_000_001; // Finite milliseconds outside the valid Date range.
  finish(running, "completed", invalidTimestamp);
  await eventually("Invalid source completion timestamp is retried by real alarms", () => upstream.filter(row =>
    row.path.endsWith(`/turns/${running}`) && row.responseUpdatedAt === invalidTimestamp).length >= 2);
  assert.equal(eventsAt("main").length, 0, "invalid occurrence time is never replaced with observation time");
  assert.equal(eventsAt("filtered").length, 0);
  const recoveredTimestamp = Date.now();
  finish(running, "completed", recoveredTimestamp);
  const completed = await eventually("Terminal status produces signed callback without client polling", () => eventsAt("main").find(event => event.body.data.turn_id === running));
  checkSignature(completed); assert.equal(completed.body.name, eventName);
  assert.equal(completed.body.timestamp, new Date(recoveredTimestamp).toISOString());
  assert.equal(completed.body.data.completed_at, completed.body.timestamp, "recovery preserves the source occurrence time");
  assert.equal(completed.body.data.status, "completed"); assert.equal(completed.body.eventId, completed.id);
  assert.equal(completed.body.data.output_preview, undefined, "event contains status metadata; full output remains behind the authorized status tool");
  const filteredEvent = await eventually("Exact turn filter survives restart and excludes other turn subscriptions", () => eventsAt("filtered")[0]);
  assert.equal(filteredEvent.body.data.turn_id, running);
  assert.equal(filteredEvent.subscription, filtered.id);
  checkSignature(filteredEvent);
  const oldAccess = tokens.access_token;
  tokens = await refresh(tokens.refresh_token);
  assert.notEqual(tokens.access_token, oldAccess);
  const rotated = await subscribe("main", { delivery: { mode: "webhook", url: callbackUrl("main"), secret: rotatedSecret } });
  assert.equal(rotated.id, main.id, "access-token refresh and signing-key rotation retain principal subscription identity");
  const rotatedTurn = await start(); finish(rotatedTurn, "failed");
  const rotatedEvent = await eventually("Rotation overlap signs with both secrets", () => eventsAt("main").find(event => event.body.data.turn_id === rotatedTurn));
  checkSignature(rotatedEvent, secret); checkSignature(rotatedEvent, rotatedSecret);
  assert.equal(rotatedEvent.body.data.status, "failed");
  await unsubscribe("main"); await unsubscribe("main");
  assert.equal(eventsAt("filtered").length, 1, "completed and failed turns only reach their matching filters");
  await unsubscribe("filtered", { turn_id: running });
  await unsubscribe("filtered", { turn_id: otherTurn });

  receivers.set("/retry", { statuses: [503, 204] });
  receivers.set("/gone", { statuses: [410, 204] });
  receivers.set("/large", { statuses: [413, 204] });
  await subscribe("retry"); await subscribe("gone"); await subscribe("large");
  const failingDelivery = await start(); finish(failingDelivery);
  await eventually("503 retries using durable outbox", () => eventsAt("retry").length === 2);
  assert.equal(eventsAt("retry")[0].id, eventsAt("retry")[1].id);
  assert.equal(eventsAt("retry")[0].raw, eventsAt("retry")[1].raw, "retry keeps event bytes and identity stable");
  assert.equal(eventsAt("gone").length, 1); assert.equal(eventsAt("large").length, 1);
  const nextTurn = await start(); finish(nextTurn, "cancelled");
  await eventually("413 drops only its event and retains subscription", () => eventsAt("large").length === 2);
  await eventually("410 drops only its event and retains subscription", () => eventsAt("gone").length === 2);
  assert.equal(eventsAt("main").length, 2, "idempotent unsubscribe stops subsequent delivery");
  assert.equal(eventsAt("foreign").length, 0, "a separate OAuth grant cannot observe these turns");
  assert.equal(eventsAt("large")[1].body.data.status, "cancelled");
  await unsubscribe("retry"); await unsubscribe("large"); await unsubscribe("gone");
  await subscribe("expired", { ttlMs: 10 });
  const expiredTurn = await start(); finish(expiredTurn);
  await subscribe("revoked");
  const revokedTurn = await start();
  await json(await call("/oauth/revoke", { method: "POST", form: { client_id: client.client_id, token: tokens.refresh_token } }));
  finish(revokedTurn);
  await new Promise(resolve => setTimeout(resolve, 6500));
  assert.equal(eventsAt("expired").length, 0, "expired subscription never receives later completion");
  assert.equal(eventsAt("revoked").length, 0, "OAuth family revocation fences background delivery");
  assert.ok(upstream.filter(row => row.path.includes("/turns/")).every(row => row.user === brokerUser && row.authorization === null && /^0x[0-9a-f]{64}$/.test(row.grant)), "background polling reconstructs scoped managed assertions without bearer pass-through");
  t.diagnostic("Observed actual alarm polling, invalid source timestamp recovery, exact-turn filtering, wrong TLS hostname rejection, signature verification, token/key rotation, 503 retry, 410/413 event drop, expiry and revocation over public OAuth/MCP boundaries.");
});
