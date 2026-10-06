import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { createRequire } from "node:module";
import path from "node:path";
import os from "node:os";
import { promisify } from "node:util";
import test from "node:test";

const require = createRequire(import.meta.url);
const wranglerRequire = createRequire(require.resolve("wrangler/package.json"));
const { Miniflare, Log, LogLevel, convertV4MiniflareOptions } = wranglerRequire("miniflare");
const account = `0x${"1".repeat(40)}`;
const owner = "11111111-1111-4111-8111-111111111111";
const app = { appId: "service-fixture", origin: "https://services.example" };
const vaultId = "v".repeat(32), totpId = "t".repeat(32), otherId = "z".repeat(32);
const numberId = "33333333-3333-4333-8333-333333333333";
const services = { vault: { ids: [vaultId, totpId], origins: ["https://destination.example"], request: true },
  phone: { numberIds: [numberId], read: true, provision: true, release: true } };
const serviceResource = value => "urn:nanocodex:services:" + encodeURIComponent(JSON.stringify(value));

// Real HTTP/workerd + real Connect Durable Object storage. Only hosted account
// authentication and private external brokers are synthetic providers.
test("standalone services HTTP: signed scope, no managed session, isolation and live revocation", { timeout: 120_000 }, async t => {
  const outdir = await mkdtemp(path.join(os.tmpdir(), "connect-services-"));
  t.after(() => rm(outdir, { recursive: true, force: true }));
  await promisify(execFile)(process.execPath, [path.join(path.dirname(require.resolve("wrangler/package.json")), "bin/wrangler.js"),
    "deploy", "--dry-run", "--env=", "--config", "wrangler.jsonc", "--outdir", outdir], { cwd: new URL("..", import.meta.url) });
  const calls = [], managed = [], approvals = [];
  let uncertain = false;
  let dropPhoneReply = false;
  const intents = new Map();
  const mf = new Miniflare(convertV4MiniflareOptions({
    modules: true, modulesRoot: outdir, scriptPath: path.join(outdir, "index.js"),
    compatibilityDate: "2026-08-23", compatibilityFlags: ["nodejs_compat"],
    durableObjects: { CONNECT_STATE: { className: "ConnectNonceStorage", useSQLite: true } },
    log: new Log(LogLevel.ERROR),
    outboundService: () => { assert.fail("No public provider or managed-session traffic is needed"); },
    serviceBindings: {
      ACCOUNTS: async request => {
        const url = new URL(request.url);
        if (url.pathname === "/connect/hosted-authorizations/exchange") {
          const body = await request.json(); approvals.push(body);
          assert.equal(body.code, "c".repeat(43));
          return Response.json({ linked: true, user_id: owner, account_address: account, resources: body.resources });
        }
        managed.push(url.pathname);
        // A legacy agent grant is created later to prove it gains no services.
        if (url.pathname === "/v1/agents") return Response.json({ agent_id: "22222222-2222-4222-8222-222222222222" });
        return new Response(null, { status: 599 });
      },
      EGRESS: async request => {
        const url = new URL(request.url);
        if (url.pathname.startsWith("/subjects/")) return new Response(null, { status: 204 });
        calls.push({ host: url.host, path: url.pathname, method: request.method, headers: Object.fromEntries(request.headers),
          ...(["POST", "DELETE"].includes(request.method) ? { body: await request.json() } : {}) });
        assert.equal(request.headers.get("authorization"), null, "grant tokens never reach private brokers");
        assert.equal(request.headers.get("x-nanocodex-phone-human-approval"), null);
        if (url.pathname === `/users/${owner}/credentials/vault`) return Response.json({ vault: [
          { id: vaultId, kind: "login", name: "Fixture login", created_at: 1, username: "person@example.test", password: "private-canary" },
          { id: totpId, kind: "totp", name: "Fixture OTP", created_at: 2, issuer: "Fixture", account: "person@example.test",
            origin: "https://destination.example", algorithm: "SHA1", digits: 6, period: 30, seed: "private-canary" },
          { id: otherId, kind: "api_key", name: "other-private-canary", created_at: 3 },
        ] });
        if (url.host === "vault-egress.internal" && url.pathname === `/v1/users/${owner}/request`) {
          if (uncertain) return new Response("private-canary", { status: 500 });
          return Response.json({ status: 204, ok: true, body: "private-canary", code: "123456" });
        }
        if (url.host === "phone-service.internal" && url.pathname === `/v1/users/${owner}/numbers` && request.method === "GET") return Response.json({
          numbers: [{ id: numberId, phone_number: "+15555550101", status: "active", provider_sid: "private-canary" }, { id: otherId, phone_number: "+15555550999" }],
        });
        if (url.pathname === `/v1/users/${owner}/numbers/${numberId}/messages`) return Response.json({ messages: [{ id: "message1", body: "Synthetic verification 123456" }] });
        if (url.pathname === `/v1/users/${owner}/numbers/available`) return Response.json({ numbers: [{ phone_number: "+15555550101", country: "US", type: "local", provider_sid: "private-canary" }] });
        if (url.pathname === `/v1/users/${owner}/numbers` && request.method === "POST" || url.pathname === `/v1/users/${owner}/numbers/${numberId}` && request.method === "DELETE") {
          const input = calls.at(-1).body;
          const operation = { operation_id: input.operation_id, kind: request.method === "POST" ? "purchase" : "release", status: "pending_approval",
            phone_number: "+15555550101", ...(request.method === "DELETE" ? { number_id: numberId } : { quote: { id: "quote1", monthly_price: "1.00", inbound_sms_price: "0.01", currency: "usd", recurring: true } }) };
          intents.set(input.operation_id, operation);
          if (dropPhoneReply) { dropPhoneReply = false; return new Response("lost response", { status: 502 }); }
          return Response.json({ request: operation }, { status: 201 });
        }
        if (url.pathname.startsWith(`/v1/users/${owner}/requests/`)) return Response.json({ request: intents.get(url.pathname.split("/").at(-1)) });
        assert.fail(`Unexpected private route ${url}`);
      },
    },
  }));
  t.after(() => mf.dispose());
  const origin = (await mf.ready).origin;
  async function call(route, { method = "GET", body, connection, identity = app, headers = {} } = {}) {
    return fetch(origin + route, { method, headers: { origin: identity.origin, "x-nanocodex-app-id": identity.appId,
      ...(connection ? { authorization: `Bearer ${connection.grant_token}` } : {}),
      ...(body === undefined ? {} : { "content-type": "application/json" }), ...headers,
    }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
  }
  async function expect(response, status, label) {
    const text = await response.text(); assert.equal(response.status, status, `${label}: ${text}`);
    t.diagnostic(`${label}: HTTP ${status}`); return text ? JSON.parse(text) : undefined;
  }
  async function connect({ scope = services, identity = app, agent = false, extra = [], fields = {} } = {}) {
    const resources = ["urn:nanocodex:authorization:hosted", `urn:nanocodex:app:${identity.appId}`,
      `urn:nanocodex:origin:${encodeURIComponent(identity.origin)}`, ...(scope ? [serviceResource(scope)] : []),
      ...(agent ? ["urn:nanocodex:agent:run"] : []), ...extra];
    const approvalResponse = await call("/v1/hosted-authorizations", { method: "POST", identity,
      headers: { origin: "https://nanocodex.gakonst.workers.dev" }, body: { app_id: identity.appId, app_origin: identity.origin,
        account_address: account, code: "c".repeat(43), resources } });
    if (!approvalResponse.ok) return approvalResponse;
    const approval = await approvalResponse.json();
    return call("/v1/connections", { method: "POST", identity, body: { app_id: identity.appId, account_address: account,
      approval_id: approval.approval_id, permission: agent ? "agent.run" : "services.use", requested_connectors: [], authorization_mode: "hosted", ...fields } });
  }
  const connection = await expect(await connect(), 201, "exchange signed service-only approval");
  assert.equal(connection.agent_id, undefined);
  assert.deepEqual(connection.grant.services, services);
  assert(!connection.grant.capabilities.includes("nanocodex.agent"));
  assert.deepEqual(managed, [], "service-only connection must never provision agent/session");
  const base = `/v1/grants/${connection.grant.id}`;
  const request = (suffix, options = {}) => call(base + suffix, { connection, ...options });
  const catalog = await expect(await request("/services"), 200, "scoped service catalog");
  assert.deepEqual(catalog.services.map(s => s.id), ["vault", "phone"]);
  const vault = await expect(await request("/services/vault"), 200, "list selected metadata only");
  assert.deepEqual(vault.vault.map(entry => entry.id), [vaultId, totpId]);
  assert(!JSON.stringify(vault).includes("private-canary"));
  assert.equal((await expect(await request(`/services/vault/${totpId}`), 200, "read selected TOTP metadata")).entry.id, totpId);
  await expect(await request(`/services/vault/${otherId}`), 403, "deny metadata for unselected item");
  const envelope = { vault_id: vaultId, url: "https://destination.example/login", method: "POST", body: "{{NANOCODEX_VAULT_PASSWORD}}" };
  assert.deepEqual(await expect(await request("/services/vault/request", { method: "POST", body: envelope }), 200, "exact item + origin request"), { status: 204, ok: true });
  assert.deepEqual(await expect(await request("/services/vault/request", { method: "POST", body: { ...envelope, vault_id: totpId, body: "{{NANOCODEX_VAULT_TOTP}}" } }), 200, "broker-only TOTP request"), { status: 204, ok: true });
  await expect(await request("/services/vault/request", { method: "POST", body: { vault_id: vaultId, url: "https://destination.example/status", headers: { authorization: "Bearer {{NANOCODEX_VAULT_PASSWORD}}" } } }), 200, "Vault request defaults GET");
  assert.equal(calls.at(-1).body.method, "GET");
  assert.deepEqual(calls.find(call => call.host === "vault-egress.internal").body.headers, {}, "omitted Vault headers default to empty object");
  const numbers = await expect(await request("/services/phone/numbers"), 200, "list selected phone numbers");
  assert.deepEqual(numbers.numbers.map(entry => entry.id), [numberId]);
  assert(!JSON.stringify(numbers).includes("private-canary"));
  await expect(await request(`/services/phone/numbers/${numberId}/messages?limit=10`), 200, "selected number inbox");
  const available = await expect(await request("/services/phone/numbers/available?country=US&limit=10"), 200, "available numbers");
  assert(!JSON.stringify(available).includes("private-canary"));
  const purchase = await expect(await request("/services/phone/numbers", { method: "POST", body: { country: "US", phone_number: "+15555550101", operation_id: "11111111-1111-4111-8111-111111111111" } }), 201, "provision intent includes quote and awaits owner approval");
  assert.equal(purchase.request.operation_id, "11111111-1111-4111-8111-111111111111", "caller operation IDs remain stable");
  assert.notEqual(purchase.request.approval_request_id, purchase.request.operation_id, "owner approval uses grant-namespaced ID");
  await expect(await request(`/services/phone/requests/${purchase.request.operation_id}`), 200, "poll own grant intent");
  const replay = await expect(await request("/services/phone/numbers", { method: "POST", body: { operation_id: "11111111-1111-4111-8111-111111111111", phone_number: "+15555550101", country: "US" } }), 201, "identical intent retry preserves operation");
  assert.equal(replay.request.operation_id, purchase.request.operation_id);
  const beforeConflict = calls.length;
  await expect(await request("/services/phone/numbers", { method: "POST", body: { operation_id: "11111111-1111-4111-8111-111111111111", phone_number: "+15555550202", country: "US" } }), 409, "changed retry fenced before provider");
  assert.equal(calls.length, beforeConflict);
  await expect(await request(`/services/phone/numbers/${numberId}`, { method: "DELETE", body: { operation_id: "22222222-2222-4222-8222-222222222222" } }), 201, "explicit selected-number release intent");
  dropPhoneReply = true;
  const lostId = "55555555-5555-4555-8555-555555555555";
  await expect(await request("/services/phone/numbers", { method: "POST", body: { operation_id: lostId, phone_number: "+15555550101", country: "US" } }), 502, "lost intent response");
  const recovered = await expect(await request(`/services/phone/requests/${lostId}`), 200, "recover lost response with original caller UUID");
  assert.equal(recovered.request.operation_id, lostId);
  assert.equal(recovered.request.status, "pending_approval");
  assert.notEqual(recovered.request.approval_request_id, lostId);
  await expect(await request(`/services/phone/requests/${recovered.request.approval_request_id}`), 404, "approval UUID is not a second public polling namespace");
  const before = calls.length;
  for (const [suffix, options, status, label] of [
    ["/services/vault/request", { method: "POST", body: { ...envelope, vault_id: otherId } }, 403, "wrong Vault item"],
    ["/services/vault/request", { method: "POST", body: { ...envelope, url: "https://destination.example.evil.test/" } }, 403, "wrong exact origin"],
    ["/services/vault/request", { method: "POST", body: { ...envelope, user_id: "other" } }, 400, "owner injection"],
    ["/services/vault", { headers: { origin: "https://other.example" } }, 401, "cross-origin token"],
    ["/services/vault", { headers: { "x-nanocodex-app-id": "other" } }, 401, "cross-app token"],
    ["/services/vault", { headers: { authorization: `Bearer ${"z".repeat(43)}` } }, 401, "wrong token"],
    ["/services/vault", { headers: { origin: "" } }, 403, "missing origin"],
    [`/services/phone/numbers/44444444-4444-4444-8444-444444444444/messages`, {}, 403, "wrong inbox"],
    ["/services/phone/requests/request1/approve", { method: "POST", body: {}, headers: { "x-nanocodex-phone-human-approval": "true" } }, 404, "Connect cannot approve purchase"],
    ["/services/phone/requests", {}, 404, "owner request list unavailable"],
    ["/threads", { method: "POST", body: {} }, 403, "service grant cannot create thread"],
    ["/agents/any/turns", { method: "POST", body: {} }, 403, "service grant cannot run agent"],
  ]) await expect(await request(suffix, options), status, label);
  assert.equal(calls.length, before, "denied requests never reach broker");
  const other = await expect(await connect({ identity: { appId: "other-service", origin: "https://other.example" } }), 201, "second app signed scope");
  await expect(await request("/services/vault", { connection: other, identity: { appId: "other-service", origin: "https://other.example" } }), 401, "other app token cannot address first grant");
  await expect(await call(`/v1/grants/${other.grant.id}/services/phone/requests/${purchase.request.operation_id}`, { connection: other, identity: { appId: "other-service", origin: "https://other.example" } }), 404, "other grant cannot poll provision intent");
  const limited = await expect(await connect({ scope: { vault: { ...services.vault, request: false }, phone: { ...services.phone, read: false, provision: false, release: false } } }), 201, "metadata-only signed service grant");
  const deniedStart = calls.length;
  for (const [suffix, method, body] of [
    ["vault/request", "POST", envelope], ["phone/numbers", "GET"], ["phone/numbers/available", "GET"],
    ["phone/numbers", "POST", { operation_id: "11111111-1111-4111-8111-111111111111", phone_number: "+15555550101", country: "US" }],
    [`phone/numbers/${numberId}`, "DELETE", { operation_id: "22222222-2222-4222-8222-222222222222" }],
  ]) await expect(await call(`/v1/grants/${limited.grant.id}/services/${suffix}`, { connection: limited, method, body }), 403, `explicit false authority rejects ${method} ${suffix}`);
  assert.equal(calls.length, deniedStart);
  await expect(await connect({ extra: [serviceResource(services)] }), 400, "duplicate signed service scope rejected");
  await expect(await connect({ scope: { vault: { ...services.vault, origins: ["https://destination.example/"] } } }), 403, "noncanonical signed origin rejected");
  await expect(await connect({ fields: { services: { vault: { ...services.vault, ids: [otherId] } } } }), 400, "unsigned authority fields rejected");
  uncertain = true;
  const dispatched = calls.length;
  await expect(await request("/services/vault/request", { method: "POST", body: envelope }), 502, "uncertain mutation returns fixed error");
  assert.equal(calls.length, dispatched + 1, "no automatic retry");
  uncertain = false;
  await expect(await request("/revoke", { method: "POST", body: {} }), 200, "revoke original grant");
  const afterRevoke = calls.length;
  await expect(await request("/services"), 401, "live revocation blocks catalog");
  await expect(await request("/services/vault"), 401, "live revocation blocks Vault");
  await expect(await request(`/services/phone/numbers/${numberId}/messages`), 401, "live revocation blocks SMS");
  assert.equal(calls.length, afterRevoke);
  assert.deepEqual(managed, [], "entire service journey used zero managed agent/session requests");
  const old = await expect(await connect({ scope: null, agent: true }), 201, "legacy grant without service resource");
  const count = calls.length;
  await expect(await call(`/v1/grants/${old.grant.id}/services/vault`, { connection: old }), 403, "old grant inherits no Vault authority");
  await expect(await call("/v1/egress", { connection: old, method: "POST", body: { ...envelope, thread_id: "123e4567-e89b-42d3-a456-426614174000",
    headers: { "x-nanocodex-vault-id": vaultId, authorization: "Bearer {{NANOCODEX_VAULT_PASSWORD}}" } } }), 403, "legacy egress cannot bypass service scope");
  assert.equal(calls.length, count);
});
