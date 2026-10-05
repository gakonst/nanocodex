import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile } from "node:fs/promises";
import { build } from "esbuild";
import { Miniflare } from "miniflare";

// Real HTTP, account routes, sessions and SQLite Durable Objects in workerd.
// Only Twilio Verify and wallet provisioning are external service fixtures.
const source = `
import { routeAccountRequest } from "./src/account-auth.ts";
export { UserAccount, Organization, ApiKeyRecord, NonceStorage } from "./src/account-auth.ts";
export default { async fetch(request, env) {
  return await routeAccountRequest(request, env, new URL(request.url))
    ?? new Response(null, { status: 404 });
}};
`;

test("SMS OTP accepts immediate resends without local phone/IP limits and preserves login checks", { timeout: 120_000 }, async t => {
  const output = new URL("../../../output/sms-otp/", import.meta.url);
  const trace = [];
  const phone = "+12025550161", code = "654321";
  const address = "0x" + "1".repeat(40);
  const serviceSid = "VA" + "1".repeat(32);
  let failDelivery = false, sends = 0;
  const verificationSids = new Set();
  const bundled = await build({
    stdin: { contents: source, resolveDir: fileURLToPath(new URL("..", import.meta.url)) },
    bundle: true, write: false, format: "esm", target: "es2022", platform: "browser",
    external: ["cloudflare:workers", "node:*"],
    alias: { "node-rsa": "./node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs" },
  });
  const mf = new Miniflare({
    script: bundled.outputFiles[0].text, modules: true,
    compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"],
    bindings: {
      ENVIRONMENT: "test", NANOCODEX_OTP_HMAC_KEY: "synthetic-sms-otp-journey-hmac-key",
      TWILIO_ACCOUNT_SID: "AC" + "1".repeat(32), TWILIO_AUTH_TOKEN: "synthetic-twilio-token",
      TWILIO_VERIFY_SERVICE_SID: serviceSid,
    },
    serviceBindings: { NANOCODEX: async () => Response.json({ address, created_at: 1 }) },
    durableObjects: {
      NANOCODEX_AUTH: { className: "NonceStorage", useSQLite: true },
      NANOCODEX_USERS: { className: "UserAccount", useSQLite: true },
      NANOCODEX_ORGANIZATIONS: { className: "Organization", useSQLite: true },
      NANOCODEX_API_KEYS: { className: "ApiKeyRecord", useSQLite: true },
    },
    outboundService: async request => {
      const url = new URL(request.url);
      assert.equal(url.origin, "https://verify.twilio.com", "no live external services allowed");
      assert.equal(request.method, "POST");
      const form = new URLSearchParams(await request.text());
      if (url.pathname === `/v2/Services/${serviceSid}/Verifications`) {
        sends++;
        assert.equal(form.get("Channel"), "sms");
        assert.match(form.get("To"), /^\+1202555\d{4}$/);
        trace.push({ provider: "Twilio send", observed: failDelivery ? 503 : 201 });
        if (failDelivery) return Response.json({ error: "synthetic delivery failure" }, { status: 503 });
        const sid = "VE" + sends.toString(16).padStart(32, "0");
        verificationSids.add(sid);
        return Response.json({ sid, status: "pending" }, { status: 201 });
      }
      assert.equal(url.pathname, `/v2/Services/${serviceSid}/VerificationCheck`);
      const sid = form.get("VerificationSid");
      if (!verificationSids.has(sid)) {
        trace.push({ provider: "Twilio check", observed: 404 });
        return Response.json({ error: "verification not found" }, { status: 404 });
      }
      const approved = form.get("Code") === code;
      // Twilio deletes approved verifications; retries cannot approve this SID again.
      if (approved) verificationSids.delete(sid);
      trace.push({ provider: "Twilio check", observed: 200, approved });
      return Response.json({ status: approved ? "approved" : "pending" });
    },
  });
  let base;
  async function http(label, path, { body, cookie, origin = "same", ip = "192.0.2.61", expected = 200 } = {}) {
    const response = await fetch(new URL(path, base), {
      method: body === undefined ? "GET" : "POST",
      headers: {
        origin: origin === "same" ? new URL(base).origin : origin,
        "cf-connecting-ip": ip,
        ...(body === undefined ? {} : { "content-type": "application/json" }),
        ...(cookie ? { cookie } : {}),
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    const value = await response.json();
    trace.push({ label, path, expected, observed: response.status,
      result: { error: value.error, challenge_issued: typeof value.challenge_id === "string",
        resend_after: value.resend_after, expires_in: value.expires_in,
        persistent: value.user?.persistent, authentication: value.authentication },
      session_cookie_issued: response.headers.has("set-cookie"),
    });
    assert.equal(response.status, expected, `${label}: ${value.error ?? "unexpected status"}`);
    if (expected !== 200) assert.equal(response.headers.has("set-cookie"), false);
    return { value, headers: response.headers };
  }
  const start = (label, options = {}) => http(label, "/v1/auth/sms/start", { body: { phone }, expected: 202, ...options });
  const verify = (label, challenge, options = {}) => http(label, "/v1/auth/sms/verify", {
    body: { phone, challenge_id: challenge, code }, ...options,
  });
  let passed = false;
  try {
    base = await mf.ready;
    const challenges = [];
    for (let i = 0; i < 21; i++) {
      const { value, headers } = await start(`same phone/IP immediate start ${i + 1}`);
      assert.equal(value.resend_after, 0);
      assert.equal(headers.has("retry-after"), false);
      assert.ok(value.expires_in > 0);
      assert.match(value.challenge_id, /^[A-Za-z0-9_-]{43}$/);
      challenges.push(value.challenge_id);
    }
    assert.equal(new Set(challenges).size, 21, "each resend issues a distinct challenge");
    // A separate IP and distinct phones independently exercise the former IP cap.
    for (let i = 0; i < 21; i++) {
      const { value } = await start(`distinct phone shared IP start ${i + 1}`, {
        body: { phone: `+1202555${String(200 + i).padStart(4, "0")}` }, ip: "192.0.2.62",
      });
      assert.equal(value.resend_after, 0);
    }
    trace.push({ assertion: "21 immediate same-phone/same-IP starts and 21 distinct-phone/shared-IP starts accepted; challenges unique", passed: true });
    assert.equal(sends, 42, "every accepted start reaches the provider");
    const latest = challenges.at(-1);
    assert.equal((await verify("superseded challenge rejected", challenges[0], { expected: 400 })).value.error, "invalid_or_expired_otp");
    assert.equal((await start("cross-origin start", { origin: "https://foreign.example", expected: 403 })).value.error, "forbidden_origin");
    assert.equal((await verify("cross-origin verify", latest, { origin: "https://foreign.example", expected: 403 })).value.error, "forbidden_origin");
    assert.equal((await verify("wrong code", latest, {
      body: { phone, challenge_id: latest, code: "000000" }, expected: 400,
    })).value.error, "invalid_or_expired_otp");
    const login = await verify("correct-code retry", latest);
    assert.equal(login.value.user.persistent, true);
    assert.equal(login.value.user.address, address);
    const setCookie = login.headers.get("set-cookie");
    assert.ok(setCookie?.includes("HttpOnly"));
    const cookie = setCookie.split(";")[0];
    const me = await http("persistent session", "/v1/me", { cookie });
    assert.equal(me.value.user.id, login.value.user.id);
    assert.equal(me.value.user.persistent, true);
    assert.equal(me.value.authentication, "account_session");
    assert.equal((await verify("consumed challenge rejected", latest, { expected: 400 })).value.error, "invalid_or_expired_otp");
    failDelivery = true;
    assert.equal((await start("provider send failure", { expected: 503 })).value.error, "sms_delivery_failed");
    failDelivery = false;
    const recovery = await start("immediate recovery after provider failure");
    assert.equal(recovery.value.resend_after, 0);
    assert.ok(!challenges.includes(recovery.value.challenge_id));
    const recoveredLogin = await verify("recovered challenge login", recovery.value.challenge_id);
    assert.equal(recoveredLogin.value.user.id, login.value.user.id, "same phone retains its account");
    passed = true;
    t.diagnostic(`${trace.length} redacted HTTP/provider observations saved to output/sms-otp/http-trace.json`);
  } finally {
    await mkdir(output, { recursive: true });
    await writeFile(new URL("http-trace.json", output), JSON.stringify({
      command: "node --test js/managed/test/sms-otp-journey.test.mjs", passed,
      inputs: "Synthetic phones, fixed test IPs, synthetic correct/wrong OTPs; credentials, challenge IDs, user IDs and cookies omitted",
      expected: "Immediate starts exceed former cooldown, 5/phone and 20/IP caps; resend_after=0; latest challenge permits wrong-code retry and one login; foreign origins fail; provider failure permits immediate recovery",
      trace,
    }, null, 2));
    await mf.dispose();
  }
});
