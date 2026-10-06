import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
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

test("SMS OTP recovers delivery and provisioning failures with durable code-bound one-use approval", { timeout: 120_000 }, async t => {
  const output = new URL("../../../output/sms-otp/", import.meta.url);
  const trace = [];
  const phone = "+12025550161", code = "654321";
  const address = "0x" + "1".repeat(40);
  const serviceSid = "VA" + "1".repeat(32);
  let failDelivery = false, failWallet = false, sends = 0, checks = 0, provisions = 0, checkStatus = 200;
  let sendGate, checkGate, walletGate;
  const gates = [];
  function gate() {
    let entered, release;
    const observed = new Promise(resolve => { entered = resolve; });
    const blocked = new Promise(resolve => { release = resolve; });
    const value = { observed, release, wait: async () => { entered(); await blocked; } };
    gates.push(value);
    return value;
  }
  const persistence = await mkdtemp(join(tmpdir(), "nanocodex-sms-journey-"));
  const verificationSids = new Set();
  const bundled = await build({
    stdin: { contents: source, resolveDir: fileURLToPath(new URL("..", import.meta.url)) },
    bundle: true, write: false, format: "esm", target: "es2022", platform: "browser",
    external: ["cloudflare:workers", "node:*"],
    alias: { "node-rsa": "./node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs" },
  });
  const options = {
    durableObjectsPersist: persistence,
    script: bundled.outputFiles[0].text, modules: true,
    compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"],
    bindings: {
      ENVIRONMENT: "test", NANOCODEX_OTP_HMAC_KEY: "synthetic-sms-otp-journey-hmac-key",
      TWILIO_ACCOUNT_SID: "AC" + "1".repeat(32), TWILIO_AUTH_TOKEN: "synthetic-twilio-token",
      TWILIO_VERIFY_SERVICE_SID: serviceSid,
    },
    serviceBindings: { NANOCODEX: async request => {
      assert.equal(request.method, "PUT");
      provisions++;
      const held = walletGate; walletGate = undefined;
      if (held) await held.wait();
      trace.push({ provider: "wallet provision", observed: failWallet ? 503 : 200 });
      return failWallet
        ? Response.json({ error: "synthetic wallet failure" }, { status: 503 })
        : Response.json({ address, created_at: 1 });
    } },
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
      const credential = options.bindings.TWILIO_API_KEY_SID
        ? `${options.bindings.TWILIO_API_KEY_SID}:${options.bindings.TWILIO_API_KEY_SECRET}`
        : `${options.bindings.TWILIO_ACCOUNT_SID}:${options.bindings.TWILIO_AUTH_TOKEN}`;
      assert.equal(request.headers.get("authorization"), `Basic ${Buffer.from(credential).toString("base64")}`);
      const form = new URLSearchParams(await request.text());
      if (url.pathname === `/v2/Services/${serviceSid}/Verifications`) {
        sends++;
        assert.equal(form.get("Channel"), "sms");
        assert.match(form.get("To"), /^\+1202555\d{4}$/);
        const failed = failDelivery;
        const held = sendGate; sendGate = undefined;
        if (held) await held.wait();
        trace.push({ provider: "Twilio send", observed: failed ? 503 : 201 });
        if (failed) return Response.json({ error: "synthetic delivery failure" }, { status: 503 });
        const sid = "VE" + sends.toString(16).padStart(32, "0");
        verificationSids.add(sid);
        return Response.json({ sid, status: "pending" }, { status: 201 });
      }
      assert.equal(url.pathname, `/v2/Services/${serviceSid}/VerificationCheck`);
      checks++;
      const held = checkGate; checkGate = undefined;
      if (held) await held.wait();
      if (checkStatus !== 200) return Response.json({ error: "synthetic verification failure" }, { status: checkStatus });
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
  };
  let mf = new Miniflare(options);
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
    const value = response.status === 204 ? {} : await response.json();
    trace.push({ label, path, expected, observed: response.status,
      result: { error: value.error, challenge_issued: typeof value.challenge_id === "string",
        resend_after: value.resend_after, expires_in: value.expires_in,
        persistent: value.user?.persistent, authentication: value.authentication },
      set_cookie_header: response.headers.has("set-cookie"),
    });
    assert.equal(response.status, expected, `${label}: ${value.error ?? "unexpected status"}`);
    if (expected >= 400 && path.startsWith("/v1/auth/sms/")) assert.equal(response.headers.has("set-cookie"), false);
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
    await verify("wrong phone cannot consume another phone's challenge", latest, {
      body: { phone: "+12025550162", challenge_id: latest, code }, expected: 400,
    });
    checkStatus = 404;
    await verify("missing provider verification fails closed", latest, { expected: 400 });
    checkStatus = 503;
    assert.equal((await verify("provider verification failure", latest, { expected: 503 })).value.error, "sms_verification_failed");
    checkStatus = 200;
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
    const stillValid = await start("challenge before failed resend");
    failDelivery = true;
    await start("failed resend preserves previously delivered code", { expected: 503 });
    failDelivery = false;
    await verify("previously delivered code succeeds after failed resend", stillValid.value.challenge_id);

    const failureGate = sendGate = gate();
    failDelivery = true;
    const delayedFailure = start("delayed failed resend", { expected: 503 });
    await failureGate.observed;
    failDelivery = false;
    const newDuringFailure = await start("new challenge while failed send is pending");
    failureGate.release();
    await delayedFailure;
    await verify("failed send cannot erase a newer challenge", newDuringFailure.value.challenge_id);

    const provisionRecovery = await start("challenge for provisioning recovery");
    failWallet = true;
    const checksBeforeRecovery = checks;
    assert.equal((await verify("approved code with wallet unavailable", provisionRecovery.value.challenge_id, { expected: 503 })).value.error, "wallet_unavailable");
    assert.equal(checks, checksBeforeRecovery + 1);
    const provisionsBeforeWrong = provisions;
    await verify("wrong code cannot reuse durable approval", provisionRecovery.value.challenge_id, {
      body: { phone, challenge_id: provisionRecovery.value.challenge_id, code: "000000" }, expected: 400,
    });
    assert.equal(provisions, provisionsBeforeWrong, "wrong code never reaches wallet provisioning");
    // Restart the actual worker and DOs over persisted SQLite, without modifying
    // auth records or bypassing the public HTTP route.
    await mf.dispose();
    mf = new Miniflare(options);
    base = await mf.ready;
    failWallet = false;
    const recoveryGate = walletGate = gate();
    const recoveryVerify = verify("same approved code succeeds after worker restart", provisionRecovery.value.challenge_id);
    await recoveryGate.observed;
    await verify("concurrent approved-code retry is fenced", provisionRecovery.value.challenge_id, { expected: 400 });
    recoveryGate.release();
    const retry = await recoveryVerify;
    assert.equal(retry.value.user.id, login.value.user.id);
    assert.equal(checks, checksBeforeRecovery + 1, "approved SID is never checked twice");
    await verify("recovered approval cannot be replayed", provisionRecovery.value.challenge_id, { expected: 400 });

    const recoveredCookie = retry.headers.get("set-cookie").split(";")[0];
    await http("SMS account logout", "/v1/auth/logout", { body: {}, cookie: recoveredCookie, expected: 204 });
    const loggedOut = await http("revoked SMS session requires reauthentication", "/v1/me", { cookie: recoveredCookie, expected: 401 });
    assert.equal(loggedOut.value.error, "reauthentication_required");

    const concurrent = await start("challenge for concurrent verification");
    const verificationGate = checkGate = gate();
    const winningVerify = verify("first concurrent verifier", concurrent.value.challenge_id);
    await verificationGate.observed;
    await verify("second concurrent verifier is fenced", concurrent.value.challenge_id, { expected: 400 });
    verificationGate.release();
    await winningVerify;
    await verify("concurrent winner cannot be replayed", concurrent.value.challenge_id, { expected: 400 });

    const superseded = await start("challenge to supersede during provisioning");
    const provisioningGate = walletGate = gate();
    const staleVerify = verify("superseded in-flight verifier cannot issue a session", superseded.value.challenge_id, { expected: 400 });
    await provisioningGate.observed;
    const superseding = await start("resend during approved-code provisioning");
    provisioningGate.release();
    await staleVerify;
    await verify("new challenge survives old verifier completion", superseding.value.challenge_id);

    const anonymous = await http("anonymous account before SMS", "/v1/me");
    const anonymousCookie = anonymous.headers.get("set-cookie").split(";")[0];
    const freshPhone = "+12025550999";
    const promote = await start("anonymous phone promotion", { body: { phone: freshPhone }, cookie: anonymousCookie });
    failWallet = true;
    await verify("anonymous promotion provisioning failure", promote.value.challenge_id, {
      body: { phone: freshPhone, challenge_id: promote.value.challenge_id, code }, cookie: anonymousCookie, expected: 503,
    });
    failWallet = false;
    const promoted = await verify("anonymous promotion recovery retains identity", promote.value.challenge_id, {
      body: { phone: freshPhone, challenge_id: promote.value.challenge_id, code }, cookie: anonymousCookie,
    });
    assert.equal(promoted.value.user.id, anonymous.value.user.id);
    const persistentCookie = promoted.headers.get("set-cookie").split(";")[0];
    const separatePhone = "+12025550998";
    const separate = await start("persistent login with a different phone", { body: { phone: separatePhone }, cookie: persistentCookie });
    const separated = await verify("different phone does not alias persistent account", separate.value.challenge_id, {
      body: { phone: separatePhone, challenge_id: separate.value.challenge_id, code }, cookie: persistentCookie,
    });
    assert.notEqual(separated.value.user.id, promoted.value.user.id);
    await start("oversized phone rejected", { body: { phone: "+1" + "2".repeat(1100) }, expected: 413 });

    // Exercise both supported provider credential configurations and ensure the
    // development-only fixed-code switch cannot bypass production verification.
    await mf.dispose();
    options.bindings.TWILIO_API_KEY_SID = "SK" + "2".repeat(32);
    options.bindings.TWILIO_API_KEY_SECRET = "synthetic-twilio-api-key";
    options.bindings.NANOCODEX_MOCK_TWILIO_VERIFY_CODE = "111111";
    options.bindings.ENVIRONMENT = "production";
    mf = new Miniflare(options); base = await mf.ready;
    const apiKey = await start("API-key delivery with normalized phone", { body: { phone: "+1 (202) 555-0161" } });
    await verify("production ignores development fixed code", apiKey.value.challenge_id, {
      body: { phone, challenge_id: apiKey.value.challenge_id, code: "111111" }, expected: 400,
    });
    await verify("API-key provider verification", apiKey.value.challenge_id);
    await mf.dispose();
    options.bindings.ENVIRONMENT = "development";
    mf = new Miniflare(options); base = await mf.ready;
    const checksBeforeDevelopment = checks, sendsBeforeDevelopment = sends;
    const development = await start("explicit development fixed code start");
    await verify("development wrong code", development.value.challenge_id, { expected: 400 });
    await verify("explicit development fixed code login", development.value.challenge_id, {
      body: { phone, challenge_id: development.value.challenge_id, code: "111111" },
    });
    assert.equal(checks, checksBeforeDevelopment);
    assert.equal(sends, sendsBeforeDevelopment);
    await mf.dispose();
    options.bindings.ENVIRONMENT = "production";
    delete options.bindings.TWILIO_VERIFY_SERVICE_SID;
    mf = new Miniflare(options); base = await mf.ready;
    await start("unconfigured production delivery fails closed", { expected: 503 });
    trace.push({ assertion: "Failed resends preserve old/new challenges; durable exact-code approval survives wallet failure and worker restart; wrong code, replay and concurrent/superseded verifies cannot issue sessions; anonymous identity preserved", passed: true });
    passed = true;
    t.diagnostic(`${trace.length} redacted HTTP/provider observations saved to output/sms-otp/http-trace.json`);
  } finally {
    await mkdir(output, { recursive: true });
    await writeFile(new URL("http-trace.json", output), JSON.stringify({
      command: "node --test js/managed/test/sms-otp-journey.test.mjs", passed,
      inputs: "Synthetic phones, fixed test IPs, synthetic correct/wrong one-time codes; credentials, challenge IDs, user IDs and cookies omitted",
      expected: "No resend throttle; failed resend preserves delivered challenge; approved code survives wallet failure/restart without rechecking deleted SID; exact-code binding, original identity, origin, concurrency, supersession and one-use checks hold",
      trace,
    }, null, 2));
    for (const held of gates) held.release();
    await mf.dispose();
    await rm(persistence, { recursive: true, force: true });
  }
});
