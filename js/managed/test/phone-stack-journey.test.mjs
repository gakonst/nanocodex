import assert from "node:assert/strict";
import { test } from "node:test";
import { createHash, createHmac, randomUUID } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import { fetch } from "./support/miniflare-fetch.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const output = join(root, "../../output/phone-stack", `${Date.now()}-${process.pid}`);
const aliceId = "11111111-1111-4111-8111-111111111111";
const bobId = "22222222-2222-4222-8222-222222222222";
const sid = "AC" + "1".repeat(32), secret = "synthetic-phone-stack-secret";
const webhook = "https://account.test/v1/services/phone/webhook";
const phone = "+14155550101", recoveryPhone = "+14155550102";

// Only enrollment is a fixture. All requests after enrollment enter the actual
// account Worker and traverse the production managed/egress/provider Workers.
const managedSource = `
import worker from './src/index.ts';
import {UserAccount,Organization,ApiKeyRecord,NonceStorage,ensureAccount,createApiKey} from './src/account-auth.ts';
import {Kv} from 'accounts/server';
export {UserAccount,Organization,ApiKeyRecord,NonceStorage};
export {PhoneProvider} from './src/phone-provider.ts';
export default {async fetch(request,env,ctx){
 if(new URL(request.url).pathname==='/__fixture'){
  const b=await request.json();await ensureAccount(env,b.user,true);
  const auth=await(await env.NANOCODEX_USERS.getByName(b.user).fetch('https://user.internal/authorization')).json();
  const key=await createApiKey(env,{kind:'api_key',userId:b.user,...auth.grant,subjectId:'api_key:'+b.user,credentialId:'fixture',capabilities:b.capabilities},'Synthetic phone stack');
  const token='s_'+crypto.randomUUID().replaceAll('-','')+'A'.repeat(11);
  await Kv.durableObject(env.NANOCODEX_AUTH,{name:'account'}).set('session:'+token,{userId:b.user,authentication:'sms_otp',issuedAt:Date.now()/1000,expiresAt:Date.now()/1000+3600});
  return Response.json({...key,cookie:'nanocodex_account='+token});
 }
 return worker.fetch(request,env,ctx);
}};`;

// Same workerd bundling contract as services-api-journey; no production module
// replacements. WASM imports remain real compiled assets, with provenance.
async function bundle(contents, provenance) {
  const assets = [];
  const compiled = await build({ stdin: { contents, resolveDir: root }, bundle: true, write: false,
    format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    banner: { js: 'import {createRequire} from "node:module";const require=createRequire("/worker.mjs");' },
    external: ["cloudflare:*", "node:*"], alias: { "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
    plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /(?:\.wasm(?:\?module)?$|^nanocodex\/wasm$)/ }, async args => {
      const path = args.path === "nanocodex/wasm" ? join(root, "../nanocodex/pkg-web/nanocodex_bg.wasm") : resolve(args.resolveDir, args.path.replace("?module", ""));
      const contents = await readFile(path), name = `fixture-${assets.length}.wasm`;
      assets.push({ type: "CompiledWasm", path: name, contents });
      provenance.push({ path, sha256: createHash("sha256").update(contents).digest("hex") });
      return { path: `./${name}`, external: true };
    }); } }], logLevel: "silent",
  });
  return [{ type: "ESModule", path: "worker.mjs", contents: compiled.outputFiles[0].text }, ...assets];
}

test("phone public HTTP journey crosses account auth, egress storage and named managed provider without agents", { timeout: 120_000 }, async () => {
  await mkdir(output, { recursive: true });
  const trace = [], providerTrace = [], provenance = [];
  const owned = new Map();
  let purchases = 0, releases = 0, losePurchaseReply = false, loseReleaseReply = false;
  const forbiddenOutbound = async request => { throw new Error("Unexpected external request: " + request.url); };
  const provider = async request => {
    const url = new URL(request.url);
    assert.ok(["https://api.twilio.com", "https://pricing.twilio.com"].includes(url.origin), request.url);
    assert.equal(request.headers.get("authorization"), "Basic " + Buffer.from(`${sid}:${secret}`).toString("base64"));
    providerTrace.push({ origin: url.origin, path: url.pathname, method: request.method });
    if (url.hostname === "pricing.twilio.com") return Response.json(url.pathname.includes("PhoneNumbers")
      ? { price_unit: "USD", phone_number_prices: [{ number_type: "local", current_price: "1.15" }] }
      : { price_unit: "USD", inbound_sms_prices: [{ number_type: "local", current_price: "0.0083" }] });
    if (url.pathname.includes("AvailablePhoneNumbers")) return Response.json({ available_phone_numbers:
      (url.searchParams.has("Contains") ? [url.searchParams.get("Contains")] : [phone, recoveryPhone])
        .map(phone_number => ({ phone_number, iso_country: "US", capabilities: { SMS: true } })) });
    if (request.method === "POST" && url.pathname.endsWith("/IncomingPhoneNumbers.json")) {
      purchases++;
      const form = new URLSearchParams(await request.text());
      assert.equal(form.get("SmsUrl"), webhook); assert.equal(form.get("SmsMethod"), "POST");
      const number = { sid: "PN" + String(purchases).padStart(32, "0"), account_sid: sid,
        phone_number: form.get("PhoneNumber"), friendly_name: form.get("FriendlyName"), sms_url: webhook, sms_method: "POST" };
      owned.set(number.sid, number);
      if (losePurchaseReply) { losePurchaseReply = false; return new Response("lost reply after provider commit", { status: 503 }); }
      return Response.json(number, { status: 201 });
    }
    if (request.method === "GET" && url.pathname.endsWith("/IncomingPhoneNumbers.json")) return Response.json({ incoming_phone_numbers:
      [...owned.values()].filter(number => number.phone_number === url.searchParams.get("PhoneNumber") && number.friendly_name === url.searchParams.get("FriendlyName")) });
    if (url.pathname.includes("/IncomingPhoneNumbers/")) {
      const id = url.pathname.split("/").at(-1).replace(".json", "");
      if (request.method === "GET") return owned.has(id) ? Response.json(owned.get(id)) : Response.json({ code: 20404 }, { status: 404 });
      if (request.method === "DELETE") {
        releases++; owned.delete(id);
        if (loseReleaseReply) { loseReleaseReply = false; return new Response(null, { status: 503 }); }
        return new Response(null, { status: 204 });
      }
    }
    throw new Error("Unexpected Twilio request: " + request.method + " " + request.url);
  };
  const [accountModules, managedModules, egressModules] = await Promise.all([
    bundle("export {default} from '../account/worker/index.ts';", provenance),
    bundle(managedSource, provenance),
    bundle("export {default} from '../egress/src/egress.ts';export {PhoneServiceAccount} from '../egress/src/egress.ts';", provenance),
  ]);
  const common = { compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"] };
  const mf = new Miniflare({ port: 0, workers: [
    { ...common, name: "account", modules: accountModules, bindings: { ENVIRONMENT: "test" },
      serviceBindings: { NANOCODEX_BACKEND: "managed", PHONE_SERVICE_EGRESS: "egress" }, outboundService: forbiddenOutbound },
    { ...common, name: "managed", modules: managedModules,
      // Intentionally no agent/session/model namespace or model transport.
      durableObjects: Object.fromEntries([["NANOCODEX_USERS", "UserAccount"], ["NANOCODEX_ORGANIZATIONS", "Organization"],
        ["NANOCODEX_API_KEYS", "ApiKeyRecord"], ["NANOCODEX_AUTH", "NonceStorage"]].map(([name, className]) => [name, { className, useSQLite: true }])),
      bindings: { TWILIO_ACCOUNT_SID: sid, TWILIO_AUTH_TOKEN: secret },
      serviceBindings: { NANOCODEX: "egress" }, outboundService: provider },
    { ...common, name: "egress", modules: egressModules,
      durableObjects: { PHONE_SERVICE_ACCOUNTS: { className: "PhoneServiceAccount", useSQLite: true } },
      bindings: { ENVIRONMENT: "test", CREDENTIAL_ENCRYPTION_KEY: "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY",
        PHONE_WEBHOOK_URL: webhook, PHONE_PROVISIONING_ENABLED: "true", PHONE_MAX_MONTHLY_PRICE: "2",
        PHONE_MAX_INBOUND_SMS_PRICE: "0.02", PHONE_MAX_NUMBERS_PER_OWNER: "3" },
      serviceBindings: { TWILIO_PHONE_PROVIDER: { name: "managed", entrypoint: "PhoneProvider" } }, outboundService: forbiddenOutbound },
  ] });
  try {
    const base = await mf.ready, backend = await mf.getWorker("managed");
    async function enroll(user, capabilities = ["data:read", "data:write", "tools:use"]) {
      const response = await backend.fetch("https://fixture.test/__fixture", { method: "POST", body: JSON.stringify({ user, capabilities }) });
      assert.equal(response.status, 200); return response.json();
    }
    const alice = await enroll(aliceId), bob = await enroll(bobId), reader = await enroll(aliceId, ["data:read"]);
    async function call(path, { method = "GET", token = alice.token, cookie, origin, body, headers = {}, expected = 200 } = {}) {
      const response = await fetch(new URL("/v1/services/phone" + path, base), { method, headers: {
        ...(token ? { authorization: `Bearer ${token}` } : {}), ...(cookie ? { cookie } : {}),
        ...(origin ? { origin: origin === "same" ? base.origin : origin } : {}),
        ...(body === undefined ? {} : { "content-type": "application/json" }), ...headers,
      }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
      const raw = await response.text();
      assert.ok(!raw.includes(secret));
      const result = raw ? JSON.parse(raw) : null;
      trace.push({ path, method, actor: token === bob.token ? "bob" : token === reader.token ? "reader" : cookie ? "alice-session" : token ? "alice-key" : "anonymous",
        expected, observed: response.status, result });
      assert.equal(response.status, expected, raw);
      assert.equal(response.headers.get("cache-control"), "no-store");
      return result;
    }
    const human = { token: null, cookie: alice.cookie, origin: "same" };
    const stage = (phone_number, options = {}) => call("/numbers", { method: "POST", expected: 202, body: { operation_id: randomUUID(), phone_number, country: "US" }, ...options });
    const approvalBody = operation => operation.kind === "purchase" ? { quote_id: operation.quote.id, accept_recurring: true } : { confirm_release: true };
    const approve = (operation, options = {}) => call(`/requests/${operation.operation_id}/approve`, { method: "POST", body: approvalBody(operation), ...human, ...options });
    async function sms(number, { invalid = false, messageSid = "SM" + "2".repeat(32), expected = 200 } = {}) {
      const form = new URLSearchParams({ AccountSid: sid, To: number, From: "+14155550999", MessageSid: messageSid, Body: "Synthetic stack code 481927", NumMedia: "0" });
      const signature = createHmac("sha1", secret).update(webhook + [...form.keys()].sort().map(key => key + form.get(key)).join("")).digest("base64");
      const response = await fetch(new URL("/v1/services/phone/webhook", base), { method: "POST", headers: {
        "content-type": "application/x-www-form-urlencoded", "x-twilio-signature": invalid ? "invalid" : signature,
      }, body: form });
      trace.push({ public_webhook: messageSid, expected, observed: response.status, body: await response.text() });
      assert.equal(response.status, expected);
    }
    await call("/numbers", { token: null, expected: 401 });
    assert.equal((await call("/numbers")).numbers.length, 0);
    assert.equal((await call("/numbers/available?country=US&area_code=415")).numbers.length, 2);
    await stage(phone, { token: reader.token, expected: 403 });
    const draft = (await stage(phone, { headers: { "x-nanocodex-phone-human-approval": "true" } })).request;
    assert.equal(draft.status, "pending_approval"); assert.ok(draft.quote.id); assert.equal(purchases, 0, "quote and forged approval header cannot spend");
    const beforeDenied = providerTrace.length;
    await approve(draft, { token: alice.token, cookie: undefined, origin: undefined, headers: { "x-nanocodex-phone-human-approval": "true" }, expected: 403 });
    await approve(draft, { origin: "https://attacker.example", expected: 403 });
    await approve(draft, { origin: undefined, expected: 403 });
    await approve(draft, { cookie: bob.cookie, expected: 404 });
    await call(`/requests/${draft.operation_id}`, { token: bob.token, expected: 404 });
    assert.equal(providerTrace.length, beforeDenied, "denied approval never reaches Twilio");
    const purchased = (await approve(draft)).request;
    assert.equal(purchased.status, "complete"); assert.equal(purchases, 1);
    await approve(draft); assert.equal(purchases, 1, "approval replay never repurchases");
    const id = purchased.number_id;
    const active = (await call("/numbers")).numbers;
    assert.equal(active.length, 1); assert.equal(active[0].id, id); assert.equal(active[0].status, "active");
    assert.equal((await call("/numbers", { token: bob.token })).numbers.length, 0);
    await sms(phone, { invalid: true, expected: 403 });
    await sms(phone); await sms(phone);
    const inbox = await call(`/numbers/${id}/messages`);
    assert.equal(inbox.messages.length, 1); assert.equal(inbox.messages[0].body, "Synthetic stack code 481927");
    await call(`/numbers/${id}/messages`, { token: bob.token, expected: 404 });
    await call(`/numbers/${id}`, { token: bob.token, expected: 404 });

    const release = (await call(`/numbers/${id}`, { method: "DELETE", expected: 202, body: { operation_id: randomUUID() }, headers: { "x-nanocodex-phone-human-approval": "true" } })).request;
    assert.equal(release.status, "pending_approval"); assert.equal(releases, 0);
    await approve(release, { token: alice.token, cookie: undefined, origin: undefined, expected: 403 });
    assert.equal((await approve(release)).request.status, "complete"); assert.equal(releases, 1);
    await approve(release); assert.equal(releases, 1);
    assert.equal((await call(`/numbers/${id}`)).number.status, "released");
    await sms(phone, { messageSid: "SM" + "3".repeat(32), expected: 404 });

    // Provider commits but returns an ambiguous failure. Only receipt GETs may
    // reconcile, and every provider operation during reconciliation must be GET.
    const recovery = (await stage(recoveryPhone)).request;
    losePurchaseReply = true;
    assert.equal((await approve(recovery, { expected: 202 })).request.status, "outcome_unknown"); assert.equal(purchases, 2);
    assert.equal((await approve(recovery)).request.status, "outcome_unknown"); assert.equal(purchases, 2);
    const beforeRead = providerTrace.length;
    const receipt = (await call(`/requests/${recovery.operation_id}`, { token: reader.token })).request;
    assert.equal(receipt.status, "complete"); assert.equal(purchases, 2);
    assert.ok(providerTrace.length > beforeRead);
    assert.ok(providerTrace.slice(beforeRead).every(entry => entry.method === "GET"));
    const releaseRecovery = (await call(`/numbers/${receipt.number_id}`, { method: "DELETE", expected: 202, body: { operation_id: randomUUID() } })).request;
    loseReleaseReply = true;
    assert.equal((await approve(releaseRecovery, { expected: 202 })).request.status, "outcome_unknown"); assert.equal(releases, 2);
    await approve(releaseRecovery); assert.equal(releases, 2);
    const beforeReleaseRead = providerTrace.length;
    assert.equal((await call(`/requests/${releaseRecovery.operation_id}`, { token: reader.token })).request.status, "complete");
    assert.ok(providerTrace.length > beforeReleaseRead);
    assert.ok(providerTrace.slice(beforeReleaseRead).every(entry => entry.method === "GET"));
    assert.equal(releases, 2);
    await sms(recoveryPhone, { messageSid: "SM" + "4".repeat(32), expected: 404 });
    console.log(JSON.stringify({ evidence: output, real_workers: ["account", "managed", "egress"], named_provider: "managed.PhoneProvider",
      agent_namespaces: 0, purchases, releases, read_only_reconciliation: true }));
  } finally {
    await mf.dispose();
    await writeFile(join(output, "trace.json"), JSON.stringify({ command: "node --test test/phone-stack-journey.test.mjs", trace, providerTrace, provenance, purchases, releases }, null, 2) + "\n");
  }
});
