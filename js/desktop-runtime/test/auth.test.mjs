import assert from "node:assert/strict";
import { once } from "node:events";
import { createServer } from "node:http";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { SmsSignIn } from "../src/auth.mjs";

const keyId = "a".repeat(12);
const key = `ncx_live_${keyId}_${"b".repeat(43)}`;
const cookie = `nanocodex_account=s_${"c".repeat(43)}`;
const challenge = "d".repeat(43);
const deferred = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };

async function service(t, handler) {
  const requests = [];
  const server = createServer(async (request, response) => {
    let encoded = ""; for await (const chunk of request) encoded += chunk;
    const recorded = { path: request.url, method: request.method, headers: request.headers, body: encoded ? JSON.parse(encoded) : undefined };
    requests.push(recorded);
    response.setHeader("content-type", "application/json");
    const send = (status, body = {}, headers = {}) => { response.writeHead(status, headers); response.end(JSON.stringify(body)); };
    try { await handler(recorded, send); }
    catch (error) { send(500, { error: "fixture_failed" }); throw error; }
  });
  server.listen(0, "127.0.0.1"); await once(server, "listening");
  t.after(() => new Promise(resolve => { server.closeAllConnections(); server.close(resolve); }));
  return { baseUrl: `http://127.0.0.1:${server.address().port}`, requests };
}

function ordinary(request, send) {
  if (request.path === "/v1/auth/sms/start") send(202, { challenge_id: challenge, expires_in: 600, resend_after: 60 });
  else if (request.path === "/v1/auth/sms/verify") send(200, { user: { persistent: true } }, { "set-cookie": `${cookie}; Path=/; HttpOnly; SameSite=Lax` });
  else if (request.path === "/v1/api-keys") send(201, { api_key: key, key: { id: keyId } });
  else send(204);
}

test("SMS HTTP boundary keeps session private and retries minting without consuming the OTP twice", async t => {
  let mintAttempts = 0;
  const { baseUrl, requests } = await service(t, (request, send) => {
    if (request.path.endsWith("/verify") && request.body.code === "000000") send(400, { error: "invalid_or_expired_otp", message: cookie });
    else if (request.path === "/v1/api-keys" && ++mintAttempts === 1) send(503, { error: "unavailable", message: key });
    else if (request.path === "/v1/auth/logout") send(503, { error: "unavailable" });
    else ordinary(request, send);
  });
  const signIn = new SmsSignIn({ baseUrl });
  const started = await signIn.start({ phone: "+1 (415) 555-0123" });
  assert.deepEqual(Object.keys(started).sort(), ["expiresAt", "phone", "resendAt"]);
  assert.equal(started.phone, "+14155550123");
  assert.ok(started.expiresAt > Date.now());
  assert.equal(JSON.stringify(signIn), "{}");
  await assert.rejects(signIn.verify({ code: "000000" }), error => error.code === "invalid_or_expired_otp" && !error.message.includes(cookie));
  await assert.rejects(signIn.verify({ code: "123456" }), error => !error.message.includes(key));
  const [first, duplicate] = await Promise.all([signIn.verify({ code: "ignored after verification" }), signIn.verify({})]);
  assert.deepEqual(first, { baseUrl, apiKey: key });
  assert.deepEqual(first, duplicate);
  assert.equal(requests.filter(r => r.path.endsWith("/verify")).length, 2);
  assert.equal(mintAttempts, 2);
  for (const request of requests) {
    assert.equal(request.headers.origin, baseUrl);
    assert.equal(request.headers.authorization, undefined);
    if (request.path === "/v1/api-keys") {
      assert.equal(request.headers.cookie, cookie);
      assert.match(request.body.label, /^Nanocodex on /);
    } else assert.equal(request.headers.cookie, undefined);
  }
  await signIn.complete();
  await signIn.cancel();
  assert.equal(requests.filter(r => r.method === "DELETE").length, 0);
  assert.equal(requests.at(-1).path, "/v1/auth/logout");
  assert.equal(requests.at(-1).headers.cookie, cookie);
  await assert.rejects(signIn.verify({ code: "123456" }), /Request a text message/);
});

test("cancelling while a key is minted revokes that exact key before discarding the session", async t => {
  const minting = deferred(), release = deferred();
  const { baseUrl, requests } = await service(t, async (request, send) => {
    if (request.path === "/v1/api-keys") { minting.resolve(); await release.promise; }
    ordinary(request, send);
  });
  const signIn = new SmsSignIn({ baseUrl });
  await signIn.start({ phone: "+14155550123" });
  const verifying = signIn.verify({ code: "123456" });
  const rejected = assert.rejects(verifying, /cancelled/);
  await minting.promise;
  const cancelled = signIn.cancel();
  release.resolve();
  await Promise.all([rejected, cancelled]);
  assert.deepEqual(requests.slice(-2).map(r => [r.method, r.path]), [["DELETE", `/v1/api-keys/${keyId}`], ["POST", "/v1/auth/logout"]]);
  assert.equal(requests.at(-2).headers.cookie, cookie);
});

test("failed cancellation retains only the owned key for a safe revocation retry", async t => {
  let deletions = 0;
  const { baseUrl, requests } = await service(t, (request, send) => {
    if (request.method === "DELETE" && ++deletions === 1) send(503, { error: "unavailable" });
    else if (request.method === "DELETE") send(404, { error: "not_found" });
    else ordinary(request, send);
  });
  const signIn = new SmsSignIn({ baseUrl });
  await signIn.start({ phone: "+14155550123" });
  await signIn.verify({ code: "123456" });
  await assert.rejects(signIn.cancel());
  await signIn.cancel();
  assert.deepEqual(requests.filter(r => r.method === "DELETE").map(r => r.path), [`/v1/api-keys/${keyId}`, `/v1/api-keys/${keyId}`]);
  assert.equal(requests.at(-1).path, "/v1/auth/logout");
});

test("phone validation, resend cooldown, rate limits, and expiry have safe errors", async t => {
  const { baseUrl, requests } = await service(t, ordinary);
  const signIn = new SmsSignIn({ baseUrl });
  await assert.rejects(signIn.start({ phone: "4155550123" }), /country code/);
  const started = await signIn.start({ phone: "+14155550123" });
  await assert.rejects(signIn.start({ phone: "+14155550123" }), error => error.code === "rate_limited" && error.retryAt >= started.resendAt);
  assert.equal(requests.length, 1);
  t.mock.timers.enable({ apis: ["Date"], now: started.expiresAt + 1 });
  await assert.rejects(signIn.verify({ code: "123456" }), error => error.code === "expired");
  t.mock.timers.reset();
  await signIn.cancel();
  const limited = await service(t, (_request, send) => send(429, { error: "rate_limited", retry_after: 95 }));
  await assert.rejects(new SmsSignIn(limited).start({ phone: "+14155550123" }), /95 seconds/);
});

test("redirects never forward the phone or private session to another origin", async t => {
  const destination = await service(t, ordinary);
  const source = await service(t, (_request, send) => send(307, {}, { location: `${destination.baseUrl}/v1/auth/sms/start` }));
  await assert.rejects(new SmsSignIn(source).start({ phone: "+14155550123" }), /reach Nanocodex/);
  assert.equal(destination.requests.length, 0);
  assert.throws(() => new SmsSignIn({ baseUrl: "https://user:secret@example.com" }));
});

for (const adopt of [false, true]) test(`native JSONL keeps auth private and ${adopt ? "preserves an adopted key" : "revokes an unused key"} on EOF`, { timeout: 5000 }, async t => {
  const { baseUrl, requests } = await service(t, (request, send) => {
    if (request.path === "/v1/agents") send(200, { data: [] });
    else ordinary(request, send);
  });
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-auth-protocol-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const env = { ...process.env, NANOCODEX_DESKTOP_DATA: directory, NANOCODEX_MANAGED_URL: baseUrl, NC_API_KEY: "", NANOCODEX_API_KEY: "", NANOCODEX_ENV_FILE: "" };
  const child = spawn(process.execPath, [new URL("../src/host.mjs", import.meta.url).pathname], { env, stdio: ["pipe", "pipe", "pipe"] });
  t.after(() => { if (child.exitCode === null) child.kill("SIGKILL"); });
  const messages = [], pending = new Map();
  let sequence = 0, stderr = "";
  child.stderr.on("data", chunk => { stderr += chunk; });
  createInterface({ input: child.stdout }).on("line", line => {
    const value = JSON.parse(line); messages.push(value);
    if (value.id !== undefined) {
      const complete = pending.get(value.id); pending.delete(value.id);
      if (value.error) complete.reject(new Error(value.error)); else complete.resolve(value.result);
    }
  });
  const call = (method, input) => new Promise((resolve, reject) => {
    const id = ++sequence; pending.set(id, { resolve, reject });
    child.stdin.write(`${JSON.stringify({ id, method, args: input === undefined ? [] : [input] })}\n`);
  });
  await call("startSignIn", { phone: "+14155550123" });
  const credential = await call("verifySignIn", { code: "123456" });
  assert.deepEqual(credential, { baseUrl, apiKey: key });
  const disconnected = await call("disconnect");
  assert.equal(disconnected.connected, false);
  assert.equal(disconnected.hasCredentials, false);
  if (adopt) assert.equal((await call("connect", { ...credential, remember: false })).connected, true);
  const state = await call("state");
  assert.equal(JSON.stringify(state).includes(key), false);
  assert.equal(JSON.stringify(messages.filter(message => message.event)).includes(key), false);
  assert.equal(JSON.stringify(messages).includes(cookie), false);
  assert.equal(JSON.stringify(messages).includes(challenge), false);
  const exited = once(child, "exit");
  child.stdin.end();
  assert.equal((await exited)[0], 0, stderr);
  assert.equal(requests.filter(request => request.method === "DELETE").length, adopt ? 0 : 1);
  assert.equal(requests.at(-1).path, "/v1/auth/logout");
  assert.equal(stderr.includes(key), false);
  if (adopt) {
    const preferences = await readFile(join(directory, "desktop.json"), "utf8");
    assert.equal(preferences.includes(key), false);
    assert.equal(preferences.includes(cookie), false);
  }
});
