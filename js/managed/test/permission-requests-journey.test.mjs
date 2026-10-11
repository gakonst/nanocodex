import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile, rm } from "node:fs/promises";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import { fetch } from "./support/miniflare-fetch.mjs";

// SMS delivery and wallet provisioning are external fixtures. Login, cookies,
// key authority, permission HTTP routes, data storage and restarts are real.
// Fixture subclasses only arrange elapsed time and membership changes that
// have no public mutation endpoint. No test hooks are added to production.
const source = `
import { UserAccount, Organization, ApiKeyRecord, NonceStorage,
  authenticatePersistentAccount, createApiKey, routeAccountRequest } from "./src/account-auth.ts";
import { routeUserDataRequest } from "./src/user-data-route.ts";
export { UserDataScope } from "./src/user-data-scope.ts";
export { UserAccount, NonceStorage };
export class FixtureOrganization extends Organization {
  async changeAuthority(mode, userId) {
    if (mode === "epoch") {
      const metadata = await this.ctx.storage.get("metadata");
      await this.ctx.storage.put("metadata", {...metadata, authorizationEpoch:metadata.authorizationEpoch+1});
    } else {
      const key = "membership:user:"+userId;
      const membership = await this.ctx.storage.get(key);
      await this.ctx.storage.put(key, {...membership, role:"writer", capabilities:["agents:read","api_keys:write"]});
    }
  }
}
export class FixtureApiKeyRecord extends ApiKeyRecord {
  async expireRequest(id) {
    const key = "permissionRequest:"+id;
    const request = await this.ctx.storage.get(key);
    await this.ctx.storage.put(key, {...request, expires_at:Date.now()-1});
  }
}
export default { async fetch(request, env) {
  const url = new URL(request.url);
  if (url.pathname === "/__fixture") {
    const principal = await authenticatePersistentAccount(request, env, url);
    if (!principal) return new Response(null,{status:401});
    const input = await request.json();
    if (input.operation === "key") return Response.json(await createApiKey(env,
      {...principal, role:"writer", capabilities:input.capabilities}, "synthetic-limited-key"));
    if (input.operation === "expire") {
      const key = await (await env.NANOCODEX_USERS.getByName(principal.userId)
        .fetch("https://user.internal/api-keys/"+input.key)).json();
      await env.NANOCODEX_API_KEYS.getByName(key.digest).expireRequest(input.id);
    } else {
      await env.NANOCODEX_ORGANIZATIONS.getByName(principal.organizationId)
        .changeAuthority(input.operation, principal.userId);
    }
    return Response.json({ok:true});
  }
  return await routeAccountRequest(request, env, url)
    ?? await routeUserDataRequest(request, env, url) ?? new Response(null,{status:404});
}};
`;

test("API key capability consent: real sessions, exact grants, isolation, expiry and durable retry", { timeout: 120_000 }, async () => {
  const trace = [];
  const output = new URL("../../../output/permission-requests/", import.meta.url);
  const persistence = fileURLToPath(new URL("store-" + crypto.randomUUID(), output));
  const bundled = await build({
    stdin: { contents: source, resolveDir: fileURLToPath(new URL("..", import.meta.url)) },
    bundle: true, write: false, format: "esm", target: "es2022", platform: "browser",
    external: ["cloudflare:workers", "node:*"],
    alias: { "node-rsa": "./node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs" },
  });
  const options = {
    script: bundled.outputFiles[0].text, modules: true,
    compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"],
    durableObjectsPersist: persistence + "/sqlite", r2Persist: persistence + "/r2",
    bindings: { ENVIRONMENT: "development", NANOCODEX_MOCK_TWILIO_VERIFY_CODE: "654321",
      NANOCODEX_OTP_HMAC_KEY: "synthetic-permission-request-otp-key" },
    serviceBindings: { NANOCODEX: async () => Response.json({ address: "0x" + "1".repeat(40), created_at: 1 }) },
    durableObjects: {
      NANOCODEX_AUTH: { className: "NonceStorage", useSQLite: true },
      NANOCODEX_USERS: { className: "UserAccount", useSQLite: true },
      NANOCODEX_ORGANIZATIONS: { className: "FixtureOrganization", useSQLite: true },
      NANOCODEX_API_KEYS: { className: "FixtureApiKeyRecord", useSQLite: true },
      NANOCODEX_USER_DATA: { className: "UserDataScope", useSQLite: true },
    }, r2Buckets: ["NANOCODEX_USER_DATA_OBJECTS"],
  };
  let mf = new Miniflare(options);
  let base;
  async function http(path, { method = "GET", token, cookie, body, origin, headers = {}, expected = 200 } = {}) {
    const response = await fetch(new URL(path, base), { method, headers: {
      ...(token ? { authorization: "Bearer " + token } : {}), ...(cookie ? { cookie } : {}),
      ...(body !== undefined ? { "content-type": "application/json" } : {}),
      ...(origin ? { origin: origin === "same" ? new URL(base).origin : origin } : {}), ...headers,
    }, ...(body !== undefined ? { body: JSON.stringify(body) } : {}) });
    const text = await response.text();
    const value = text ? JSON.parse(text) : null;
    trace.push({ path, method, expected, observed: response.status, result: path === "/__fixture" || path.startsWith("/v1/auth/") ? "identity fixture (redacted)" : value });
    assert.equal(response.status, expected, text);
    if (path.startsWith("/v1/permission-requests")) {
      assert.equal(response.headers.get("cache-control"), "no-store");
      assert.ok(!text.includes("digest") && !text.includes("ncx_live_"), "safe response has no credential material");
    }
    return { value, headers: response.headers };
  }
  async function login(phone) {
    const { value } = await http("/v1/auth/sms/start", { method: "POST", origin: "same", body: { phone }, expected: 202 });
    const response = await http("/v1/auth/sms/verify", { method: "POST", origin: "same", body: { phone, challenge_id: value.challenge_id, code: "654321" } });
    return response.headers.get("set-cookie").split(";")[0];
  }
  async function fixture(cookie, body) { return (await http("/__fixture", { method: "POST", cookie, body })).value; }
  async function issue(cookie, capabilities = ["agents:read"]) {
    return fixture(cookie, { operation: "key", capabilities });
  }
  const input = (capabilities = ["data:write"], operation_id = crypto.randomUUID()) => ({ operation_id, capabilities, reason: "Save the requested personal document" });
  const data = (token, body, expected) => http("/v1/data", { method: "POST", token, body, expected });
  const create = (token, body, expected = 200) => http("/v1/permission-requests", { method: "POST", token, body, expected });
  const path = (value) => `/v1/permission-requests/${value.key_id}/${value.request_id}`;
  try {
    base = await mf.ready;
    const alice = await login("+12025550141"), bob = await login("+12025550142");
    const limited = await issue(alice), other = await issue(alice), foreign = await issue(bob);
    const document = { operation: "document_put", key: "com.example/consent", value: "persisted after consent" };
    await data(limited.token, document, 403);
    const firstInput = input();
    const { value: first } = await create(limited.token, firstInput);
    assert.equal(first.status, "pending");
    assert.equal(first.key_id, limited.metadata.id);
    assert.equal(first.request_id, firstInput.operation_id);
    assert.deepEqual(first.capabilities, ["data:write"]);
    assert.equal(first.key_label, "synthetic-limited-key");
    assert.ok(first.expires_at > Date.now() && first.expires_at <= Date.now() + 15 * 60_000);
    assert.equal(new URL(first.approval_url).searchParams.get("permission_request"), first.request_id);
    assert.deepEqual((await create(limited.token, firstInput)).value, first);
    await create(limited.token, { ...firstInput, reason: "Changed intent" }, 409);
    for (const capabilities of [["api_keys:write"], ["organization:write"], ["api_keys:read"], [], ["data:write", "data:write"]]) {
      await create(limited.token, input(capabilities), 400);
    }
    await create(limited.token, { ...input(), keyId: foreign.metadata.id }, 400);
    await http("/v1/permission-requests", { method: "POST", cookie: alice, body: input(), expected: 403 });
    await http(path(first), { token: other.token, expected: 403 });
    await http(path(first), { token: foreign.token, expected: 403 });
    await http(path(first), { cookie: bob, expected: 404 });
    assert.equal((await http(path(first), { cookie: alice })).value.can_decide, true);
    const limitedReview = (await http(path(first), { token: limited.token })).value;
    assert.equal(limitedReview.can_decide, false);
    assert.equal(typeof limitedReview.capability_descriptions["data:write"], "string");
    // Even an API key already holding key-management capabilities must use
    // the owner's persistent browser session for dynamic consent decisions.
    const native = await issue(alice, ["agents:read", "api_keys:write"]);
    const nativeRequest = (await create(native.token, input())).value;
    assert.equal((await http(path(nativeRequest), { token: native.token })).value.can_decide, false);
    await http(path(first) + "/approve", { method: "POST", token: native.token, expected: 403 });
    await http(path(nativeRequest) + "/approve", { method: "POST", token: native.token, origin: "same", expected: 403 });
    await http(path(nativeRequest) + "/approve", { method: "POST", token: native.token, expected: 403 });
    await data(native.token, { ...document, key: document.key + "/native" }, 403);
    const aliceId = (await http("/v1/me", { cookie: alice })).value.user.id;
    // The service ingress really resolves these as Connect grants; they must
    // never acquire the browser-session authority needed to approve a key.
    for (const [target, method] of [["/v1/permission-requests", "POST"], [path(first), "GET"], [path(first) + "/approve", "POST"]]) {
      const response = await mf.dispatchFetch("https://nanocodex.internal" + target, { method, headers: {
        "x-nanocodex-connect-user": aliceId,
        "x-nanocodex-connect-grant-id": "0x" + "2".repeat(64),
        "x-nanocodex-connect-capabilities": JSON.stringify(["agents:read"]),
        "x-nanocodex-connect-connectors": "[]", "x-nanocodex-connect-mcp-ids": "[]",
        "origin": "https://nanocodex.internal", "content-type": "application/json",
      }, ...(method === "POST" ? { body: JSON.stringify(input()) } : {}) });
      const result = await response.json();
      trace.push({ case: "connect_rejected", path: target, expected: 403, observed: response.status, result });
      assert.equal(response.status, 403, JSON.stringify(result));
    }
    for (const action of ["approve", "deny"]) {
      await http(path(first) + "/" + action, { method: "POST", token: limited.token, origin: "same", expected: 403 });
      await http(path(first) + "/" + action, { method: "POST", cookie: bob, origin: "same", expected: 404 });
      await http(path(first) + "/" + action, { method: "POST", cookie: alice, expected: 403 });
      await http(path(first) + "/" + action, { method: "POST", cookie: alice, origin: "https://forged.example", expected: 403 });
      await http(path(first) + "/" + action, { method: "POST", cookie: alice, origin: "same", headers: { "sec-fetch-site": "cross-site" }, expected: 403 });
    }
    // Restart with a pending request; the exact request and key token survive.
    await mf.dispose(); mf = new Miniflare(options); base = await mf.ready;
    assert.equal((await http(path(first), { cookie: alice })).value.status, "pending");
    assert.equal((await http(path(first) + "/approve", { method: "POST", cookie: alice, origin: "same" })).value.status, "approved");
    await data(limited.token, document, 200);
    await data(limited.token, { operation: "document_get", key: document.key }, 403);
    assert.equal((await http(path(first) + "/approve", { method: "POST", cookie: alice, origin: "same" })).value.status, "approved");
    await mf.dispose(); mf = new Miniflare(options); base = await mf.ready;
    assert.equal((await create(limited.token, firstInput)).value.status, "approved");
    await data(limited.token, { ...document, value: "same-token retry", if_version: 1 }, 200);
    await data(limited.token, { operation: "document_get", key: document.key }, 403);
    // A denied request cannot be converted to an approval by retrying.
    const denied = (await create(limited.token, input(["data:read"]))).value;
    assert.equal((await http(path(denied) + "/deny", { method: "POST", cookie: alice, origin: "same" })).value.status, "denied");
    assert.equal((await http(path(denied) + "/approve", { method: "POST", cookie: alice, origin: "same" })).value.status, "denied");
    await data(limited.token, { operation: "document_get", key: document.key }, 403);
    const expired = (await create(limited.token, input(["data:read"]))).value;
    await fixture(alice, { operation: "expire", key: expired.key_id, id: expired.request_id });
    assert.equal((await http(path(expired) + "/approve", { method: "POST", cookie: alice, origin: "same" })).value.status, "expired");
    await data(limited.token, { operation: "document_get", key: document.key }, 403);
    // Distinct consent requests race without dropping either exact grant.
    const concurrent = await issue(alice);
    const writeRequest = (await create(concurrent.token, input())).value;
    const readRequest = (await create(concurrent.token, input(["data:read"]))).value;
    await Promise.all([writeRequest, readRequest].map(async (value) => {
      assert.equal((await http(path(value) + "/approve", { method: "POST", cookie: alice, origin: "same" })).value.status, "approved");
    }));
    await data(concurrent.token, { ...document, key: document.key + "/concurrent" }, 200);
    assert.equal((await data(concurrent.token, { operation: "document_get", key: document.key + "/concurrent" }, 200)).value.document.value, document.value);
    // Revoke through the real public endpoint; pending consent cannot recreate it.
    const revoked = (await create(other.token, input())).value;
    await http("/v1/api-keys/" + other.metadata.id, { method: "DELETE", cookie: alice, origin: "same", expected: 204 });
    await http(path(revoked) + "/approve", { method: "POST", cookie: alice, origin: "same", expected: 404 });
    await create(other.token, input(), 401);
    // The bound holds even with maximum-size Unicode reasons; a retry of an
    // existing operation still works after the key reaches its request limit.
    const bounded = await issue(alice);
    const boundedFirst = { ...input(["history:read"]), reason: "Ω".repeat(1_000) };
    await create(bounded.token, boundedFirst);
    for (let i = 1; i < 128; i++) await create(bounded.token, { ...boundedFirst, operation_id: crypto.randomUUID() });
    await create(bounded.token, input(), 429);
    assert.equal((await create(bounded.token, boundedFirst)).value.status, "pending");
    const epoch = (await create(limited.token, input(["data:read"]))).value;
    await fixture(alice, { operation: "epoch" });
    await http(path(epoch) + "/approve", { method: "POST", cookie: alice, origin: "same", expected: 409 });
    await data(limited.token, document, 401);
    // Membership no longer grants the requested capability, even though it
    // still allows this approver to manage keys and the existing key is valid.
    const membership = (await create(foreign.token, input())).value;
    await fixture(bob, { operation: "membership" });
    await http(path(membership) + "/approve", { method: "POST", cookie: bob, origin: "same", expected: 403 });
    await data(foreign.token, document, 403);
  } finally {
    await mkdir(output, { recursive: true });
    await writeFile(new URL("consent-http-trace.json", output), JSON.stringify({
      command: "node --test js/managed/test/permission-requests-journey.test.mjs",
      expected: "Only same-owner persistent browser sessions approve exact capabilities; same token works after consent and restart; rejected cross-owner, self-approval, forged origin, expiry, revoke and stale membership/epoch", trace,
    }, null, 2));
    await mf.dispose();
    await rm(persistence, { recursive: true, force: true });
  }
});
