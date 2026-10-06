import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { randomUUID } from "node:crypto";
import http from "node:http";
import test from "node:test";

const require = createRequire(import.meta.url);
const wranglerRequire = createRequire(require.resolve("wrangler/package.json"));
const { Miniflare, Log, LogLevel, convertV4MiniflareOptions } = wranglerRequire("miniflare");
const app = { appId: "thread-fixture", origin: "https://threads.example" };
const account = `0x${"1".repeat(40)}`;
const brokerUser = "11111111-1111-4111-8111-111111111111";
const scopeResource = "urn:nanocodex:agent:threads:app";
const historyResource = "urn:nanocodex:agent:history:read";

// The shipped HTTP Worker and real workerd Durable Objects own authentication,
// grant exchange, persistence, membership and mutations. Only external account
// identity/provider services are synthetic; no storage/auth methods are mocked.
test("app threads HTTP journey: consent, isolation, reconnect, history and deletion", { timeout: 120_000 }, async t => {
  const outdir = await mkdtemp(path.join(os.tmpdir(), "nanocodex-app-threads-"));
  t.after(() => rm(outdir, { recursive: true, force: true }));
  await promisify(execFile)(process.execPath, [path.join(path.dirname(require.resolve("wrangler/package.json")), "bin/wrangler.js"),
    "deploy", "--dry-run", "--env=", "--config", "wrangler.jsonc", "--outdir", outdir],
  { cwd: new URL("..", import.meta.url) });
  const upstream = [];
  const agents = new Map();
  let loseNextCreateReply = false;
  const externalStreams = new Set();
  let rejectDelete = false;
  let inactiveHost = false;
  const mf = new Miniflare(convertV4MiniflareOptions({
    modules: true, modulesRoot: outdir, scriptPath: path.join(outdir, "index.js"),
    compatibilityDate: "2026-08-23", compatibilityFlags: ["nodejs_compat"],
    durableObjects: { CONNECT_STATE: { className: "ConnectNonceStorage", useSQLite: true } },
    log: new Log(LogLevel.ERROR),
    serviceBindings: {
      ACCOUNTS: async request => {
        const url = new URL(request.url);
        if (url.pathname === "/connect/hosted-authorizations/exchange") {
          const body = await request.json();
          if (body.code !== "c".repeat(43)) return new Response(null, { status: 403 });
          return Response.json({ linked: true, user_id: brokerUser, account_address: body.account_address, resources: body.resources });
        }
        if (url.pathname === "/connect/host-principals/exchange") {
          const body = await request.json();
          const principal = { kind: "host", id: body.exchange, issuer: "https://identity.example", tenant: "test",
            app_id: body.app_id, app_origin: body.app_origin, session_epoch: 1, session_digest: "s".repeat(43) };
          return Response.json({ principal, user_id: brokerUser, resources: body.resources, expires_at: Math.floor(Date.now() / 1000) + 3600 });
        }
        if (url.pathname === "/connect/host-principals/validate") {
          return Response.json({ active: !inactiveHost, user_id: brokerUser }, { status: inactiveHost ? 403 : 200 });
        }
        upstream.push({ path: url.pathname, method: request.method,
          grant: request.headers.get("x-nanocodex-connect-grant-id"),
          user: request.headers.get("x-nanocodex-connect-user"),
          authorization: request.headers.get("authorization") });
        if (url.pathname === "/v1/agents" && request.method === "POST") {
          const key = request.headers.get("idempotency-key");
          if (key) assert.match(key, /^0x[0-9a-f]{64}$/);
          // Intentionally allocate on every dispatch: Connect must not replay
          // an uncertain creation, even if managed has its own deduplication.
          const agent_id = randomUUID().replace(/^(.{14})4/, "$17");
          agents.set(agent_id, []);
          if (loseNextCreateReply) { loseNextCreateReply = false; return new Response(null, { status: 503 }); }
          return Response.json({ agent_id });
        }
        const [, id, suffix] = url.pathname.match(/^\/v1\/agents\/([^/]+)(.*)$/) ?? [];
        assert.ok(id, `Unexpected account service path ${url.pathname}`);
        if (!agents.has(id)) return new Response(null, { status: 404 });
        if (suffix === "/_connect-existence") return new Response(null, { status: 204 });
        if (request.method === "DELETE") {
          if (rejectDelete) return new Response(null, { status: 503 });
          agents.delete(id); return new Response(null, { status: 204 });
        }
        if (suffix === "/turns" && request.method === "POST") {
          assert.ok(JSON.parse(request.headers.get("x-nanocodex-connect-connectors")).includes("chatgpt"), "turn fixture enforces managed ChatGPT capability");
          const body = await request.json();
          assert.ok(body.id || request.headers.get("idempotency-key"), "turn fixture enforces managed idempotency requirement");
          agents.get(id).push({ type: "turn_accepted", input: body.input, turn_id: body.id });
          return Response.json({ turn_id: agents.get(id).at(-1).turn_id }, { status: 202 });
        }
        if (suffix === "/realtime/calls" && request.method === "POST") {
          assert.equal(request.headers.get("x-nanocodex-voice-session-id"), voiceSessionId);
          return new Response("synthetic SDP answer", { status: 201, headers: {
            "content-type": "application/sdp", "x-nanocodex-realtime-location": "/realtime/calls/synthetic",
          } });
        }
        if (suffix === "/events") {
          let timer;
          const body = new ReadableStream({
            start(controller) {
              const send = () => controller.enqueue(new TextEncoder().encode(`data: ${JSON.stringify({ type: "turn_accepted", input: "stream canary" })}\n\n`));
              send(); timer = setInterval(send, 100);
              externalStreams.add(() => { clearInterval(timer); try { controller.close(); } catch {} });
            },
            cancel() { clearInterval(timer); },
          });
          return new Response(body, { headers: { "content-type": "text/event-stream" } });
        }
        if (suffix === "/events/history") return Response.json({ data: agents.get(id), has_more: false });
        assert.equal(suffix, "", `Unexpected managed resource ${suffix}`);
        return Response.json({ agent_id: id, accepted_turns: agents.get(id).length, active_turns: [] });
      },
      EGRESS: async request => {
        const url = new URL(request.url);
        if (url.pathname.endsWith("/connectors")) return Response.json({ connectors: {} });
        if (url.pathname.endsWith("/credentials")) return Response.json({ chatgpt: { connected: true } });
        if (url.pathname.startsWith("/subjects/")) return new Response(null, { status: 204 });
        assert.fail(`Unexpected egress path ${url.pathname}`);
      },
    },
  }));
  t.after(() => { for (const close of externalStreams) close(); return mf.dispose(); });
  const origin = (await mf.ready).origin;
  const voiceSessionId = randomUUID();
  async function call(route, { method = "GET", body, headers = {}, identity = app, connection } = {}) {
    const response = await fetch(`${origin}${route}`, { method, headers: {
      origin: identity.origin, "x-nanocodex-app-id": identity.appId,
      ...(connection ? { authorization: `Bearer ${connection.grant_token}` } : {}),
      ...(body === undefined ? {} : { "content-type": "application/json" }), ...headers,
    }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
    return response;
  }
  async function expect(response, status, label) {
    assert.equal(response.status, status, `${label}: ${await response.clone().text()}`);
    t.diagnostic(`${label}: HTTP ${status}`);
    return status === 204 ? undefined : response.json();
  }
  async function connect({ identity = app, accountAddress = account, resources = [scopeResource, historyResource], host, status = 201 } = {}) {
    const approved = ["urn:nanocodex:agent:run", "urn:nanocodex:authorization:hosted",
      "urn:nanocodex:agent:output:final", "urn:nanocodex:agent:output:actions", "urn:nanocodex:connector:chatgpt",
      `urn:nanocodex:app:${identity.appId}`, `urn:nanocodex:origin:${encodeURIComponent(identity.origin)}`, ...resources,
      ...(host ? [`urn:nanocodex:host-principal:exchange:${host}`] : [])];
    const approval = await expect(await call("/v1/hosted-authorizations", { method: "POST",
      headers: { origin: "https://nanocodex.gakonst.workers.dev" }, body: {
        app_id: identity.appId, app_origin: identity.origin, resources: approved,
        ...(host ? {} : { account_address: accountAddress, code: "c".repeat(43) }),
      } }), 200, "synthetic signed/hosted approval exchanged");
    return expect(await call("/v1/connections", { method: "POST", identity, body: {
      app_id: identity.appId, approval_id: approval.approval_id, permission: "agent.run", requested_connectors: ["chatgpt"], authorization_mode: "hosted",
      ...(host ? { principal: { kind: "host", id: host } } : { account_address: accountAddress }),
    } }), status, `grant creation ${status}`);
  }
  const connection = await connect();
  assert.ok(connection.grant.capabilities.includes("agent.threads.app"));
  const base = `/v1/grants/${connection.grant.id}`;
  const request = (suffix, options = {}) => call(`${base}${suffix}`, { connection, ...options,
    ...(suffix === "/threads" && options.method === "POST" ? { body: { operation_id: randomUUID(), ...options.body } } : {}),
  });
  const openStream = async agentId => {
    const response = await fetch(`${origin}${base}/agents/${agentId}/events`, {
      headers: { origin: app.origin, "x-nanocodex-app-id": app.appId, authorization: `Bearer ${connection.grant_token}` },
      signal: AbortSignal.timeout(8000),
    });
    assert.equal(response.status, 200);
    const reader = response.body.getReader();
    const first = await reader.read();
    assert.match(new TextDecoder().decode(first.value), /stream canary/);
    return reader;
  };
  const expectStreamClosed = async (reader, label) => {
    const started = Date.now();
    while (!(await reader.read()).done) { /* Drain until server cancels revoked source. */ }
    assert.ok(Date.now() - started < 6500, "revocation closes stream within bounded interval");
    t.diagnostic(`${label}: existing HTTP SSE stream closed within 6.5s`);
  };
  assert.deepEqual((await expect(await request("/threads"), 200, "empty app list")).threads, []);
  const firstOperation = randomUUID();
  const created = await expect(await request("/threads", { method: "POST", body: { title: "First thread", operation_id: firstOperation } }), 201, "create first thread");
  assert.equal(created.connection.grant_token, connection.grant_token);
  assert.equal(created.connection.grant.id, connection.grant.id);
  assert.equal(created.connection.grant.conversation_id, created.thread.id);
  const agentPath = `/agents/${created.connection.agent_id}`;
  const clientContext = JSON.stringify({ client: "web", timezone: "UTC" });
  await t.test("managed browser preflights allow SDK metadata without granting authority", async () => {
    // Node fetch does not enforce CORS. Check the actual workerd HTTP preflight
    // before issuing the same cross-origin SDK request headers below.
    const common = ["authorization", "x-nanocodex-app-id", "x-nanocodex-client-context"];
    const beforePreflight = upstream.length;
    for (const [suffix, method, headers] of [
      ["/agents", "GET", common],
      [agentPath, "GET", common],
      [`${agentPath}/turns`, "POST", [...common, "content-type", "idempotency-key"]],
      [`${agentPath}/events`, "GET", [...common, "last-event-id"]],
      [`${agentPath}/realtime/calls`, "POST", [...common, "content-type", "x-nanocodex-voice-session-id"]],
    ]) {
      const response = await fetch(`${origin}${base}${suffix}`, { method: "OPTIONS", headers: {
        origin: app.origin, "access-control-request-method": method,
        "access-control-request-headers": headers.join(", "),
      } });
      assert.equal(response.status, 204);
      assert.equal(response.headers.get("access-control-allow-origin"), app.origin);
      assert.equal(response.headers.get("access-control-allow-credentials"), "true");
      assert.ok(response.headers.get("access-control-allow-methods").split(/,\s*/).includes(method));
      const allowed = response.headers.get("access-control-allow-headers").toLowerCase().split(/,\s*/);
      for (const header of headers) assert.ok(allowed.includes(header), `${method} ${suffix}: CORS must allow ${header}`);
      assert.ok(!allowed.includes("*"), "credentialed requests require explicit headers");
      t.diagnostic(`OPTIONS ${suffix}: HTTP 204 permits ${method} with ${headers.join(", ")}`);
    }
    const forged = await fetch(`${origin}${base}${agentPath}`, { method: "OPTIONS", headers: {
      origin: app.origin, "access-control-request-method": "GET",
      "access-control-request-headers": "x-nanocodex-connect-user, x-unrecognized-header",
    } });
    const allowed = forged.headers.get("access-control-allow-headers").toLowerCase().split(/,\s*/);
    assert.ok(!allowed.includes("x-nanocodex-connect-user"));
    assert.ok(!allowed.includes("x-unrecognized-header"), "requested headers are not reflected");
    for (const deniedOrigin of ["null", "http://untrusted.example"]) {
      const response = await fetch(`${origin}${base}${agentPath}`, { method: "OPTIONS", headers: {
        origin: deniedOrigin, "access-control-request-method": "GET",
        "access-control-request-headers": common.join(", "),
      } });
      assert.equal(response.headers.get("access-control-allow-origin"), null);
    }
    assert.equal(upstream.length, beforePreflight, "preflights do not dispatch authenticated work");
    const stateResponse = await request(agentPath, { headers: { "x-nanocodex-client-context": clientContext } });
    assert.equal(stateResponse.headers.get("access-control-allow-origin"), app.origin);
    assert.equal((await expect(stateResponse, 200, "managed state with SDK client context")).agent_id, created.connection.agent_id);
    const voiceResponse = await request(`${agentPath}/realtime/calls`, { method: "POST", body: { sdp: "synthetic SDP offer" },
      headers: { "x-nanocodex-voice-session-id": voiceSessionId } });
    assert.equal(voiceResponse.status, 201);
    assert.equal(voiceResponse.headers.get("access-control-allow-origin"), app.origin);
    assert.ok(voiceResponse.headers.get("access-control-expose-headers").split(/,\s*/).includes("x-nanocodex-realtime-location"));
    assert.equal(voiceResponse.headers.get("x-nanocodex-realtime-location"), "/realtime/calls/synthetic");
    assert.equal(await voiceResponse.text(), "synthetic SDP answer");
    const denied = await request(agentPath, { headers: {
      "x-nanocodex-client-context": clientContext, authorization: `Bearer ${"z".repeat(43)}`,
    } });
    assert.equal(denied.headers.get("access-control-allow-origin"), app.origin, "auth failures remain browser-readable");
    await expect(denied, 401, "SDK headers do not bypass grant authentication");
    t.diagnostic("voice HTTP 201 preserves session header and exposes realtime location; unknown headers/origins remain disallowed");
  });
  await expect(await request(`${agentPath}/turns`, { method: "POST", body: { id: randomUUID(), input: "synthetic history canary" },
    headers: { "x-nanocodex-client-context": clientContext, "idempotency-key": randomUUID() } }), 202, "send on selected agent");
  const history = await expect(await request(`${agentPath}/events/history`), 200, "history on selected agent");
  assert.equal(history.data[0].input, "synthetic history canary");
  const ticket = await expect(await request(`${agentPath}/tool-host/ticket`, { method: "POST", body: {} }), 200, "selected agent tool-host ticket");
  const second = await expect(await request("/threads", { method: "POST", body: {} }), 201, "create second thread");
  assert.notEqual(second.connection.agent_id, created.connection.agent_id);
  const reopened = await expect(await request(`/threads/${created.thread.id}`), 200, "open first thread after switching");
  assert.equal(reopened.connection.agent_id, created.connection.agent_id);
  const renamed = await expect(await request(`/threads/${created.thread.id}`, { method: "PATCH", body: { title: "Renamed" } }), 200, "rename");
  assert.equal(renamed.thread.title, "Renamed");
  const fresh = await connect();
  const freshList = await expect(await call(`/v1/grants/${fresh.grant.id}/threads`, { connection: fresh }), 200, "new grant retains account app threads");
  assert.equal(freshList.threads.length, 2);
  const freshHistory = await expect(await call(`/v1/grants/${fresh.grant.id}${agentPath}/events/history`, { connection: fresh }), 200, "history survives grant renewal");
  assert.equal(freshHistory.data[0].input, "synthetic history canary");
  const renewedState = await expect(await call(`/v1/grants/${fresh.grant.id}${agentPath}`, { connection: fresh }), 200, "renewed grant reads selected agent state");
  assert.equal(renewedState.agent_id, created.connection.agent_id);
  assert.equal(renewedState.accepted_turns, 1);
  await expect(await call(`/v1/grants/${fresh.grant.id}${agentPath}/turns`, { connection: fresh, method: "POST",
    body: { id: randomUUID(), input: "renewed grant turn" } }), 202, "renewed grant sends new turn on existing agent");
  await expect(await call(`/v1/grants/${fresh.grant.id}${agentPath}/tool-host/ticket`, { connection: fresh, method: "POST", body: {} }), 200, "renewed grant gets fresh selected tool-host ticket");
  assert.equal(upstream.at(-1).grant, fresh.grant.id, "renewed work carries the current grant assertion");

  for (const options of [{ identity: { ...app, appId: "other-app" } }, { identity: { ...app, origin: "https://other.example" } },
    { accountAddress: `0x${"2".repeat(40)}` }, { host: "p".repeat(43) }]) {
    const other = await connect(options);
    const identity = options.identity ?? app;
    const otherBase = `/v1/grants/${other.grant.id}`;
    const count = upstream.length;
    assert.deepEqual((await expect(await call(`${otherBase}/threads`, { connection: other, identity }), 200, "separate owner/app/origin empty list")).threads, []);
    for (const [suffix, method] of [[`/threads/${created.thread.id}`, "GET"], [`/threads/${created.thread.id}`, "DELETE"],
      [`${agentPath}/events/history`, "GET"], [`${agentPath}/tool-host/ticket`, "POST"]]) {
      await expect(await call(`${otherBase}${suffix}`, { connection: other, identity, method }), 404, "cross-scope access denied");
    }
    assert.equal(upstream.length, count, "denied IDs never reach managed agent");
  }
  const hostA = await connect({ host: "p".repeat(43) });
  const hostB = await connect({ host: "q".repeat(43) });
  const hostThread = await expect(await call(`/v1/grants/${hostA.grant.id}/threads`, { connection: hostA, method: "POST", body: { operation_id: randomUUID() } }), 201, "host principal thread");
  await expect(await call(`/v1/grants/${hostB.grant.id}/threads/${hostThread.thread.id}`, { connection: hostB }), 404, "two principals sharing broker account isolated");
  inactiveHost = true;
  await expect(await call(`/v1/grants/${hostA.grant.id}/threads`, { connection: hostA }), 403, "host session revoked");
  inactiveHost = false;
  const legacy = await connect({ resources: [historyResource] });
  await expect(await call(`/v1/grants/${legacy.grant.id}/threads`, { connection: legacy }), 403, "legacy history grant not broadened");
  await expect(await call(`/v1/grants/${legacy.grant.id}/agents/${legacy.agent_id}`, { connection: legacy }), 200, "legacy single agent preserved");
  await connect({ resources: [scopeResource], status: 403 });
  await connect({ resources: [scopeResource, historyResource, `urn:nanocodex:agent:conversation:${randomUUID()}`], status: 403 });
  for (const headers of [{ origin: "https://other.example" }, { "x-nanocodex-app-id": "other-app" }, { authorization: `Bearer ${"z".repeat(43)}` }]) {
    await expect(await request("/threads", { headers }), 401, "token app/origin binding enforced");
  }
  await expect(await request(`/agents/${randomUUID()}/events/history`), 404, "guessed agent denied");
  await expect(await request(`/agents/${connection.agent_id}/events/history`), 404, "unregistered initial agent denied");
  await expect(await request("/threads", { method: "POST", body: { agent_id: created.connection.agent_id } }), 400, "caller cannot register an agent");
  await expect(await request(`/threads/${created.thread.id}`, { method: "PATCH", body: { title: " " } }), 400, "empty title rejected");
  await expect(await request("/threads?scope=other"), 400, "caller cannot select namespace");
  const deletedStream = await openStream(created.connection.agent_id);
  await expect(await request("/threads", { method: "POST", body: { operation_id: null } }), 400, "stable operation required");
  rejectDelete = true;
  await expect(await request(`/threads/${created.thread.id}`, { method: "DELETE" }), 503, "upstream delete failure reported");
  await expect(await request(`${agentPath}/events/history`), 404, "failed delete immediately revokes membership");
  const wsStatus = await new Promise((resolve, reject) => {
    const req = http.request(`${origin}${base}${agentPath}/tool-host?ticket=${ticket.ticket}`, {
      headers: { origin: app.origin, connection: "Upgrade", upgrade: "websocket", "sec-websocket-version": "13", "sec-websocket-key": "c3ludGhldGljLXRlc3QhIQ==" },
    }, response => { response.resume(); resolve(response.statusCode); });
    req.on("error", reject); req.end();
  });
  assert.equal(wsStatus, 404, "previously issued tool-host ticket cannot reopen deleted membership");
  t.diagnostic(`stale websocket ticket denied after delete: HTTP ${wsStatus}`);
  await expectStreamClosed(deletedStream, "tombstoned membership");
  rejectDelete = false;
  await expect(await request(`/threads/${created.thread.id}`, { method: "DELETE" }), 204, "retry completes tombstoned deletion");
  await expect(await request(`/threads/${created.thread.id}`, { method: "DELETE" }), 204, "deletion is idempotent");
  await expect(await request(`/threads/${created.thread.id}`), 404, "deleted thread cannot reopen");
  assert.equal((await expect(await request("/threads"), 200, "deleted thread absent from list")).threads.length, 1);
  await expect(await request("/threads", { method: "POST", body: { operation_id: firstOperation, title: "First thread" } }), 410, "creation retry cannot resurrect deleted thread");
  const operation = randomUUID();
  const beforeCreation = agents.size;
  const duplicates = await Promise.all(Array.from({ length: 8 }, () => request("/threads", { method: "POST", body: { operation_id: operation, title: "Once" } })));
  const results = [];
  for (const response of duplicates) {
    assert.ok([201, 503].includes(response.status));
    const value = await expect(response, response.status, "concurrent same operation");
    if (response.status === 201) results.push(value);
    else assert.equal(value.error.code, "thread_creation_unresolved");
  }
  assert.ok(results.length >= 1);
  assert.equal(new Set(results.map(value => value.thread.id)).size, 1);
  assert.equal(new Set(results.map(value => value.connection.agent_id)).size, 1);
  assert.equal(agents.size, beforeCreation + 1);
  await expect(await request("/threads", { method: "POST", body: { operation_id: operation, title: "Changed" } }), 409, "operation payload conflict");
  const renewedRetry = await expect(await call(`/v1/grants/${fresh.grant.id}/threads`, { connection: fresh, method: "POST", body: { operation_id: operation, title: "Once" } }), 201, "operation replay after grant renewal");
  assert.equal(renewedRetry.thread.id, results[0].thread.id);
  const uncertainOperation = randomUUID();
  loseNextCreateReply = true;
  const beforeUncertain = agents.size;
  await expect(await request("/threads", { method: "POST", body: { operation_id: uncertainOperation } }), 503, "lost managed create reply");
  const beforeReplay = upstream.length;
  const unresolved = await expect(await request("/threads", { method: "POST", body: { operation_id: uncertainOperation } }), 503, "same operation fences uncertain creation");
  assert.equal(unresolved.error.code, "thread_creation_unresolved");
  await expect(await call(`/v1/grants/${fresh.grant.id}/threads`, { connection: fresh, method: "POST",
    body: { operation_id: uncertainOperation } }), 503, "renewed grant cannot redispatch uncertain creation");
  assert.equal(upstream.length, beforeReplay, "uncertain retries never contact managed creation");
  assert.equal(agents.size, beforeUncertain + 1, "only the original uncertain creation reached managed");
  // Concurrent creation and pagination must not lose rows or truncate history.
  const concurrent = await Promise.all(Array.from({ length: 101 }, (_, i) =>
    request("/threads", { method: "POST", body: { title: `Parallel ${i}` } })));
  for (const response of concurrent) assert.equal(response.status, 201, await response.clone().text());
  const ids = new Set();
  let cursor;
  let pages = 0;
  do {
    const page = await expect(await request(`/threads${cursor ? `?cursor=${cursor}` : ""}`), 200, "paginated concurrent thread list");
    for (const row of page.threads) { assert.ok(!ids.has(row.id)); ids.add(row.id); }
    cursor = page.next_cursor; pages++;
  } while (cursor);
  assert.equal(ids.size, 103); assert.equal(pages, 2);
  const revokedStream = await openStream(second.connection.agent_id);
  await expect(await request("/revoke", { method: "POST", body: {} }), 200, "revoke grant");
  await expectStreamClosed(revokedStream, "revoked grant");
  const revoked = await request("/threads");
  assert.ok([401, 403].includes(revoked.status));
  t.diagnostic(`revoked grant denied: HTTP ${revoked.status}`);
  assert.ok(upstream.every(row => row.user === brokerUser && row.authorization === null && /^0x[0-9a-f]{64}$/.test(row.grant)), "only authenticated service assertions forwarded");
});
