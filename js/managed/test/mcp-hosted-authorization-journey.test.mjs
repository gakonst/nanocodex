import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile } from "node:fs/promises";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import { fetch } from "./support/miniflare-fetch.mjs";

// Production OAuth resource generation, account login, hosted authorization,
// and atomic code exchange run in workerd with real SQLite Durable Objects.
// Only SMS delivery and external wallet provisioning are synthetic. OAuth
// hooks provide deployment policy; this journey stops at the account exchange.
const source = `
import { Kv } from "accounts/server";
import { routeAccountRequest } from "./src/account-auth.ts";
import { routeAccountLinkRequest } from "./src/account-links.ts";
import { oauthMcp } from "../connect-api/src/oauthMcp.mts";
export { UserAccount, Organization, ApiKeyRecord, NonceStorage } from "./src/account-auth.ts";
export default { async fetch(request, env) {
  const url = new URL(request.url);
  const store = Kv.durableObject(env.NANOCODEX_AUTH, { name: "mcp-oauth-journey" });
  return await routeAccountLinkRequest(request, env, url)
    ?? await routeAccountRequest(request, env, url)
    ?? await oauthMcp(request, store, {
      requireDialog(request) { if (request.headers.get("origin") !== new URL(request.url).origin) throw new Error("Invalid dialog origin"); },
      consentOrigin(request) { return new URL(request.url).origin; },
      registrationAllowed: async () => true, authorizationAllowed: async () => true,
      approve: async () => { throw new Error("Not used: account exchange is tested directly"); },
      active: async () => false, revoke: async () => { throw new Error("Not used"); },
    }) ?? new Response(null, { status: 404 });
}};
`;

test("MCP consent resources cross the real hosted account authorization boundary", { timeout: 120_000 }, async t => {
  const output = new URL("../../../output/mcp-hosted-authorization/", import.meta.url);
  const trace = [];
  const bundled = await build({
    stdin: { contents: source, resolveDir: fileURLToPath(new URL("..", import.meta.url)) },
    bundle: true, write: false, format: "esm", target: "es2022", platform: "browser",
    external: ["cloudflare:workers", "node:*"],
    alias: { "node-rsa": "./node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs" },
  });
  const address = "0x" + "1".repeat(40);
  const mf = new Miniflare({
    script: bundled.outputFiles[0].text, modules: true,
    compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"],
    bindings: { ENVIRONMENT: "development", NANOCODEX_MOCK_TWILIO_VERIFY_CODE: "654321",
      NANOCODEX_OTP_HMAC_KEY: "synthetic-mcp-hosted-authorization-otp-key" },
    serviceBindings: { NANOCODEX: async () => Response.json({ address, created_at: 1 }) },
    durableObjects: {
      NANOCODEX_AUTH: { className: "NonceStorage", useSQLite: true },
      NANOCODEX_USERS: { className: "UserAccount", useSQLite: true },
      NANOCODEX_ORGANIZATIONS: { className: "Organization", useSQLite: true },
      NANOCODEX_API_KEYS: { className: "ApiKeyRecord", useSQLite: true },
    },
  });
  let base;
  async function http(path, { body, cookie, origin = "same", expected = 200, internal = false } = {}) {
    const url = new URL(path, internal ? "https://nanocodex.internal" : base);
    const options = { method: body === undefined ? "GET" : "POST", redirect: "manual", headers: {
      ...(body === undefined ? {} : { "content-type": "application/json" }),
      ...(cookie ? { cookie } : {}), ...(origin ? { origin: origin === "same" ? new URL(base).origin : origin } : {}),
    }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) };
    const response = internal ? await mf.dispatchFetch(url, options) : await fetch(url, options);
    const text = await response.text();
    const value = text ? JSON.parse(text) : null;
    trace.push({ path: url.pathname, method: options.method, expected, observed: response.status,
      input: body && !path.startsWith("/v1/auth/") ? { ...body, ...(body.code ? { code: "[redacted]" } : {}) } : undefined,
      result: path.startsWith("/v1/auth/") ? "synthetic login (redacted)" : value?.code ? { code: "[issued]" } : value });
    assert.equal(response.status, expected, `${url.pathname}: ${text}`);
    return { value, headers: response.headers };
  }
  try {
    base = await mf.ready;
    const phone = "+12025550149";
    const { value: challenge } = await http("/v1/auth/sms/start", { body: { phone }, expected: 202 });
    const login = await http("/v1/auth/sms/verify", { body: { phone, challenge_id: challenge.challenge_id, code: "654321" } });
    const cookie = login.headers.get("set-cookie").split(";")[0];
    const callback = "https://synthetic-mcp.example/callback";
    const { value: client } = await http("/oauth/register", { body: { client_name: "Synthetic hosted MCP client", redirect_uris: [callback] }, expected: 201 });
    const query = new URLSearchParams({ client_id: client.client_id, redirect_uri: callback, response_type: "code",
      resource: new URL("/mcp", base).href, code_challenge_method: "S256", code_challenge: "a".repeat(43), scope: "agent:run data:read" });
    const redirect = await http(`/oauth/authorize?${query}`, { expected: 302 });
    const requestId = new URL(redirect.headers.get("location")).searchParams.get("oauth_request");
    const { value: details } = await http(`/oauth/requests/${requestId}`);
    assert.equal(details.app_id, `mcp:${client.client_id}`);
    assert.ok(details.resources.includes(`urn:nanocodex:app:${encodeURIComponent(details.app_id)}`));
    const input = { account_address: address, app_id: details.app_id, app_origin: details.app_origin, resources: details.resources };
    const authorize = (body = input, expected = 200, session = cookie, origin = "same") => http("/v1/connect/hosted-authorization/authorize", { body, expected, cookie: session, origin });
    const { value: approved } = await authorize();
    assert.match(approved.code, /^[A-Za-z0-9_-]{43}$/);
    await authorize(input, 401, null);
    await authorize(input, 403, cookie, "https://foreign.example");
    await authorize({ ...input, account_address: "0x" + "2".repeat(40) }, 403);
    await authorize({ ...input, app_id: "mcp:another-client" }, 400);
    await authorize({ ...input, app_origin: "https://another-client.example" }, 400);
    for (const appResource of ["urn:nanocodex:app:mcp%ZZclient", "urn:nanocodex:app:mcp%253Aclient", "urn:nanocodex:app:%2Fclient"]) {
      await authorize({ ...input, resources: input.resources.map(r => r.startsWith("urn:nanocodex:app:") ? appResource : r) }, 400);
    }
    await authorize({ ...input, resources: [...input.resources, "urn:nanocodex:app:other"] }, 400);
    const exchange = { ...input, code: approved.code };
    const { value: linked } = await http("/connect/hosted-authorizations/exchange", { internal: true, body: exchange });
    assert.equal(linked.linked, true);
    assert.equal(linked.account_address, address);
    assert.deepEqual(linked.resources, input.resources);
    await http("/connect/hosted-authorizations/exchange", { internal: true, body: exchange, expected: 403 });
    // Legacy plain IDs remain accepted with the same exact signed-resource binding.
    const legacy = { ...input, app_id: "atlas-workspace", resources: input.resources.map(r => r.startsWith("urn:nanocodex:app:") ? "urn:nanocodex:app:atlas-workspace" : r) };
    const { value: legacyApproval } = await authorize(legacy);
    await http("/connect/hosted-authorizations/exchange", { internal: true, body: { ...legacy, code: legacyApproval.code } });
    t.diagnostic(`Real account route accepted OAuth-generated encoded MCP ID; exact resources survived atomic exchange; ${trace.length} HTTP responses recorded.`);
  } finally {
    await mkdir(output, { recursive: true });
    await writeFile(new URL("http-trace.json", output), JSON.stringify({
      command: "node --test js/managed/test/mcp-hosted-authorization-journey.test.mjs",
      expected: "Real persistent SMS session can authorize OAuth-generated encoded MCP app ID and exchange exactly once; unauthenticated, foreign origin, mismatched identity and malformed resources fail", trace,
    }, null, 2));
    await mf.dispose();
  }
});
