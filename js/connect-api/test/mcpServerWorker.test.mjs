import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash, randomBytes, randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import test from "node:test";

const require = createRequire(import.meta.url);
const wranglerRequire = createRequire(require.resolve("wrangler/package.json"));
const { Miniflare, Log, LogLevel, convertV4MiniflareOptions } = wranglerRequire("miniflare");
const sdkRequire = createRequire(new URL("../../nanocodex/package.json", import.meta.url));
const { Client } = sdkRequire("@modelcontextprotocol/sdk/client/index.js");
const { StreamableHTTPClientTransport } = sdkRequire("@modelcontextprotocol/sdk/client/streamableHttp.js");
const accountOrigin = "https://nanocodex.gakonst.workers.dev";
const accountAddress = `0x${"1".repeat(40)}`;
const brokerUser = "11111111-1111-4111-8111-111111111111";
const callback = "https://mcp-client.example/oauth/callback";
const opaque = () => randomBytes(32).toString("base64url");

// Run the shipped HTTP Worker in real workerd with its SQLite Durable Object.
// Only account approval and managed-agent/egress services are external fixtures.
// No Worker imports, storage substitutions, or authentication helper mocks.
// Coverage limits: hosted grants expire after 30 days with no public clock/TTL
// override. Independent parent-grant revocation needs a private Connect token,
// deliberately absent from OAuth responses; family revocation is covered below.
test("MCP HTTP journey: discovery, hosted consent, PKCE, scoped tools and token lifecycle", { timeout: 120_000 }, async t => {
  t.diagnostic("Reproduce from repository root with Node 24+: node --test js/connect-api/test/mcpServerWorker.test.mjs; all accounts, prompts and connector data are synthetic.");
  const outdir = await mkdtemp(path.join(os.tmpdir(), "nanocodex-mcp-worker-"));
  t.after(() => rm(outdir, { recursive: true, force: true }));
  await promisify(execFile)(process.execPath, [path.join(path.dirname(require.resolve("wrangler/package.json")), "bin/wrangler.js"),
    "deploy", "--dry-run", "--env=", "--config", "wrangler.jsonc", "--outdir", outdir],
  { cwd: new URL("..", import.meta.url) });
  const approvals = new Map();
  const agents = new Map();
  const upstream = [];
  const exchanged = [];
  const documents = new Map();
  const memories = new Map();
  let chatgptConnected = true;
  const connectorA = "a".repeat(43), connectorB = "b".repeat(43);
  let liveConnections = [connectorA, connectorB];
  const connectorCalls = [];
  const mf = new Miniflare(convertV4MiniflareOptions({
    modules: true, modulesRoot: outdir, scriptPath: path.join(outdir, "index.js"),
    compatibilityDate: "2026-08-23", compatibilityFlags: ["nodejs_compat"],
    durableObjects: { CONNECT_STATE: { className: "ConnectNonceStorage", useSQLite: true }, MCP_EVENTS: { className: "McpEvents", useSQLite: true } },
    log: new Log(LogLevel.ERROR),
    serviceBindings: {
      ACCOUNTS: async request => {
        const url = new URL(request.url);
        if (url.pathname === "/connect/hosted-authorizations/exchange") {
          const body = await request.json();
          exchanged.push(body);
          const signed = approvals.get(body.code);
          if (!signed || JSON.stringify(body) !== JSON.stringify(signed)) return new Response(null, { status: 403 });
          approvals.delete(body.code);
          return Response.json({ linked: true, user_id: brokerUser, account_address: body.account_address, resources: body.resources });
        }
        const encoded = await request.text();
        const body = encoded ? JSON.parse(encoded) : undefined;
        upstream.push({ path: url.pathname, method: request.method, body,
          grant: request.headers.get("x-nanocodex-connect-grant-id"),
          user: request.headers.get("x-nanocodex-connect-user"),
          authorization: request.headers.get("authorization"),
          capabilities: request.headers.get("x-nanocodex-connect-capabilities"),
          subject: request.headers.get("x-nanocodex-subject") });
        if (url.pathname === "/v1/agents" && request.method === "POST") {
          const agent_id = randomUUID().replace(/^(.{14})4/, "$17"); agents.set(agent_id, []);
          return Response.json({ agent_id });
        }
        if (url.pathname === "/v1/data") {
          if (body.operation === "document_put") {
            documents.set(body.key, body.value);
            return Response.json({ key: body.key, value: body.value, version: 1 });
          }
          if (body.operation === "document_get") return Response.json({ key: body.key, value: documents.get(body.key), version: 1 });
          assert.fail(`Unexpected data operation ${body.operation}`);
        }
        if (url.pathname === "/v1/memories/write") {
          assert.equal(body.operation, "put"); assert.equal(body.write_operation, undefined);
          assert.ok(JSON.parse(request.headers.get("x-nanocodex-connect-capabilities")).includes("memory:write"));
          memories.set(body.path, body.content);
          return Response.json({ path: body.path, saved: true });
        }
        if (url.pathname === "/v1/memories/read") {
          assert.equal(body.operation, undefined);
          assert.ok(JSON.parse(request.headers.get("x-nanocodex-connect-capabilities")).includes("memory:read"));
          return Response.json({ path: body.path, content: memories.get(body.path) });
        }
        if (url.pathname === "/v1/history/sessions/search") {
          assert.equal(body.query, "synthetic history canary");
          assert.ok(JSON.parse(request.headers.get("x-nanocodex-connect-capabilities")).includes("history:read"));
          return Response.json({ sessions: [{ session_id: "synthetic-session", turn_ids: ["synthetic-turn"] }] });
        }
        if (url.pathname === "/v1/history/sessions/synthetic-session/read") {
          assert.deepEqual(body.turn_ids, ["synthetic-turn"]); assert.equal(body.session_id, undefined);
          return Response.json({ session_id: "synthetic-session", turns: [{ id: "synthetic-turn", text: "synthetic history canary" }] });
        }
        const [, id, suffix] = url.pathname.match(/^\/v1\/agents\/([^/]+)(.*)$/) ?? [];
        assert.ok(id && agents.has(id), `Unexpected account service path ${url.pathname}`);
        if (suffix === "/_connect-existence") return new Response(null, { status: 204 });
        if (suffix === "/turns" && request.method === "POST") {
          assert.ok(body.id || request.headers.get("idempotency-key"), "agent turn has a stable id");
          agents.get(id).push(body);
          return Response.json({ turn_id: body.id, status: "accepted" }, { status: 202 });
        }
        if (suffix.startsWith("/turns/") && request.method === "GET") return Response.json({ state: "running", updated_at: Date.now() });
        if (!suffix) return Response.json({ agent_id: id, active_turns: [], accepted_turns: agents.get(id).length });
        assert.fail(`Unexpected managed resource ${suffix}`);
      },
      EGRESS: async request => {
        const url = new URL(request.url);
        if (url.pathname.endsWith("/connectors")) return Response.json({ connectors: { spotify: {
          connected: liveConnections.length > 0,
          connections: liveConnections.map((id, index) => ({ id, label: `Synthetic music ${index + 1}`, account_id: `music-${index + 1}` })),
        } } });
        if (url.origin === "https://api.spotify.com") {
          const connection = request.headers.get("x-nanocodex-connector-connection");
          const encoded = await request.text();
          const item = { url: url.href, method: request.method, connection, body: encoded ? JSON.parse(encoded) : undefined,
            authorization: request.headers.get("authorization"), subject: request.headers.get("x-nanocodex-subject"),
            contentType: request.headers.get("content-type") };
          connectorCalls.push(item);
          assert.equal(item.authorization, "Bearer NANOCODEX_PROVIDER_CREDENTIAL");
          assert.match(item.subject, /^[A-Za-z0-9_-]{43}$/);
          if (!liveConnections.includes(connection)) return Response.json({ error: "connection_removed" }, { status: 401 });
          if (request.method === "DELETE") return new Response(null, { status: 204 });
          return Response.json({ id: "synthetic-playlist", accepted: item.body ?? null });
        }
        if (url.pathname.endsWith("/credentials")) return Response.json({ chatgpt: { connected: chatgptConnected } });
        if (url.pathname.startsWith("/subjects/")) return new Response(null, { status: 204 });
        assert.fail(`Unexpected egress path ${url.pathname}`);
      },
    },
  }));
  t.after(() => mf.dispose());
  const origin = (await mf.ready).origin;
  const resource = `${origin}/mcp`;
  const transcript = [];
  async function call(route, { method = "GET", json, form, raw, token, headers = {} } = {}) {
    const response = await fetch(`${origin}${route}`, { method, redirect: "manual", headers: {
      ...(json === undefined ? {} : { "content-type": "application/json" }),
      ...(form === undefined ? {} : { "content-type": "application/x-www-form-urlencoded" }),
      ...(token ? { authorization: `Bearer ${token}` } : {}), ...headers,
    }, ...(json === undefined ? {} : { body: JSON.stringify(json) }),
    ...(form === undefined ? {} : { body: new URLSearchParams(form) }),
    ...(raw === undefined ? {} : { body: raw }) });
    transcript.push(`${method} ${route.split("?")[0]} -> ${response.status}`);
    return response;
  }
  async function expect(response, status, label) {
    assert.equal(response.status, status, `${label}: ${await response.clone().text()}`);
    t.diagnostic(`${label}: HTTP ${status}`);
    return [202, 204, 302].includes(status) ? undefined : response.json();
  }
  async function oauthError(response, error, label, status = 400) {
    const body = await expect(response, status, label);
    assert.equal(body.error, error, label);
  }
  let rpcId = 0;
  async function rpc(token, method, params, options = {}) {
    return call("/mcp", { method: "POST", token,
      json: { jsonrpc: "2.0", id: ++rpcId, method, ...(params === undefined ? {} : { params }) },
      headers: { accept: "application/json, text/event-stream", "mcp-protocol-version": "2025-06-18" }, ...options });
  }
  async function rpcResult(token, method, params, label = method, options = {}) {
    const reply = await expect(await rpc(token, method, params, options), 200, label);
    assert.equal(reply.jsonrpc, "2.0"); assert.equal(reply.id, rpcId);
    assert.equal(reply.error, undefined, JSON.stringify(reply));
    return reply.result;
  }
  t.after(() => t.diagnostic(`External managed evidence: ${JSON.stringify(upstream)}\nPublic HTTP evidence (tokens and authorization codes omitted):\n${transcript.join("\n")}`));

  const missing = await rpc(undefined, "initialize", {});
  assert.equal(missing.status, 401);
  const challenge = missing.headers.get("www-authenticate");
  assert.match(challenge, /Bearer/i);
  assert.ok(challenge.includes(`${origin}/.well-known/oauth-protected-resource/mcp`), challenge);
  const metadata = await expect(await call("/.well-known/oauth-protected-resource/mcp"), 200, "unauthenticated protected-resource discovery");
  assert.equal(metadata.resource, resource);
  assert.deepEqual(metadata.authorization_servers, [origin]);
  assert.deepEqual(await expect(await call("/.well-known/oauth-protected-resource"), 200, "root resource metadata alias"), metadata);
  const authorization = await expect(await call("/.well-known/oauth-authorization-server"), 200, "authorization server discovery");
  assert.equal(authorization.issuer, origin);
  for (const [field, route] of [["authorization_endpoint", "/oauth/authorize"], ["token_endpoint", "/oauth/token"],
    ["registration_endpoint", "/oauth/register"], ["revocation_endpoint", "/oauth/revoke"]]) assert.equal(authorization[field], origin + route);
  assert.ok(authorization.code_challenge_methods_supported.includes("S256"));
  assert.ok(authorization.token_endpoint_auth_methods_supported.includes("none"));

  const register = (fields = {}) => call("/oauth/register", { method: "POST", json: {
    client_name: "Synthetic MCP client", redirect_uris: [callback], token_endpoint_auth_method: "none", ...fields,
  } });
  for (const redirect_uris of [["https://mcp-client.example/cb#fragment"], ["http://public.example/cb"], ["https://mcp-client.example/*"], ["https://user:password@mcp-client.example/cb"], []]) {
    await oauthError(await register({ redirect_uris }), "invalid_redirect_uri", "unsafe client redirect rejected");
  }
  await oauthError(await register({ token_endpoint_auth_method: "client_secret_basic" }), "invalid_client_metadata", "confidential-client metadata rejected");
  const client = await expect(await register(), 201, "public dynamic client registration");
  assert.ok(client.client_id); assert.equal(client.client_secret, undefined);
  assert.deepEqual(client.redirect_uris, [callback]);
  const secondClient = await expect(await register(), 201, "second public client gets independent identity");
  assert.notEqual(secondClient.client_id, client.client_id);
  function authParams(fields = {}) {
    const verifier = opaque();
    return { verifier, params: { response_type: "code", client_id: client.client_id, redirect_uri: callback,
      resource, scope: "agent:run", state: opaque(), code_challenge: createHash("sha256").update(verifier).digest("base64url"),
      code_challenge_method: "S256", ...fields } };
  }
  const authorize = params => call(`/oauth/authorize?${new URLSearchParams(params)}`);
  for (const [fields, error, label] of [
    [{ redirect_uri: `${callback}/other` }, "invalid_request", "unregistered redirect"],
    [{ resource: "https://other.example/mcp" }, "invalid_target", "wrong resource audience"],
    [{ response_type: "token" }, "unsupported_response_type", "implicit response type rejected"],
    [{ scope: "agent:run admin:all" }, "invalid_scope", "unknown scope"],
    [{ scope: "connector:chatgpt" }, "invalid_scope", "model credential is not a connector API scope"],
    [{ code_challenge_method: "plain" }, "invalid_request", "plain PKCE forbidden"],
    [{ code_challenge: "" }, "invalid_request", "missing PKCE forbidden"],
  ]) {
    const { params } = authParams(fields);
    const response = await authorize(params);
    if (fields.redirect_uri !== undefined) {
      await oauthError(response, error, label);
      assert.equal(response.headers.get("location"), null, "untrusted callback never receives OAuth redirects");
    } else {
      await expect(response, 302, label);
      const location = new URL(response.headers.get("location"));
      assert.equal(location.origin + location.pathname, callback);
      assert.equal(location.searchParams.get("error"), error);
      assert.equal(location.searchParams.get("state"), params.state);
      assert.equal(location.searchParams.get("iss"), origin);
    }
  }
  const unknownClient = await authorize(authParams({ client_id: opaque() }).params);
  await oauthError(unknownClient, "invalid_client", "unregistered client denied before callback", 401);
  assert.equal(unknownClient.headers.get("location"), null);

  async function pending(fields = {}) {
    const { verifier, params } = authParams(fields);
    const response = await authorize(params);
    await expect(response, 302, "authorization redirects to account consent");
    const location = new URL(response.headers.get("location"));
    assert.equal(location.origin, origin, "local workerd keeps the consent UI on its local issuer"); assert.equal(location.pathname, "/connect-dialog");
    const requestId = location.searchParams.get("oauth_request"); assert.ok(requestId);
    const route = `/oauth/requests/${requestId}`;
    const detail = await expect(await call(route, { headers: { origin: accountOrigin } }), 200, "account reads exact pending consent");
    assert.equal(detail.client_id, params.client_id); assert.equal(detail.client_name, "Synthetic MCP client");
    assert.equal(detail.redirect_uri, params.redirect_uri); assert.equal(detail.resource, resource);
    assert.equal(detail.scope, params.scope); assert.equal(detail.app_origin, new URL(params.redirect_uri).origin);
    assert.ok(detail.resources.includes(`urn:nanocodex:app:${encodeURIComponent(detail.app_id)}`));
    assert.ok(detail.resources.includes(`urn:nanocodex:origin:${encodeURIComponent(detail.app_origin)}`));
    return { verifier, params, route, detail };
  }
  function signedApproval(request, fields = {}) {
    const code = opaque();
    const body = { account_address: accountAddress, app_id: request.detail.app_id, app_origin: request.detail.app_origin,
      code, resources: request.detail.resources, ...fields };
    approvals.set(code, body);
    return { account_address: body.account_address, code, resources: body.resources };
  }
  async function consent(request, selectedScope) {
    const resources = selectedScope === undefined ? request.detail.resources
      : [...new Set([...request.detail.base_resources, ...selectedScope.split(" ").flatMap(scope => request.detail.scope_resources[scope])])];
    const response = await expect(await call(`${request.route}/approve`, { method: "POST", headers: { origin: accountOrigin },
      json: { ...signedApproval(request, { resources }), ...(selectedScope === undefined ? {} : { scope: selectedScope }) } }), 200, "genuine account approval creates authorization code");
    const location = new URL(response.redirect_uri);
    assert.equal(location.origin + location.pathname, request.params.redirect_uri);
    assert.equal(location.searchParams.get("state"), request.params.state);
    assert.equal(location.searchParams.get("iss"), origin, "callback identifies its authorization server");
    const code = location.searchParams.get("code"); assert.ok(code);
    return code;
  }
  function exchange(request, code, fields = {}) {
    return call("/oauth/token", { method: "POST", form: { grant_type: "authorization_code", code,
      client_id: request.params.client_id, redirect_uri: request.params.redirect_uri, resource,
      code_verifier: request.verifier, ...fields } });
  }
  function refresh(token, fields = {}) {
    return call("/oauth/token", { method: "POST", form: { grant_type: "refresh_token", client_id: client.client_id,
      refresh_token: token, resource, ...fields } });
  }
  const loopback = await expect(await register({ redirect_uris: ["http://127.0.0.1/callback"] }), 201, "native client registers loopback without fixed port");
  for (const redirect_uri of ["http://localhost:43219/callback", "http://127.0.0.1:43219/other"]) {
    await oauthError(await authorize(authParams({ client_id: loopback.client_id, redirect_uri }).params), "invalid_request", "loopback exception never changes host or path");
  }
  const nativeRequest = await pending({ client_id: loopback.client_id, redirect_uri: "http://127.0.0.1:43219/callback", scope: "data:read" });
  const nativeCode = await consent(nativeRequest);
  const nativeTokens = await expect(await exchange(nativeRequest, nativeCode), 200, "native client exchanges code for selected ephemeral loopback port");
  await rpcResult(nativeTokens.access_token, "ping", undefined, "native client can use MCP without browser Origin");

  const denied = await pending();
  for (const headers of [{}, { origin: "https://attacker.example" }, { origin: "null" }]) {
    await expect(await call(denied.route, { headers }), 403, "pending consent excludes missing/forged account origin");
  }
  const deniedReply = await expect(await call(`${denied.route}/deny`, { method: "POST", json: {}, headers: { origin: accountOrigin } }), 200, "user declines consent");
  const deniedLocation = new URL(deniedReply.redirect_uri);
  assert.equal(deniedLocation.searchParams.get("error"), "access_denied");
  assert.equal(deniedLocation.searchParams.get("state"), denied.params.state);
  await expect(await call(`${denied.route}/approve`, { method: "POST", headers: { origin: accountOrigin }, json: signedApproval(denied) }), 400, "denied request cannot later approve");

  const request = await pending();
  const beforeForged = exchanged.length;
  await expect(await call(`${request.route}/approve`, { method: "POST", headers: { origin: "https://attacker.example" }, json: signedApproval(request) }), 403, "forged origin cannot approve");
  assert.equal(exchanged.length, beforeForged, "forged browser never reaches account exchange");
  await expect(await call(`${request.route}/approve`, { method: "POST", headers: { origin: accountOrigin },
    json: { account_address: accountAddress, code: opaque(), resources: request.detail.resources } }), 403, "invented hosted approval rejected by account service");
  await expect(await call(`${request.route}/approve`, { method: "POST", headers: { origin: accountOrigin },
    json: signedApproval(request, { resources: [...request.detail.resources, "urn:nanocodex:data:write"] }) }), 400, "consent cannot expand exact requested resources");
  const otherPending = await pending();
  await expect(await call(`${request.route}/approve`, { method: "POST", headers: { origin: accountOrigin }, json: signedApproval(otherPending) }), 400, "account approval binds exact request resources and identity");
  const code = await consent(request);
  await expect(await call(`${request.route}/approve`, { method: "POST", headers: { origin: accountOrigin }, json: signedApproval(request) }), 400, "completed consent cannot issue another code");
  for (const [fields, label] of [
    [{ client_id: secondClient.client_id }, "authorization code binds client"],
    [{ redirect_uri: `${callback}/other` }, "authorization code binds redirect"],
    
    [{ code_verifier: opaque() }, "authorization code binds PKCE verifier"],
  ]) await oauthError(await exchange(request, code, fields), "invalid_grant", label);
  await oauthError(await exchange(request, code, { resource: "https://other.example/mcp" }), "invalid_target", "authorization code binds audience");
  const tokens = await expect(await exchange(request, code), 200, "PKCE exchange issues access and refresh tokens");
  assert.equal(tokens.token_type.toLowerCase(), "bearer"); assert.ok(tokens.access_token); assert.ok(tokens.refresh_token);
  assert.notEqual(tokens.access_token, tokens.refresh_token); assert.equal(tokens.scope, "agent:run");
  assert.ok(tokens.expires_in > 0 && tokens.expires_in <= 3600);
  assert.equal(tokens.grant_token, undefined, "OAuth exchange keeps underlying Connect credential private");
  await oauthError(await exchange(request, code), "invalid_grant", "authorization code replay rejected");

  const initialized = await rpcResult(tokens.access_token, "initialize", { protocolVersion: "2025-06-18", capabilities: {},
    clientInfo: { name: "workerd-e2e", version: "1.0.0" } });
  assert.equal(initialized.protocolVersion, "2025-06-18"); assert.ok(initialized.capabilities.tools);
  assert.ok(initialized.serverInfo.name);
  await expect(await call("/mcp", { method: "POST", token: tokens.access_token,
    headers: { accept: "application/json, text/event-stream" }, json: { jsonrpc: "2.0", method: "notifications/initialized" } }), 202, "initialized notification accepted");
  assert.deepEqual(await rpcResult(tokens.access_token, "ping"), {});
  const listed = await rpcResult(tokens.access_token, "tools/list");
  assert.ok(listed.tools.length > 0);
  for (const tool of listed.tools) { assert.equal(typeof tool.name, "string"); assert.equal(tool.inputSchema.type, "object"); }
  const names = listed.tools.map(tool => tool.name);
  assert.deepEqual(names.sort(), ["nanocodex_connection", "nanocodex_agent_start", "nanocodex_agent_status"].sort());
  assert.ok(!names.some(name => /data|memory|history/.test(name)), "unapproved tools omitted from discovery");
  const unknown = await expect(await rpc(tokens.access_token, "unrecognized/method"), 200, "unknown JSON-RPC method");
  assert.equal(unknown.error.code, -32601);
  const unknownTool = await expect(await rpc(tokens.access_token, "tools/call", { name: "unrecognized_tool", arguments: {} }), 200, "unknown MCP tool");
  assert.ok(unknownTool.error || unknownTool.result?.isError, "unknown tool gives a protocol/tool error");
  const invalid = await expect(await call("/mcp", { method: "POST", token: tokens.access_token, headers: { accept: "application/json, text/event-stream" },
    json: { jsonrpc: "1.0", id: 99, method: "ping" } }), 400, "malformed JSON-RPC envelope rejected");
  assert.equal(invalid.error.code, -32600);
  const parseError = await expect(await call("/mcp", { method: "POST", token: tokens.access_token, raw: "{invalid-json",
    headers: { "content-type": "application/json", accept: "application/json, text/event-stream" } }), 400, "JSON-RPC parse error reported");
  assert.equal(parseError.error.code, -32700);
  await expect(await rpc(tokens.access_token, "ping", {}, { headers: { origin: "https://attacker.example", accept: "application/json, text/event-stream" } }), 403, "forged MCP browser origin rejected");
  await expect(await rpc("upstream-token-canary", "ping"), 401, "arbitrary upstream bearer cannot authenticate MCP");
  await expect(await rpc(tokens.refresh_token, "ping"), 401, "refresh token cannot authenticate resource requests");

  async function tool(token, name, args = {}, options = {}) {
    const result = await rpcResult(token, "tools/call", { name, arguments: args }, `tool ${name}`, options);
    assert.ok(!result.isError, JSON.stringify(result));
    assert.ok(Array.isArray(result.content));
    return result.structuredContent ?? JSON.parse(result.content.find(item => item.type === "text").text);
  }
  const sdkClient = new Client({ name: "official-sdk-e2e", version: "1.0.0" }, { capabilities: {} });
  const transport = new StreamableHTTPClientTransport(new URL(resource), { requestInit: {
    headers: { authorization: `Bearer ${tokens.access_token}` },
  } });
  try {
    await sdkClient.connect(transport);
    const sdkTools = await sdkClient.listTools();
    assert.deepEqual(sdkTools.tools.map(item => item.name).sort(), names);
    const sdkCall = await sdkClient.callTool({ name: "nanocodex_connection", arguments: {} });
    assert.ok(!sdkCall.isError); assert.ok(sdkCall.content.some(item => item.type === "text"));
    t.diagnostic("Official @modelcontextprotocol/sdk Client + StreamableHTTPClientTransport: initialize, initialized notification, tools/list and tools/call succeeded against real workerd HTTP.");
  } finally { await sdkClient.close(); }
  const connectionInfo = await tool(tokens.access_token, "nanocodex_connection");
  assert.ok(connectionInfo);
  const infoWire = JSON.stringify(connectionInfo);
  assert.ok(!infoWire.includes(tokens.access_token) && !infoWire.includes(tokens.refresh_token));
  assert.ok(!/grant_token|access_token|refresh_token/.test(infoWire), "connection info never discloses credentials");
  const beforeStart = upstream.length;
  const operation = randomUUID();
  await tool(tokens.access_token, "nanocodex_agent_start", { prompt: "Synthetic MCP turn canary", operation_id: operation }, { headers: {
    accept: "application/json, text/event-stream", "x-nanocodex-connect-user": "forged-user", "x-nanocodex-connect-grant-id": "forged-grant",
    "x-nanocodex-connect-capabilities": '["forged-capability"]', "x-nanocodex-subject": "forged-subject",
  } });
  assert.ok(!JSON.stringify(upstream.at(-1)).includes("forged-"), "untrusted internal assertion headers are replaced");
  assert.equal(upstream.slice(beforeStart).filter(row => row.path.endsWith("/turns")).length, 1);
  assert.ok(JSON.stringify(upstream.at(-1).body).includes("Synthetic MCP turn canary"));
  const agentStatus = await tool(tokens.access_token, "nanocodex_agent_status");
  assert.equal(agentStatus.accepted_turns, 1);
  const beforeDeniedTool = upstream.length;
  const missingScope = await expect(await rpc(tokens.access_token, "tools/call", {
    name: "nanocodex_data_write", arguments: { operation: "document_put", key: "org.example.mcp.canary", value: "denied" },
  }), 200, "unapproved data tool cannot bypass discovery filtering");
  assert.ok(missingScope.error || missingScope.result?.isError);
  assert.equal(upstream.length, beforeDeniedTool, "denied tool never reaches data service");
  await expect(await call("/v1/data", { method: "POST", token: tokens.access_token,
    headers: { origin: request.detail.app_origin, "x-nanocodex-app-id": request.detail.app_id },
    json: { operation: "document_get", key: "org.example.mcp.canary" } }), 401, "OAuth bearer cannot be passed through as a Connect credential");

  // Data-only consent works when the account has no ChatGPT credential and
  // exposes exactly data tools; a new consent cannot inherit prior agent scope.
  chatgptConnected = false;
  const dataRequest = await pending({ scope: "data:read data:write" });
  assert.ok(!dataRequest.detail.resources.includes("urn:nanocodex:connector:chatgpt"));
  const dataCode = await consent(dataRequest);
  const dataTokens = await expect(await exchange(dataRequest, dataCode), 200, "data-only consent requires no model credential");
  const dataTools = (await rpcResult(dataTokens.access_token, "tools/list")).tools.map(item => item.name).sort();
  assert.deepEqual(dataTools, ["nanocodex_connection", "nanocodex_data_read", "nanocodex_data_write"].sort());
  await tool(dataTokens.access_token, "nanocodex_data_write", { operation: "document_put", key: "org.example.mcp.canary", value: { marker: "saved through MCP" } });
  const document = await tool(dataTokens.access_token, "nanocodex_data_read", { operation: "document_get", key: "org.example.mcp.canary" });
  assert.deepEqual(document.value, { marker: "saved through MCP" });
  const readOnlyRequest = await pending({ scope: "data:read" });
  const readOnlyCode = await consent(readOnlyRequest);
  const readOnly = await expect(await exchange(readOnlyRequest, readOnlyCode), 200, "read-only data consent");
  const beforeWrite = upstream.length;
  const writeThroughRead = await expect(await rpc(readOnly.access_token, "tools/call", { name: "nanocodex_data_read",
    arguments: { operation: "document_put", key: "org.example.mcp.canary", value: "forbidden" } }), 200, "read tool cannot tunnel a write operation");
  assert.ok(writeThroughRead.error || writeThroughRead.result?.isError);
  assert.equal(upstream.length, beforeWrite);
  await oauthError(await refresh(readOnly.refresh_token, { scope: "data:read data:write" }), "invalid_scope", "refresh cannot expand approved scopes");
  const narrowedRequest = await pending({ scope: "agent:run data:read data:write" });
  const narrowedCode = await consent(narrowedRequest, "data:read");
  const narrowed = await expect(await exchange(narrowedRequest, narrowedCode), 200, "user narrows requested scopes to read-only without model credential");
  assert.equal(narrowed.scope, "data:read");
  assert.deepEqual((await rpcResult(narrowed.access_token, "tools/list")).tools.map(item => item.name).sort(), ["nanocodex_connection", "nanocodex_data_read"].sort());
  chatgptConnected = true;

  const recallRequest = await pending({ scope: "memory:read memory:write history:read" });
  const recallCode = await consent(recallRequest);
  const recallTokens = await expect(await exchange(recallRequest, recallCode), 200, "scoped team memory and history consent");
  const recallTools = (await rpcResult(recallTokens.access_token, "tools/list")).tools.map(item => item.name).sort();
  assert.deepEqual(recallTools, ["nanocodex_connection", "nanocodex_memory_read", "nanocodex_memory_write", "nanocodex_history_search", "nanocodex_history_read"].sort());
  const savedMemory = await tool(recallTokens.access_token, "nanocodex_memory_write", {
    operation: "write", write_operation: "put", path: "memory/mcp-canary.md", content: "Synthetic shared memory canary",
  });
  assert.equal(savedMemory.saved, true);
  const readMemory = await tool(recallTokens.access_token, "nanocodex_memory_read", { operation: "read", path: "memory/mcp-canary.md" });
  assert.equal(readMemory.content, "Synthetic shared memory canary");
  const historyMatches = await tool(recallTokens.access_token, "nanocodex_history_search", { query: "synthetic history canary", limit: 5 });
  const historyContent = await tool(recallTokens.access_token, "nanocodex_history_read", historyMatches.sessions[0]);
  assert.equal(historyContent.turns[0].text, "synthetic history canary");
  t.diagnostic("Memory write/read round trip and history search/read succeeded with scoped assertions; write_operation translated to backend operation=put.");

  const connectorRequest = await pending({ scope: "connector:spotify" });
  const connectorCode = await consent(connectorRequest);
  const connectorTokens = await expect(await exchange(connectorRequest, connectorCode), 200, "connector-only consent");
  const connectorTools = (await rpcResult(connectorTokens.access_token, "tools/list")).tools.map(item => item.name).sort();
  assert.deepEqual(connectorTools, ["nanocodex_connection", "nanocodex_spotify_request"].sort());
  const connectorInfo = await tool(connectorTokens.access_token, "nanocodex_connection");
  assert.deepEqual(connectorInfo.connectors.spotify.connections.map(item => item.id), [connectorA, connectorB]);
  const createdPlaylist = await tool(connectorTokens.access_token, "nanocodex_spotify_request", {
    path: "/v1/me/playlists", method: "POST", connection_id: connectorB, body: { name: "Synthetic playlist" },
  });
  assert.deepEqual(createdPlaylist.accepted, { name: "Synthetic playlist" });
  assert.equal(connectorCalls.at(-1).connection, connectorB);
  assert.equal(connectorCalls.at(-1).contentType, "application/json");
  const deletedPlaylist = await tool(connectorTokens.access_token, "nanocodex_spotify_request", {
    path: "/v1/playlists/synthetic-playlist/followers", method: "DELETE", connection_id: connectorB,
  });
  assert.equal(deletedPlaylist.status, 204, "successful empty provider response remains a successful tool receipt");
  for (const fields of [{ connection_id: "c".repeat(43) }, { path: "https://attacker.example/steal" }, { path: "//attacker.example/steal" },
    { path: "/v1/me?access_token=forged-token" }, { headers: { authorization: "Bearer forged-token" } },
    { headers: { "x-nanocodex-subject": "forged-subject" } }]) {
    const count = connectorCalls.length;
    const deniedConnector = await rpcResult(connectorTokens.access_token, "tools/call", { name: "nanocodex_spotify_request", arguments: {
      path: "/v1/me", connection_id: connectorA, ...fields,
    } }, "connector identity/path/header injection denied");
    assert.equal(deniedConnector.isError, true);
    assert.equal(connectorCalls.length, count, "invalid connector authority never reaches the broker");
  }
  liveConnections = [connectorA];
  const liveInfo = await tool(connectorTokens.access_token, "nanocodex_connection");
  assert.deepEqual(liveInfo.connectors.spotify.connections.map(item => item.id), [connectorA]);
  const removedConnector = await rpcResult(connectorTokens.access_token, "tools/call", { name: "nanocodex_spotify_request", arguments: {
    path: "/v1/me/playlists", connection_id: connectorB, method: "POST", body: { name: "must not create" },
  } }, "removed external connection denies later tool call");
  assert.equal(removedConnector.isError, true);
  t.diagnostic("Connector POST preserved JSON and selected identity; forged destinations/headers blocked before broker; removed identity returned a tool error.");

  const racingRequest = await pending({ scope: "data:read" });
  const racingCode = await consent(racingRequest);
  const codeRace = await Promise.all(Array.from({ length: 4 }, () => exchange(racingRequest, racingCode)));
  assert.deepEqual(codeRace.map(response => response.status).sort(), [200, 400, 400, 400]);
  const racingTokens = await codeRace.find(response => response.status === 200).json();
  const refreshRace = await Promise.all([refresh(racingTokens.refresh_token), refresh(racingTokens.refresh_token)]);
  assert.deepEqual(refreshRace.map(response => response.status).sort(), [200, 400]);
  const racingRotated = await refreshRace.find(response => response.status === 200).json();
  await expect(await rpc(racingRotated.access_token, "ping"), 401, "concurrent refresh reuse fences winner access token");
  await oauthError(await refresh(racingRotated.refresh_token), "invalid_grant", "concurrent refresh reuse fences winner refresh token");
  t.diagnostic("Atomic code exchange: 1 success/3 invalid_grant; concurrent refresh: 1 issuance/1 rejection and entire family unusable.");

  await expect(await call("/oauth/token", { method: "POST", headers: { origin: "https://attacker.example" }, form: {
    grant_type: "refresh_token", client_id: client.client_id, refresh_token: tokens.refresh_token, resource,
  } }), 403, "forged token-endpoint origin rejected");
  await oauthError(await refresh(tokens.refresh_token, { resource: "https://other.example/mcp" }), "invalid_target", "refresh binds resource audience");
  await oauthError(await refresh(tokens.refresh_token, { client_id: secondClient.client_id }), "invalid_grant", "refresh token binds client");
  const rotated = await expect(await refresh(tokens.refresh_token), 200, "refresh token rotation");
  assert.notEqual(rotated.refresh_token, tokens.refresh_token); assert.notEqual(rotated.access_token, tokens.access_token);
  await rpcResult(rotated.access_token, "ping", undefined, "rotated access token usable before replay");
  await oauthError(await refresh(tokens.refresh_token), "invalid_grant", "rotated refresh replay revokes family");
  await expect(await rpc(rotated.access_token, "ping"), 401, "refresh replay fences newly issued access token");
  await oauthError(await refresh(rotated.refresh_token), "invalid_grant", "refresh replay fences new refresh token");
  const recoveryRequest = await pending();
  const recoveryCode = await consent(recoveryRequest);
  const recovered = await expect(await exchange(recoveryRequest, recoveryCode), 200, "fresh user consent recovers after replay revocation");
  await rpcResult(recovered.access_token, "ping", undefined, "fresh authorization usable");
  await expect(await call("/oauth/revoke", { method: "POST", form: { token: recovered.refresh_token, client_id: client.client_id } }), 200, "revoke current token family");
  await expect(await rpc(recovered.access_token, "ping"), 401, "revocation invalidates live resource access");
  await oauthError(await refresh(recovered.refresh_token), "invalid_grant", "revocation invalidates refresh access");
  await expect(await call("/oauth/revoke", { method: "POST", form: { token: opaque(), client_id: client.client_id } }), 200, "unknown-token revocation is idempotent");
  assert.ok(upstream.length > 0);
  assert.ok(upstream.every(row => row.user === brokerUser && row.authorization === null && /^0x[0-9a-f]{64}$/.test(row.grant)),
    "external managed services receive only current Connect assertions, never OAuth access/refresh bearer tokens");
  t.diagnostic(`Synthetic account consumed genuine hosted approvals; ${upstream.length} managed calls carry bound Connect assertions without bearer pass-through.`);
});
