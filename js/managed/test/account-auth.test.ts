import { afterEach, describe, expect, it, vi } from "vitest";

import {
  authenticate,
  ensureAccount,
  ensureAccountWallet,
  resolveChiefOfStaffPrincipal,
  routeAccountRequest,
  type AccountAuthEnv,
} from "../src/account-auth";
import { routeConnectorRequest } from "../src/connectors";
import { routeCredentialRequest } from "../src/credentials";

const USER_ID = "11111111-1111-4111-8111-111111111111";
const ORGANIZATION_ID = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const TEAM_ID = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const LOCAL_HMAC_KEY = "shared-local-development-hmac-key";
const CREDENTIAL_ID = "cG9ydGFibGUtY3JlZGVudGlhbA";
const SECOND_USER_ID = "22222222-2222-4222-8222-222222222222";
const SECOND_CREDENTIAL_ID = "c2Vjb25kLXBvcnRhYmxlLWNyZWRlbnRpYWw";
const PUBLIC_KEY = "0x01020304";
const SECOND_PUBLIC_KEY = "0x05060708";
const LOCAL_PASSKEY_COOKIE = "nanocodex_local_passkey";
const CONNECT_GRANT_ID = `0x${"a".repeat(64)}`;
const CONNECT_MCP_ID = "m".repeat(43);
const CONNECTOR_CONNECTION_ID = "n".repeat(43);
const APP_TOOL_CATALOG_DIGEST = `0x${"c".repeat(64)}`;
const TWILIO_ACCOUNT_SID = `AC${"d".repeat(32)}`;
const TWILIO_API_KEY_SID = `SK${"e".repeat(32)}`;
const TWILIO_API_KEY_SECRET = "test-twilio-api-key-secret";
const TWILIO_VERIFY_SERVICE_SID = `VA${"f".repeat(32)}`;

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("Connect grant assertions", () => {
  it("projects the trusted assertion to the exact live capability and tool slice", async () => {
    const { env } = portableEnv();
    const principal = await authenticate(new Request("https://nanocodex.internal/v1/agents", {
      headers: connectHeaders({
        capabilities: ["agents:read", "agents:write", "tools:use", "memory:read"],
        connectors: ["github", "gcalendar", "slack", "chatgpt"],
        connectorConnections: {
          github: [CONNECTOR_CONNECTION_ID],
          gcalendar: [CONNECTOR_CONNECTION_ID],
          slack: [CONNECTOR_CONNECTION_ID],
        },
        mcpIds: [CONNECT_MCP_ID],
        appToolCatalogDigest: APP_TOOL_CATALOG_DIGEST,
      }),
    }), env);

    expect(principal).toMatchObject({
      kind: "connect_grant",
      credentialId: CONNECT_GRANT_ID,
      capabilities: ["agents:read", "agents:write", "tools:use", "memory:read"],
      connectGrant: {
        grantId: CONNECT_GRANT_ID,
        connectors: ["github", "gcalendar", "slack", "chatgpt"],
        connectorConnections: {
          github: [CONNECTOR_CONNECTION_ID],
          gcalendar: [CONNECTOR_CONNECTION_ID],
          slack: [CONNECTOR_CONNECTION_ID],
        },
        mcpIds: [CONNECT_MCP_ID],
        appToolCatalogDigest: APP_TOOL_CATALOG_DIGEST,
      },
    });
  });

  it("rejects incomplete, malformed, duplicate, or account-widening assertions", async () => {
    const { env } = portableEnv();
    const request = (headers: HeadersInit) => authenticate(new Request(
      "https://nanocodex.internal/v1/agents",
      { headers },
    ), env);

    await expect(request({ "x-nanocodex-connect-user": USER_ID })).resolves.toBeUndefined();
    await expect(request(connectHeaders({ connectors: ["github", "github"] })))
      .resolves.toBeUndefined();
    await expect(request(connectHeaders({ capabilities: ["organization:write"] })))
      .resolves.toBeUndefined();
    await expect(request(connectHeaders({ mcpIds: ["short"] })))
      .resolves.toBeUndefined();
    await expect(request(connectHeaders({
      connectors: ["github"],
      connectorConnections: { github: ["short"] },
    }))).resolves.toBeUndefined();
    await expect(request(connectHeaders({
      connectors: ["github"],
      connectorConnections: { slack: [CONNECTOR_CONNECTION_ID] },
    }))).resolves.toBeUndefined();
    await expect(request(connectHeaders({ appToolCatalogDigest: "not-a-digest" })))
      .resolves.toBeUndefined();
    const duplicateCatalogDigest = connectHeaders({ appToolCatalogDigest: APP_TOOL_CATALOG_DIGEST });
    duplicateCatalogDigest.append(
      "x-nanocodex-connect-app-tool-catalog-digest",
      APP_TOOL_CATALOG_DIGEST,
    );
    await expect(request(duplicateCatalogDigest)).resolves.toBeUndefined();
  });
});

describe("connector route compatibility", () => {
  it("forwards legacy provider-level DELETE to unified broker bulk revoke", async () => {
    const local = portableEnv();
    const sessionToken = "d".repeat(64);
    local.set("webauthn", `session:${sessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const requests: Request[] = [];
    const env = {
      ...local.env,
      NANOCODEX: {
        async fetch(input: RequestInfo | URL, init?: RequestInit) {
          requests.push(new Request(input, init));
          return new Response(null, { status: 204 });
        },
      } as Fetcher,
    };
    const url = new URL("https://nanocodex.example/v1/connectors/gmail");
    const response = await routeConnectorRequest(new Request(url, {
      method: "DELETE",
      headers: {
        cookie: `nanocodex_account=${sessionToken}`,
        origin: url.origin,
      },
    }), env, url);

    expect(response?.status).toBe(204);
    expect(requests).toHaveLength(1);
    expect(requests[0]!.method).toBe("DELETE");
    expect(requests[0]!.url).toBe(
      `https://broker.internal/users/${USER_ID}/connectors/google`,
    );
  });
});

describe("account provisioning", () => {
  it("narrows Chief of Staff principals to the agent tool boundary", async () => {
    const { env } = portableEnv();
    const principal = await resolveChiefOfStaffPrincipal(env, USER_ID, `chief:${"a".repeat(64)}`);

    expect(principal).toMatchObject({
      kind: "service",
      userId: USER_ID,
      capabilities: ["agents:read", "agents:write", "tools:use"],
    });
    expect(principal?.capabilities).not.toContain("organization:write");
    expect(principal?.capabilities).not.toContain("api_keys:write");
    expect(principal?.capabilities).not.toContain("agents:portability");
  });

  it("accepts a matching persistent account after a create conflict", async () => {
    const requests: string[] = [];
    const env = accountEnv(async (request) => {
      requests.push(request.method);
      return request.method === "PUT"
        ? new Response(null, { status: 409 })
        : Response.json(account(USER_ID, true));
    });

    await expect(ensureAccount(env, USER_ID, true)).resolves.toBeUndefined();
    expect(requests).toEqual(["PUT", "GET"]);
  });

  it("rejects a conflict owned by another account", async () => {
    const env = accountEnv(async (request) => request.method === "PUT"
      ? new Response(null, { status: 409 })
      : Response.json(account("22222222-2222-4222-8222-222222222222", true)));

    await expect(ensureAccount(env, USER_ID, true)).rejects.toThrow("account provisioning failed");
  });

  it("does not promote an anonymous account without a successful write", async () => {
    const env = accountEnv(async (request) => request.method === "PUT"
      ? new Response(null, { status: 409 })
      : Response.json(account(USER_ID, false)));

    await expect(ensureAccount(env, USER_ID, true)).rejects.toThrow("account provisioning failed");
  });
});

describe("SMS OTP authentication", () => {
  it("uses a fixed Verify code only in the explicit development environment", async () => {
    const local = portableEnv();
    local.env.NANOCODEX_OTP_HMAC_KEY = "test-sms-otp-hmac-key-with-at-least-thirty-two-bytes";
    local.env.ENVIRONMENT = "development";
    local.env.NANOCODEX_MOCK_TWILIO_VERIFY_CODE = "123456";
    const origin = "https://nanocodex.example";
    const started = await beginSmsOtp(local.env, origin, "+14155550120");
    expect(started.response.status).toBe(202);

    const rejected = await completeSmsOtp(
      local.env,
      origin,
      "+14155550120",
      started.challengeId,
      "000000",
    );
    expect(rejected.status).toBe(400);
    await expect(rejected.json()).resolves.toEqual({
      error: "invalid_or_expired_otp",
    });

    const approved = await completeSmsOtp(
      local.env,
      origin,
      "+14155550120",
      started.challengeId,
      "123456",
    );
    expect(approved.status).toBe(200);
    expect(approved.headers.get("set-cookie")).toMatch(/^nanocodex_account=s_/);

    const production = portableEnv();
    production.env.NANOCODEX_OTP_HMAC_KEY = local.env.NANOCODEX_OTP_HMAC_KEY;
    production.env.ENVIRONMENT = "production";
    production.env.NANOCODEX_MOCK_TWILIO_VERIFY_CODE = "123456";
    const unavailable = await beginSmsOtp(production.env, origin, "+14155550121");
    expect(unavailable.response.status).toBe(503);
  });

  it("creates and restores one persistent account without exposing the phone number", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-09-02T12:00:00Z"));
    try {
      const local = portableEnv();
      local.env.NANOCODEX_OTP_HMAC_KEY = "test-sms-otp-hmac-key-with-at-least-thirty-two-bytes";
      const twilio = mockTwilioVerify(local.env);
      const origin = "https://nanocodex.example";

      const first = await beginSmsOtp(local.env, origin, "+30 (690) 000-0000");
      expect(first.response.status).toBe(202);
      expect(first.challengeId).toMatch(/^[A-Za-z0-9_-]{43}$/);
      expect(twilio.calls[0]).toEqual({
        authorization: `Basic ${btoa(`${TWILIO_API_KEY_SID}:${TWILIO_API_KEY_SECRET}`)}`,
        form: {
          Channel: "sms",
          To: "+306900000000",
        },
        url: `https://verify.twilio.com/v2/Services/${TWILIO_VERIFY_SERVICE_SID}/Verifications`,
      });
      expect(JSON.stringify([...local.values("sms-otp")])).not.toContain("+306900000000");
      expect(JSON.stringify([...local.values("sms-otp")])).not.toContain("123456");

      twilio.checkStatus = "pending";
      const wrong = await completeSmsOtp(
        local.env,
        origin,
        "+306900000000",
        first.challengeId,
        "999999",
      );
      expect(wrong.status).toBe(400);
      await expect(wrong.json()).resolves.toEqual({ error: "invalid_or_expired_otp" });

      twilio.checkStatus = "approved";
      const verified = await completeSmsOtp(
        local.env,
        origin,
        "+306900000000",
        first.challengeId,
        "123456",
      );
      expect(verified.status).toBe(200);
      expect(twilio.calls[2]).toEqual({
        authorization: `Basic ${btoa(`${TWILIO_API_KEY_SID}:${TWILIO_API_KEY_SECRET}`)}`,
        form: {
          Code: "123456",
          VerificationSid: `VE${"0".repeat(31)}1`,
        },
        url: `https://verify.twilio.com/v2/Services/${TWILIO_VERIFY_SERVICE_SID}/VerificationCheck`,
      });
      const firstAccount = await verified.clone().json<{
        user: { address: string; id: string; persistent: boolean };
      }>();
      expect(firstAccount.user).toEqual({
        address: "0x1111111111111111111111111111111111111111",
        id: expect.any(String),
        persistent: true,
      });
      const cookie = verified.headers.get("set-cookie")!.split(";", 1)[0]!;
      expect(cookie).toMatch(/^nanocodex_account=s_[A-Za-z0-9_-]{43}$/);

      const current = await routeAccountRequest(new Request(`${origin}/v1/me`, {
        headers: { cookie },
      }), local.env, new URL(`${origin}/v1/me`));
      await expect(current!.json()).resolves.toMatchObject(firstAccount);

      const loggedOut = await routeAccountRequest(new Request(`${origin}/v1/auth/logout`, {
        method: "POST",
        headers: { cookie, origin },
      }), local.env, new URL(`${origin}/v1/auth/logout`));
      expect(loggedOut?.status).toBe(204);

      vi.setSystemTime(new Date("2026-09-02T12:01:01Z"));
      local.deleteMatching("sms-otp", "cooldown:");
      const second = await beginSmsOtp(local.env, origin, "+306900000000", "198.51.100.2");
      expect(second.response.status).toBe(202);
      const restored = await completeSmsOtp(
        local.env,
        origin,
        "+306900000000",
        second.challengeId,
        "654321",
      );
      await expect(restored.json()).resolves.toEqual(firstAccount);
    } finally {
      vi.useRealTimers();
    }
  });

  it("promotes only the anonymous pre-login user and never aliases a new phone to a persistent account", async () => {
    const local = portableEnv();
    local.env.NANOCODEX_OTP_HMAC_KEY = "test-sms-otp-hmac-key-with-at-least-thirty-two-bytes";
    mockTwilioVerify(local.env);
    const origin = "https://nanocodex.example";
    const meUrl = new URL("/v1/me", origin);
    const anonymous = await routeAccountRequest(new Request(meUrl), local.env, meUrl);
    const anonymousUser = await anonymous!.clone().json<{ user: { id: string; persistent: boolean } }>();
    const anonymousCookie = anonymous!.headers.get("set-cookie")!.split(";", 1)[0]!;
    expect(anonymousUser.user.persistent).toBe(false);

    const first = await beginSmsOtp(local.env, origin, "+14155550125", "198.51.100.10", anonymousCookie);
    const promoted = await completeSmsOtp(
      local.env,
      origin,
      "+14155550125",
      first.challengeId,
      "123456",
      anonymousCookie,
    );
    const promotedUser = await promoted.clone().json<{
      user: { address: string; id: string; persistent: boolean };
    }>();
    const persistentCookie = promoted.headers.get("set-cookie")!.split(";", 1)[0]!;
    expect(promotedUser.user).toEqual({
      address: "0x1111111111111111111111111111111111111111",
      id: anonymousUser.user.id,
      persistent: true,
    });

    const second = await beginSmsOtp(local.env, origin, "+14155550126", "198.51.100.11", persistentCookie);
    const separate = await completeSmsOtp(
      local.env,
      origin,
      "+14155550126",
      second.challengeId,
      "654321",
      persistentCookie,
    );
    const separateUser = await separate.json<{ user: { id: string; persistent: boolean } }>();
    expect(separateUser.user.persistent).toBe(true);
    expect(separateUser.user.id).not.toBe(promotedUser.user.id);
  });

  it("enforces browser origin, resend limits, and configured delivery", async () => {
    const local = portableEnv();
    local.env.NANOCODEX_OTP_HMAC_KEY = "test-sms-otp-hmac-key-with-at-least-thirty-two-bytes";
    mockTwilioVerify(local.env);
    const origin = "https://nanocodex.example";

    const crossOrigin = await routeAccountRequest(new Request(`${origin}/v1/auth/sms/start`, {
      method: "POST",
      headers: { "content-type": "application/json", origin: "https://evil.example" },
      body: JSON.stringify({ phone: "+14155550123" }),
    }), local.env, new URL(`${origin}/v1/auth/sms/start`));
    expect(crossOrigin?.status).toBe(403);

    const oversized = await routeAccountRequest(new Request(`${origin}/v1/auth/sms/start`, {
      method: "POST",
      headers: { "content-type": "application/json", origin },
      body: JSON.stringify({ phone: `+1${"2".repeat(1_100)}` }),
    }), local.env, new URL(`${origin}/v1/auth/sms/start`));
    expect(oversized?.status).toBe(413);

    const first = await beginSmsOtp(local.env, origin, "+14155550123");
    expect(first.response.status).toBe(202);
    const limited = await beginSmsOtp(local.env, origin, "+14155550123");
    expect(limited.response.status).toBe(429);
    expect(limited.response.headers.get("retry-after")).toBe("60");

    const unavailable = portableEnv();
    unavailable.env.NANOCODEX_OTP_HMAC_KEY = local.env.NANOCODEX_OTP_HMAC_KEY;
    const failed = await beginSmsOtp(unavailable.env, origin, "+14155550124");
    expect(failed.response.status).toBe(503);
    mockTwilioVerify(unavailable.env);
    const retried = await beginSmsOtp(unavailable.env, origin, "+14155550124");
    expect(retried.response.status).toBe(202);
  });

  it("fails closed on provider errors and preserves the challenge for retry", async () => {
    const local = portableEnv();
    local.env.NANOCODEX_OTP_HMAC_KEY = "test-sms-otp-hmac-key-with-at-least-thirty-two-bytes";
    const twilio = mockTwilioVerify(local.env);
    const origin = "https://nanocodex.example";
    const started = await beginSmsOtp(local.env, origin, "+14155550124");
    twilio.checkHttpStatus = 404;
    const rejected = await completeSmsOtp(
      local.env,
      origin,
      "+14155550124",
      started.challengeId,
      "000000",
    );
    expect(rejected.status).toBe(400);
    await expect(rejected.json()).resolves.toEqual({ error: "invalid_or_expired_otp" });

    twilio.checkHttpStatus = 503;
    const unavailable = await completeSmsOtp(
      local.env,
      origin,
      "+14155550124",
      started.challengeId,
      "123456",
    );
    expect(unavailable.status).toBe(503);
    await expect(unavailable.json()).resolves.toEqual({ error: "sms_verification_failed" });

    twilio.checkHttpStatus = 200;
    twilio.checkStatus = "approved";
    const retried = await completeSmsOtp(
      local.env,
      origin,
      "+14155550124",
      started.challengeId,
      "123456",
    );
    expect(retried.status).toBe(200);
  });

  it("cleans up failed delivery so the same phone can retry immediately", async () => {
    const local = portableEnv();
    local.env.NANOCODEX_OTP_HMAC_KEY = "test-sms-otp-hmac-key-with-at-least-thirty-two-bytes";
    const twilio = mockTwilioVerify(local.env);
    const origin = "https://nanocodex.example";
    twilio.startHttpStatus = 503;

    const unavailable = await beginSmsOtp(local.env, origin, "+14155550127");
    expect(unavailable.response.status).toBe(503);
    await expect(unavailable.response.json()).resolves.toEqual({ error: "sms_delivery_failed" });

    twilio.startHttpStatus = 201;
    const retried = await beginSmsOtp(local.env, origin, "+14155550127");
    expect(retried.response.status).toBe(202);
  });

  it("supports account SID and auth token credentials", async () => {
    const local = portableEnv();
    local.env.NANOCODEX_OTP_HMAC_KEY = "test-sms-otp-hmac-key-with-at-least-thirty-two-bytes";
    const twilio = mockTwilioVerify(local.env);
    delete local.env.TWILIO_API_KEY_SID;
    delete local.env.TWILIO_API_KEY_SECRET;
    local.env.TWILIO_AUTH_TOKEN = "test-twilio-auth-token";

    const started = await beginSmsOtp(local.env, "https://nanocodex.example", "+14155550128");
    expect(started.response.status).toBe(202);
    expect(twilio.calls[0]?.authorization).toBe(
      `Basic ${btoa(`${TWILIO_ACCOUNT_SID}:test-twilio-auth-token`)}`,
    );
  });
});

describe("managed wallet bridge", () => {
  it("bounds broker wallet provisioning so OTP verification cannot hang forever", async () => {
    const local = portableEnv();
    local.env.NANOCODEX = {
      fetch: () => new Promise<Response>(() => {}),
    } as unknown as Fetcher;

    await expect(ensureAccountWallet(local.env, USER_ID, 1)).rejects.toThrow("wallet unavailable");
  });

  it("includes the broker wallet address in OTP success and leaves a failed wallet provision retryable", async () => {
    const local = portableEnv();
    local.env.NANOCODEX_OTP_HMAC_KEY = "test-sms-otp-hmac-key-with-at-least-thirty-two-bytes";
    mockTwilioVerify(local.env);
    let available = false;
    local.env.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit) {
        const request = new Request(input, init);
        if (request.method !== "PUT") return new Response(null, { status: 404 });
        return available
          ? Response.json({ address: "0x2222222222222222222222222222222222222222", created_at: 2 })
          : new Response(null, { status: 503 });
      },
    } as Fetcher;
    const origin = "https://nanocodex.example";
    const started = await beginSmsOtp(local.env, origin, "+14155550129");

    const unavailable = await completeSmsOtp(
      local.env,
      origin,
      "+14155550129",
      started.challengeId,
      "123456",
    );
    expect(unavailable.status).toBe(503);
    await expect(unavailable.json()).resolves.toEqual({ error: "wallet_unavailable" });
    expect(unavailable.headers.get("set-cookie")).toBeNull();

    available = true;
    const retried = await completeSmsOtp(
      local.env,
      origin,
      "+14155550129",
      started.challengeId,
      "123456",
    );
    expect(retried.status).toBe(200);
    await expect(retried.json()).resolves.toMatchObject({
      user: { address: "0x2222222222222222222222222222222222222222", persistent: true },
    });
  });

  it("requires a persistent session and same-origin mutations before forwarding wallet requests", async () => {
    const local = portableEnv();
    const requests: Request[] = [];
    local.env.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit) {
        requests.push(new Request(input, init));
        return Response.json({ accepted: true }, { status: 202, headers: { "x-wallet": "preserved" } });
      },
    } as Fetcher;
    const origin = "https://nanocodex.example";
    const url = new URL("/v1/wallet/connect", origin);
    const anonymous = await routeAccountRequest(new Request(url, {
      method: "POST",
      headers: { "content-type": "application/json", origin },
      body: "{}",
    }), local.env, url);
    expect(anonymous?.status).toBe(401);

    const cookie = persistentAccountCookie(local, USER_ID, "d");
    const crossOrigin = await routeAccountRequest(new Request(url, {
      method: "POST",
      headers: { cookie, "content-type": "application/json", origin: "https://evil.example" },
      body: "{}",
    }), local.env, url);
    expect(crossOrigin?.status).toBe(403);

    const rejectedMaterial = await routeAccountRequest(new Request(url, {
      method: "POST",
      headers: { cookie, "content-type": "application/json", origin },
      body: JSON.stringify({ private_key: "0xdeadbeef" }),
    }), local.env, url);
    expect(rejectedMaterial?.status).toBe(400);
    expect(requests).toHaveLength(0);

    const publicScopeAddress = "0x3333333333333333333333333333333333333333";
    const allowedPublicAddress = await routeAccountRequest(new Request(url, {
      method: "POST",
      headers: { cookie, "content-type": "application/json", origin },
      body: JSON.stringify({
        request: {
          method: "wallet_connect",
          params: [{
            capabilities: {
              authorizeAccessKey: {
                scopes: [{ address: publicScopeAddress, type: "contract" }],
              },
            },
          }],
        },
      }),
    }), local.env, url);
    expect(allowedPublicAddress?.status).toBe(202);
    expect(requests).toHaveLength(1);
    await expect(requests[0]!.json()).resolves.toMatchObject({
      request: {
        params: [{ capabilities: { authorizeAccessKey: { scopes: [{ address: publicScopeAddress }] } } }],
      },
    });

    const balanceUrl = new URL("/v1/wallet/balance", origin);
    const anonymousBalance = await routeAccountRequest(new Request(balanceUrl), local.env, balanceUrl);
    expect(anonymousBalance?.status).toBe(401);
    const balance = await routeAccountRequest(new Request(balanceUrl, {
      headers: { cookie },
    }), local.env, balanceUrl);
    expect(balance?.status).toBe(202);
    expect(requests[1]?.url).toBe(`https://broker.internal/users/${USER_ID}/wallet/balance`);
  });

  it("forwards each wallet operation only to its authenticated user and uses the canonical address for /v1/me", async () => {
    const local = portableEnv();
    const requests: Request[] = [];
    local.env.NANOCODEX = {
      async fetch(input: RequestInfo | URL, init?: RequestInit) {
        const request = new Request(input, init);
        requests.push(request);
        return Response.json({
          address: new URL(request.url).pathname.includes(SECOND_USER_ID)
            ? "0x2222222222222222222222222222222222222222"
            : "0x1111111111111111111111111111111111111111",
          created_at: 1,
        });
      },
    } as Fetcher;
    const origin = "https://nanocodex.example";
    const firstCookie = persistentAccountCookie(local, USER_ID, "d");
    const secondCookie = persistentAccountCookie(local, SECOND_USER_ID, "e");
    local.set("account", `address:${USER_ID}`, "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    const firstUrl = new URL("/v1/wallet/connect", origin);
    const first = await routeAccountRequest(new Request(firstUrl, {
      method: "POST",
      headers: { cookie: firstCookie, "content-type": "application/json", origin },
      body: JSON.stringify({ request: { method: "wallet_connect", params: [{}] } }),
    }), local.env, firstUrl);
    expect(first?.status).toBe(200);

    const secondUrl = new URL("/v1/wallet/revoke-access-key", origin);
    await routeAccountRequest(new Request(secondUrl, {
      method: "POST",
      headers: { cookie: secondCookie, "content-type": "application/json", origin },
      body: JSON.stringify({ request: { method: "wallet_revokeAccessKey", params: [{ key_id: "key_123" }] } }),
    }), local.env, secondUrl);
    const mutations = requests.filter((request) => request.method === "POST");
    expect(mutations.map((request) => request.url)).toEqual([
      `https://broker.internal/users/${USER_ID}/wallet/connect`,
      `https://broker.internal/users/${SECOND_USER_ID}/wallet/revoke-access-key`,
    ]);
    await expect(mutations[0]!.json()).resolves.toEqual({
      request: { method: "wallet_connect", params: [{}] },
    });
    await expect(mutations[1]!.json()).resolves.toEqual({
      request: {
        method: "wallet_revokeAccessKey",
        params: [{ address: "0x2222222222222222222222222222222222222222", key_id: "key_123" }],
      },
    });

    const meUrl = new URL("/v1/me", origin);
    const me = await routeAccountRequest(new Request(meUrl, { headers: { cookie: firstCookie } }), local.env, meUrl);
    await expect(me?.json()).resolves.toMatchObject({
      user: { address: "0x1111111111111111111111111111111111111111", id: USER_ID },
    });
  });
});

describe("local WebAuthn credential portability", () => {
  it("uses one parent RP ID while retaining exact per-request origin checks", async () => {
    for (const [origin, rpId] of [
      ["http://nanocodex.localhost:5173", "nanocodex.localhost"],
      ["http://branch.nanocodex.localhost", "nanocodex.localhost"],
      ["http://branch.nanocodex.localhost:20735", "nanocodex.localhost"],
      ["https://branch.nanocodex.localhost:20735", "nanocodex.localhost"],
      ["https://nanocodex.example", "nanocodex.example"],
      ["https://localhost", "localhost"],
      ["https://nanocodex.local", "nanocodex.local"],
      ["http://branch.example", "branch.example"],
    ]) {
      const { env } = portableEnv();
      const response = await routeAccountRequest(new Request(`${origin}/webauthn/login/options`, {
        method: "POST",
        headers: { "content-type": "application/json", origin },
        body: "{}",
      }), env, new URL(`${origin}/webauthn/login/options`));
      expect(response?.status).toBe(200);
      const body = await response!.json<{ options: { publicKey: { rpId: string } } }>();
      expect(body.options.publicKey.rpId).toBe(rpId);
    }

    const { env } = portableEnv();
    const rejected = await routeAccountRequest(new Request(
      "http://branch.nanocodex.localhost:20735/webauthn/login/options",
      {
        method: "POST",
        headers: {
          "content-type": "application/json",
          origin: "http://branch.nanocodex.localhost:20736",
        },
        body: "{}",
      },
    ), env, new URL("http://branch.nanocodex.localhost:20735/webauthn/login/options"));
    expect(rejected?.status).toBe(403);
  });

  it("carries one signed credential into an isolated local auth store", async () => {
    const source = portableEnv();
    const sessionToken = "a".repeat(64);
    source.set("webauthn", `session:${sessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });

    const migrated = await routeAccountRequest(new Request("http://one.nanocodex.localhost:20735/v1/me", {
      headers: { cookie: `nanocodex_account=${sessionToken}` },
    }), source.env, new URL("http://one.nanocodex.localhost:20735/v1/me"));
    expect(migrated?.status).toBe(200);
    const setCookie = localPasskeySetCookie(migrated!.headers);
    expect(setCookie).toBeDefined();
    expect(setCookie).toContain("Domain=nanocodex.localhost");
    expect(setCookie).toContain("HttpOnly");
    expect(setCookie).toContain("SameSite=Lax");
    expect(setCookie).toContain("Secure");
    const portableCookie = setCookie!.split(";", 1)[0]!;
    const payload = portableCookie.split("=", 2)[1]!.split(".", 1)[0]!;
    expect(JSON.parse(decodeBase64Url(payload))).toEqual({
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: USER_ID,
    });

    const target = portableEnv();
    const options = await routeAccountRequest(new Request(
      "http://two.nanocodex.localhost:20736/webauthn/login/options",
      {
        method: "POST",
        headers: {
          cookie: portableCookie,
          "content-type": "application/json",
          origin: "http://two.nanocodex.localhost:20736",
        },
        body: JSON.stringify({ credentialId: CREDENTIAL_ID }),
      },
    ), target.env, new URL("http://two.nanocodex.localhost:20736/webauthn/login/options"));
    expect(await options!.json()).toMatchObject({
      options: {
        publicKey: {
          allowCredentials: [{ id: CREDENTIAL_ID }],
        },
      },
    });

    const attempted = await loginWithPortableCookie(target.env, portableCookie, CREDENTIAL_ID);
    expect(attempted.status).toBe(400);
    expect(target.get("webauthn", `credential:${CREDENTIAL_ID}`)).toEqual({
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
    });
  });

  it("targets the selected older account after logout while the portable hint is newer", async () => {
    const target = portableEnv();
    const latestSessionToken = "b".repeat(64);
    target.set("webauthn", `credential:${CREDENTIAL_ID}`, {
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
    });
    target.set("webauthn", `credential:${SECOND_CREDENTIAL_ID}`, {
      publicKey: SECOND_PUBLIC_KEY,
      userId: encodeUserId(SECOND_USER_ID),
    });
    target.set("webauthn", `session:${latestSessionToken}`, {
      credentialId: SECOND_CREDENTIAL_ID,
      publicKey: SECOND_PUBLIC_KEY,
      userId: encodeUserId(SECOND_USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const origin = "http://two.nanocodex.localhost:20736";
    const current = await routeAccountRequest(new Request(`${origin}/v1/me`, {
      headers: { cookie: `nanocodex_account=${latestSessionToken}` },
    }), target.env, new URL(`${origin}/v1/me`));
    const portableCookie = localPasskeySetCookie(current!.headers)!.split(";", 1)[0]!;

    const logout = await routeAccountRequest(new Request(`${origin}/webauthn/logout`, {
      method: "POST",
      headers: { cookie: `nanocodex_account=${latestSessionToken}`, origin },
    }), target.env, new URL(`${origin}/webauthn/logout`));
    expect(logout?.status).toBe(204);
    expect(target.get("webauthn", `session:${latestSessionToken}`)).toBeUndefined();

    const selected = await routeAccountRequest(new Request(`${origin}/webauthn/login/options`, {
      method: "POST",
      headers: {
        cookie: portableCookie,
        "content-type": "application/json",
        origin,
      },
      body: JSON.stringify({ credentialId: CREDENTIAL_ID }),
    }), target.env, new URL(`${origin}/webauthn/login/options`));
    expect(selected?.status).toBe(200);
    const selectedBody = await selected!.json<{
      options: { publicKey: { allowCredentials: { id: string }[]; challenge: string } };
    }>();
    expect(selectedBody.options.publicKey.allowCredentials).toEqual([
      { id: CREDENTIAL_ID, type: "public-key" },
    ]);

    const mismatched = await routeAccountRequest(new Request(`${origin}/webauthn/login`, {
      method: "POST",
      headers: {
        cookie: portableCookie,
        "content-type": "application/json",
        origin,
      },
      body: JSON.stringify({
        id: SECOND_CREDENTIAL_ID,
        metadata: {
          clientDataJSON: JSON.stringify({ challenge: selectedBody.options.publicKey.challenge }),
        },
      }),
    }), target.env, new URL(`${origin}/webauthn/login`));
    expect(mismatched?.status).toBe(400);
    expect(await mismatched!.json()).toEqual({ error: "selected_credential_mismatch" });
  });

  it("refuses an unknown selected credential instead of falling back to the portable hint", async () => {
    const source = portableEnv();
    const sessionToken = "b".repeat(64);
    source.set("webauthn", `session:${sessionToken}`, {
      credentialId: SECOND_CREDENTIAL_ID,
      publicKey: SECOND_PUBLIC_KEY,
      userId: encodeUserId(SECOND_USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const origin = "http://two.nanocodex.localhost:20736";
    const current = await routeAccountRequest(new Request(`${origin}/v1/me`, {
      headers: { cookie: `nanocodex_account=${sessionToken}` },
    }), source.env, new URL(`${origin}/v1/me`));
    const portableCookie = localPasskeySetCookie(current!.headers)!.split(";", 1)[0]!;
    const unknown = await routeAccountRequest(new Request(`${origin}/webauthn/login/options`, {
      method: "POST",
      headers: {
        cookie: portableCookie,
        "content-type": "application/json",
        origin,
      },
      body: JSON.stringify({ credentialId: "unknown-saved-passkey" }),
    }), source.env, new URL(`${origin}/webauthn/login/options`));

    expect(unknown?.status).toBe(400);
    expect(await unknown!.json()).toEqual({ error: "unknown credential" });
  });

  it("leaves an untargeted login discoverable even when a portable hint exists", async () => {
    const source = portableEnv();
    const sessionToken = "d".repeat(64);
    source.set("webauthn", `session:${sessionToken}`, {
      credentialId: SECOND_CREDENTIAL_ID,
      publicKey: SECOND_PUBLIC_KEY,
      userId: encodeUserId(SECOND_USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const origin = "http://two.nanocodex.localhost:20736";
    const current = await routeAccountRequest(new Request(`${origin}/v1/me`, {
      headers: { cookie: `nanocodex_account=${sessionToken}` },
    }), source.env, new URL(`${origin}/v1/me`));
    const portableCookie = localPasskeySetCookie(current!.headers)!.split(";", 1)[0]!;
    const chooser = await routeAccountRequest(new Request(`${origin}/webauthn/login/options`, {
      method: "POST",
      headers: {
        cookie: portableCookie,
        "content-type": "application/json",
        origin,
      },
      body: "{}",
    }), source.env, new URL(`${origin}/webauthn/login/options`));
    const body = await chooser!.json<{ options: { publicKey: Record<string, unknown> } }>();

    expect(chooser?.status).toBe(200);
    expect(body.options.publicKey).not.toHaveProperty("allowCredentials");
  });

  it("does not import tampered, mismatched, unsigned, or differently signed records", async () => {
    const source = portableEnv();
    const sessionToken = "c".repeat(64);
    source.set("webauthn", `session:${sessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const migrated = await routeAccountRequest(new Request("http://one.nanocodex.localhost:20735/v1/me", {
      headers: { cookie: `nanocodex_account=${sessionToken}` },
    }), source.env, new URL("http://one.nanocodex.localhost:20735/v1/me"));
    const portableCookie = localPasskeySetCookie(migrated!.headers)!.split(";", 1)[0]!;
    const [name, value] = portableCookie.split("=", 2) as [string, string];
    const [payload, signature] = value.split(".") as [string, string];
    const tamperedSignature = `${signature[0] === "A" ? "B" : "A"}${signature.slice(1)}`;

    for (const testCase of [
      { cookie: `${name}=${payload}.${tamperedSignature}`, id: CREDENTIAL_ID },
      { cookie: portableCookie, id: "different-credential" },
      { cookie: `${name}=${payload}`, id: CREDENTIAL_ID },
    ]) {
      const target = portableEnv();
      await loginWithPortableCookie(target.env, testCase.cookie, testCase.id);
      expect(target.get("webauthn", `credential:${CREDENTIAL_ID}`)).toBeUndefined();
      expect(target.get("webauthn", `credential:${testCase.id}`)).toBeUndefined();
    }

    const differentKey = portableEnv("different-local-hmac-key");
    await loginWithPortableCookie(differentKey.env, portableCookie, CREDENTIAL_ID);
    expect(differentKey.get("webauthn", `credential:${CREDENTIAL_ID}`)).toBeUndefined();
  });

  it("issues the same portable record on the browser-safe localhost fallback", async () => {
    const source = portableEnv();
    const sessionToken = "e".repeat(64);
    source.set("webauthn", `session:${sessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const migrated = await routeAccountRequest(new Request(
      "http://passkey-a.nanocodex.localhost:20735/v1/me",
      { headers: { cookie: `nanocodex_account=${sessionToken}` } },
    ), source.env, new URL("http://passkey-a.nanocodex.localhost:20735/v1/me"));
    const setCookie = localPasskeySetCookie(migrated!.headers);
    expect(setCookie).toBeDefined();
    expect(setCookie).toContain("Domain=nanocodex.localhost");
    expect(setCookie).toContain("Secure");

    const target = portableEnv();
    await loginWithPortableCookie(
      target.env,
      setCookie!.split(";", 1)[0]!,
      CREDENTIAL_ID,
      "http://passkey-b.nanocodex.localhost:20736",
    );
    expect(target.get("webauthn", `credential:${CREDENTIAL_ID}`)).toEqual({
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
    });
  });

  it("lets an exact localhost origin forget only its portable credential hint", async () => {
    const { env } = portableEnv();
    const origin = "http://passkey-a.nanocodex.localhost:20735";
    const response = await routeAccountRequest(new Request(
      `${origin}/webauthn/portable-credential`,
      { method: "DELETE", headers: { origin } },
    ), env, new URL(`${origin}/webauthn/portable-credential`));
    expect(response?.status).toBe(204);
    expect(localPasskeySetCookie(response!.headers)).toContain(
      "nanocodex_local_passkey=; Path=/; Domain=nanocodex.localhost; Max-Age=0",
    );

    const wrongOrigin = await routeAccountRequest(new Request(
      `${origin}/webauthn/portable-credential`,
      { method: "DELETE", headers: { origin: "http://passkey-b.nanocodex.localhost:20736" } },
    ), env, new URL(`${origin}/webauthn/portable-credential`));
    expect(wrongOrigin?.status).toBe(403);
  });

  it("never issues or imports the portable record on production or generic loopback origins", async () => {
    const source = portableEnv();
    const portableSessionToken = "c".repeat(64);
    source.set("webauthn", `session:${portableSessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const migrated = await routeAccountRequest(new Request("http://nanocodex.localhost:5173/v1/me", {
      headers: { cookie: `nanocodex_account=${portableSessionToken}` },
    }), source.env, new URL("http://nanocodex.localhost:5173/v1/me"));
    const portableCookie = localPasskeySetCookie(migrated!.headers)!.split(";", 1)[0]!;

    for (const origin of [
      "https://nanocodex.example",
      "https://localhost",
      "https://127.0.0.1",
      "http://branch.example",
      "http://nanocodex.local",
      "http://nested.branch.nanocodex.localhost:20735",
    ]) {
      const local = portableEnv();
      const sessionToken = "f".repeat(64);
      local.set("webauthn", `session:${sessionToken}`, {
        credentialId: CREDENTIAL_ID,
        publicKey: PUBLIC_KEY,
        userId: encodeUserId(USER_ID),
        issuedAt: 1,
        expiresAt: Math.floor(Date.now() / 1_000) + 60,
      });
      const response = await routeAccountRequest(new Request(`${origin}/v1/me`, {
        headers: { cookie: `nanocodex_account=${sessionToken}` },
      }), local.env, new URL(`${origin}/v1/me`));
      expect(response?.status).toBe(200);
      expect(localPasskeySetCookie(response!.headers)).toBeUndefined();

      await loginWithPortableCookie(local.env, portableCookie, CREDENTIAL_ID, origin);
      expect(local.get("webauthn", `credential:${CREDENTIAL_ID}`)).toBeUndefined();
    }
  });
});

describe("manual credential vault account boundary", () => {
  it("requires a persistent same-origin session and forwards only validated JSON", async () => {
    const local = portableEnv();
    const sessionToken = "9".repeat(64);
    local.set("webauthn", `session:${sessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const seen: Request[] = [];
    const binding = {
      async fetch(input: RequestInfo | URL, init?: RequestInit) {
        const request = new Request(input, init);
        seen.push(request);
        return request.method === "POST"
          ? Response.json({ id: "v".repeat(32), kind: "login", name: "Example", created_at: 1 }, {
              status: 201,
            })
          : new Response(null, { status: 204 });
      },
    } as Fetcher;
    const credentialEnv = { ...local.env, NANOCODEX: binding };
    const origin = "http://nanocodex.localhost:20735";
    const url = new URL("/v1/credentials/vault/login", origin);
    const headers = {
      cookie: `nanocodex_account=${sessionToken}`,
      "content-type": "application/json; charset=utf-8",
      origin,
    };
    const created = await routeCredentialRequest(new Request(url, {
      method: "POST",
      headers,
      body: JSON.stringify({ name: "Example", username: "person", password: "secret" }),
    }), credentialEnv, url);
    expect(created?.status).toBe(201);
    expect(seen).toHaveLength(1);
    expect(seen[0]!.url).toBe(
      `https://broker.internal/users/${USER_ID}/credentials/vault/login`,
    );
    expect(seen[0]!.headers.get("content-type")).toBe("application/json");
    expect(await seen[0]!.json()).toEqual({
      name: "Example",
      username: "person",
      password: "secret",
    });

    const unauthenticated = await routeCredentialRequest(new Request(url, {
      method: "POST",
      headers: { "content-type": "application/json", origin },
      body: JSON.stringify({ name: "Example", username: "person", password: "secret" }),
    }), credentialEnv, url);
    expect(unauthenticated?.status).toBe(401);

    const crossOrigin = await routeCredentialRequest(new Request(url, {
      method: "POST",
      headers: { ...headers, origin: "https://attacker.example" },
      body: JSON.stringify({ name: "Example", username: "person", password: "secret" }),
    }), credentialEnv, url);
    expect(crossOrigin?.status).toBe(403);
    expect(seen).toHaveLength(1);

    const deleteUrl = new URL(`/v1/credentials/vault/login/${"v".repeat(32)}`, origin);
    const removed = await routeCredentialRequest(new Request(deleteUrl, {
      method: "DELETE",
      headers: { cookie: headers.cookie, origin },
    }), credentialEnv, deleteUrl);
    expect(removed?.status).toBe(204);
    expect(seen[1]!.method).toBe("DELETE");
    expect(seen[1]!.body).toBeNull();
  });

  it("rejects invalid content, schemas, and oversized bodies before egress", async () => {
    const local = portableEnv();
    const sessionToken = "8".repeat(64);
    local.set("webauthn", `session:${sessionToken}`, {
      credentialId: CREDENTIAL_ID,
      publicKey: PUBLIC_KEY,
      userId: encodeUserId(USER_ID),
      issuedAt: 1,
      expiresAt: Math.floor(Date.now() / 1_000) + 60,
    });
    const binding = { fetch: vi.fn() } as unknown as Fetcher;
    const credentialEnv = { ...local.env, NANOCODEX: binding };
    const origin = "http://nanocodex.localhost:20735";
    const url = new URL("/v1/credentials/vault/card", origin);
    const baseHeaders = { cookie: `nanocodex_account=${sessionToken}`, origin };

    const wrongType = await routeCredentialRequest(new Request(url, {
      method: "POST",
      headers: baseHeaders,
      body: "{}",
    }), credentialEnv, url);
    expect(wrongType?.status).toBe(415);

    const invalid = await routeCredentialRequest(new Request(url, {
      method: "POST",
      headers: { ...baseHeaders, "content-type": "application/json" },
      body: JSON.stringify({
        name: "Card",
        card_number: "4111111111111111",
        expiry_month: 9,
        expiry_year: "2031",
        cvv: "123",
        billing_zip: "10001",
      }),
    }), credentialEnv, url);
    expect(invalid?.status).toBe(400);

    const oversized = await routeCredentialRequest(new Request(
      new URL("/v1/credentials/vault/login", origin),
      {
        method: "POST",
        headers: { ...baseHeaders, "content-type": "application/json" },
        body: JSON.stringify({
          name: "Example",
          username: "person",
          password: "x".repeat(13 * 1024),
        }),
      },
    ), credentialEnv, new URL("/v1/credentials/vault/login", origin));
    expect(oversized?.status).toBe(413);
    expect(binding.fetch).not.toHaveBeenCalled();
  });
});

function accountEnv(fetch: (request: Request) => Promise<Response>): AccountAuthEnv {
  return {
    NANOCODEX_USERS: {
      getByName() {
        return {
          fetch(input: RequestInfo | URL, init?: RequestInit) {
            return fetch(new Request(input, init));
          },
        };
      },
    },
  } as unknown as AccountAuthEnv;
}

function connectHeaders(overrides: Readonly<{
  appToolCatalogDigest?: string;
  capabilities?: readonly string[];
  connectors?: readonly string[];
  connectorConnections?: Readonly<Record<string, readonly string[]>>;
  mcpIds?: readonly string[];
}> = {}): Headers {
  return new Headers({
    "x-nanocodex-connect-user": USER_ID,
    "x-nanocodex-connect-grant-id": CONNECT_GRANT_ID,
    "x-nanocodex-connect-capabilities": JSON.stringify(
      overrides.capabilities ?? ["agents:read", "agents:write", "tools:use"],
    ),
    "x-nanocodex-connect-connectors": JSON.stringify(overrides.connectors ?? []),
    "x-nanocodex-connect-mcp-ids": JSON.stringify(overrides.mcpIds ?? []),
    ...(overrides.appToolCatalogDigest === undefined
      ? {}
      : {
        "x-nanocodex-connect-app-tool-catalog-digest": overrides.appToolCatalogDigest,
      }),
    ...(overrides.connectorConnections === undefined
      ? {}
      : {
        "x-nanocodex-connect-connector-connections": JSON.stringify(
          overrides.connectorConnections,
        ),
      }),
  });
}

function localPasskeySetCookie(headers: Headers): string | undefined {
  return headers.getSetCookie().find((cookie) => cookie.startsWith(`${LOCAL_PASSKEY_COOKIE}=`));
}

function portableEnv(secret = LOCAL_HMAC_KEY): {
  env: AccountAuthEnv;
  get(name: string, key: string): unknown;
  set(name: string, key: string, value: unknown): void;
  values(name: string): IterableIterator<unknown>;
  deleteMatching(name: string, prefix: string): void;
} {
  const stores = new Map<string, Map<string, unknown>>();
  const store = (name: string) => {
    let current = stores.get(name);
    if (!current) {
      current = new Map();
      stores.set(name, current);
    }
    return current;
  };
  const auth = {
    idFromName(name: string) {
      return name;
    },
    get(id: string) {
      return {
        async fetch(input: RequestInfo | URL, init?: RequestInit) {
          const request = new Request(input, init);
          const url = new URL(request.url);
          const key = url.searchParams.get("key")!;
          const current = store(id);
          if (url.pathname === "/get") return Response.json({ value: current.get(key) });
          if (url.pathname === "/set") {
            const body = await request.json<{ value: unknown }>();
            current.set(key, body.value);
            return Response.json({ ok: true });
          }
          if (url.pathname === "/create") {
            const body = await request.json<{ value: unknown }>();
            const created = !current.has(key);
            if (created) current.set(key, body.value);
            return Response.json({ created });
          }
          if (url.pathname === "/delete") {
            current.delete(key);
            return Response.json({ ok: true });
          }
          if (url.pathname === "/take") {
            const value = current.get(key);
            current.delete(key);
            return Response.json({ value });
          }
          return new Response(null, { status: 404 });
        },
      };
    },
  } as unknown as DurableObjectNamespace;
  const users = {
    getByName(userId: string) {
      return {
        async fetch(input: RequestInfo | URL, init?: RequestInit) {
          const request = new Request(input, init);
          if (new URL(request.url).pathname !== "/account") {
            return new Response(null, { status: 404 });
          }
          if (request.method === "PUT") {
            const body = await request.json<{ persistent: boolean }>();
            return Response.json(account(userId, body.persistent));
          }
          return Response.json(account(userId, true));
        },
      };
    },
  } as unknown as DurableObjectNamespace;
  const organizations = {
    getByName() {
      return {
        fetch() {
          return Promise.resolve(Response.json({
            organizationId: ORGANIZATION_ID,
            teamId: TEAM_ID,
            role: "owner",
            authorizationEpoch: 1,
            capabilities: [
              "agents:read",
              "agents:write",
              "api_keys:read",
              "api_keys:write",
              "history:read",
              "memory:read",
              "memory:write",
              "tools:use",
              "organization:read",
              "organization:write",
            ],
          }));
        },
      };
    },
  } as unknown as DurableObjectNamespace;
  return {
    env: {
      NANOCODEX_AUTH: auth,
      NANOCODEX: {
        async fetch(input: RequestInfo | URL, init?: RequestInit) {
          const request = new Request(input, init);
          const match = new URL(request.url).pathname.match(
            /^\/users\/([0-9a-f-]+)\/wallet(?:\/(connect|revoke-access-key))?$/,
          );
          if (!match) return new Response(null, { status: 404 });
          return Response.json({
            address: "0x1111111111111111111111111111111111111111",
            created_at: 1,
          });
        },
      } as Fetcher,
      NANOCODEX_LOCAL_WEBAUTHN_HMAC_KEY: secret,
      NANOCODEX_ORGANIZATIONS: organizations,
      NANOCODEX_USERS: users,
    } as unknown as AccountAuthEnv,
    get: (name, key) => store(name).get(key),
    set: (name, key, value) => store(name).set(key, value),
    values: (name) => store(name).values(),
    deleteMatching: (name, prefix) => {
      for (const key of store(name).keys()) if (key.startsWith(prefix)) store(name).delete(key);
    },
  };
}

function loginWithPortableCookie(
  env: AccountAuthEnv,
  cookie: string,
  credentialId: string,
  origin = "http://two.nanocodex.localhost:20736",
): Promise<Response> {
  const url = new URL("/webauthn/login", origin);
  return routeAccountRequest(new Request(url, {
    method: "POST",
    headers: {
      cookie,
      "content-type": "application/json",
      origin: url.origin,
    },
    body: JSON.stringify({
      id: credentialId,
      metadata: { clientDataJSON: JSON.stringify({ challenge: "AQ" }) },
    }),
  }), env, url) as Promise<Response>;
}

function persistentAccountCookie(
  local: ReturnType<typeof portableEnv>,
  userId: string,
  character: string,
): string {
  const token = character.repeat(64);
  local.set("webauthn", `session:${token}`, {
    credentialId: CREDENTIAL_ID,
    publicKey: PUBLIC_KEY,
    userId: encodeUserId(userId),
    issuedAt: 1,
    expiresAt: Math.floor(Date.now() / 1_000) + 60,
  });
  return `nanocodex_account=${token}`;
}

type TwilioVerifyCall = Readonly<{
  authorization: string | null;
  form: Record<string, string>;
  url: string;
}>;

function mockTwilioVerify(env: AccountAuthEnv) {
  env.TWILIO_ACCOUNT_SID = TWILIO_ACCOUNT_SID;
  env.TWILIO_API_KEY_SID = TWILIO_API_KEY_SID;
  env.TWILIO_API_KEY_SECRET = TWILIO_API_KEY_SECRET;
  env.TWILIO_VERIFY_SERVICE_SID = TWILIO_VERIFY_SERVICE_SID;
  const state = {
    calls: [] as TwilioVerifyCall[],
    checkHttpStatus: 200,
    checkStatus: "approved",
    startHttpStatus: 201,
    verificationCount: 0,
  };
  vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const request = new Request(input, init);
    const form = Object.fromEntries([...await request.formData()]
      .map(([key, value]) => [key, String(value)]));
    state.calls.push({
      authorization: request.headers.get("authorization"),
      form,
      url: request.url,
    });
    if (request.url.endsWith("/Verifications")) {
      state.verificationCount += 1;
      return Response.json({
        sid: `VE${state.verificationCount.toString(16).padStart(32, "0")}`,
        status: "pending",
      }, { status: state.startHttpStatus });
    }
    return Response.json({ status: state.checkStatus }, { status: state.checkHttpStatus });
  }));
  return state;
}

async function beginSmsOtp(
  env: AccountAuthEnv,
  origin: string,
  phone: string,
  ip = "198.51.100.1",
  cookie?: string,
): Promise<{ challengeId: string; response: Response }> {
  const url = new URL("/v1/auth/sms/start", origin);
  const response = await routeAccountRequest(new Request(url, {
    method: "POST",
    headers: {
      "cf-connecting-ip": ip,
      "content-type": "application/json",
      origin,
      ...(cookie ? { cookie } : {}),
    },
    body: JSON.stringify({ phone }),
  }), env, url) as Response;
  const body: { challenge_id?: string } = await response.clone()
    .json<{ challenge_id?: string }>()
    .catch(() => ({}));
  return { challengeId: body.challenge_id ?? "", response };
}

function completeSmsOtp(
  env: AccountAuthEnv,
  origin: string,
  phone: string,
  challengeId: string,
  code: string,
  cookie?: string,
): Promise<Response> {
  const url = new URL("/v1/auth/sms/verify", origin);
  return routeAccountRequest(new Request(url, {
    method: "POST",
    headers: { "content-type": "application/json", origin, ...(cookie ? { cookie } : {}) },
    body: JSON.stringify({ challenge_id: challengeId, code, phone }),
  }), env, url) as Promise<Response>;
}

function encodeUserId(value: string): string {
  return btoa(value).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function decodeBase64Url(value: string): string {
  const base64 = value.replaceAll("-", "+").replaceAll("_", "/");
  return atob(base64.padEnd(Math.ceil(base64.length / 4) * 4, "="));
}

function account(id: string, persistent: boolean) {
  return {
    id,
    organizationId: ORGANIZATION_ID,
    persistent,
    createdAt: 1,
    lastAuthenticatedAt: 1,
  };
}
