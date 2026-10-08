import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { build } from "esbuild";
import { Miniflare } from "miniflare";

const root = fileURLToPath(new URL("..", import.meta.url));
const output = join(root, "../../output/services-api", `${Date.now()}-${process.pid}`);
const owner = "11111111-1111-4111-8111-111111111111";
const secondOwner = "22222222-2222-4222-8222-222222222222";
const vaultId = "v".repeat(32);
const source = `
import worker from './src/index.ts';
import {UserAccount,Organization,ApiKeyRecord,NonceStorage,ensureAccount,createApiKey} from './src/account-auth.ts';
import {Kv} from 'accounts/server';
export {UserAccount,Organization,ApiKeyRecord,NonceStorage};
export default {async fetch(request,env,ctx){
 if(new URL(request.url).pathname==='/__fixture'){
  const b=await request.json();await ensureAccount(env,b.user,true);
  const auth=await(await env.NANOCODEX_USERS.getByName(b.user).fetch('https://user.internal/authorization')).json();
  const key=await createApiKey(env,{kind:'api_key',userId:b.user,...auth.grant,subjectId:'api_key:'+b.user,credentialId:'fixture',capabilities:b.capabilities},'Synthetic standalone services');
  const token='s_'+crypto.randomUUID().replaceAll('-','')+'A'.repeat(11);
  await Kv.durableObject(env.NANOCODEX_AUTH,{name:'account'}).set('session:'+token,{userId:b.user,authentication:'sms_otp',issuedAt:Date.now()/1000,expiresAt:Date.now()/1000+3600});
  return Response.json({...key,cookie:'nanocodex_account='+token});
 }
 return worker.fetch(request,env,ctx);
}};`;

async function bundle(contents, provenance) {
  const assets = [];
  const compiled = await build({ stdin: { contents, resolveDir: root }, bundle: true, write: false,
    format: "esm", platform: "node", conditions: ["workerd"], target: "es2022",
    banner: { js: 'import {createRequire} from "node:module";const require=createRequire("/worker.mjs");' },
    external: ["cloudflare:*", "node:*"], alias: { "node-rsa": join(root, "../nanocodex/tools/browser/unsupportedNodeRsa.mjs") },
    plugins: [{ name: "wasm", setup(builder) { builder.onResolve({ filter: /(?:\.wasm$|^nanocodex\/wasm$)/ }, async args => {
      const path = args.path === "nanocodex/wasm" ? join(root, "../nanocodex/pkg-web/nanocodex_bg.wasm") : join(args.resolveDir, args.path);
      const contents = await readFile(path), name = `fixture-${assets.length}.wasm`;
      assets.push({ type: "CompiledWasm", path: name, contents });
      provenance.push({ path, sha256: createHash("sha256").update(contents).digest("hex") });
      return { path: `./${name}`, external: true };
    }); } }], logLevel: "silent",
  });
  return [{ type: "ESModule", path: "worker.mjs", contents: compiled.outputFiles[0].text }, ...assets];
}

test("standalone account services use real authentication, no agent, and private approval", { timeout: 120_000 }, async () => {
  await mkdir(output, { recursive: true });
  const trace = [], dispatches = [], provenance = [];
  const modules = await bundle(source, provenance);
  const proxy = await bundle(`import {routeManaged} from '../account/worker/managedProxy.ts';
    export default {async fetch(r,e){return await routeManaged(r,e,new URL(r.url))??new Response(null,{status:404})}}`, provenance);
  let loseReply = false;
  const mf = new Miniflare({ port: 0, workers: [
    { name: "account", modules: proxy, compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat"], serviceBindings: { NANOCODEX_BACKEND: "managed" } },
    { name: "managed", modules, compatibilityDate: "2026-07-30", compatibilityFlags: ["nodejs_compat", "enable_request_signal"],
      // Deliberately no session/model namespace: these routes must not create or run an agent.
      durableObjects: Object.fromEntries([["NANOCODEX_USERS", "UserAccount"], ["NANOCODEX_ORGANIZATIONS", "Organization"],
        ["NANOCODEX_API_KEYS", "ApiKeyRecord"], ["NANOCODEX_AUTH", "NonceStorage"]].map(([name, className]) => [name, { className, useSQLite: true }])),
      serviceBindings: { NANOCODEX: "egress" },
    },
    // A real egress-shaped entrypoint: RPC metadata reads and fetch share one dispatch trail.
    { name: "egress", modules: true, compatibilityDate: "2026-07-30", script: `import { WorkerEntrypoint } from "cloudflare:workers";
      export default class extends WorkerEntrypoint {
        fetch(request) { return this.env.DISPATCH.fetch(request); }
        async readAccountVault(userId) {
          const response = await this.env.DISPATCH.fetch("https://broker.internal/users/" + encodeURIComponent(userId) + "/credentials/vault");
          return { status: response.status, vault: response.ok ? (await response.json()).vault : null };
        }
      }`,
      serviceBindings: { DISPATCH: async request => {
        const url = new URL(request.url);
        dispatches.push({ origin: url.origin, path: url.pathname, method: request.method,
          human_approval: request.headers.get("x-nanocodex-phone-human-approval") === "true" });
        if (url.origin === "https://broker.internal" && url.pathname.endsWith("/credentials/vault")) {
          return Response.json({ vault: [{ id: vaultId, kind: "api_key", name: "Synthetic service", created_at: 1 }] });
        }
        if (url.origin === "https://vault-egress.internal") return Response.json({ status: 204, ok: true, unsafe_extra: "not-projected" });
        if (url.origin === "https://phone-service.internal") {
          if (loseReply) throw new Error("simulated provider connection loss");
          return Response.json({ status: request.method === "GET" ? "ready" : "approval_required" });
        }
        throw new Error("Unexpected dispatch; no model execution is permitted");
      } },
    },
  ] });
  try {
    const base = await mf.ready, backend = await mf.getWorker("managed");
    async function enroll(user, capabilities) {
      const response = await backend.fetch("https://fixture.test/__fixture", { method: "POST", body: JSON.stringify({ user, capabilities }) });
      assert.equal(response.status, 200); return response.json();
    }
    const alice = await enroll(owner, ["data:read", "data:write", "tools:use"]);
    const bob = await enroll(secondOwner, ["data:read", "data:write", "tools:use"]);
    const reader = await enroll(owner, ["data:read"]);
    async function call(path, { method = "GET", token = alice.token, cookie, origin, body, headers = {}, expected = 200 } = {}) {
      const response = await fetch(new URL(path, base), { method, headers: {
        ...(token ? { authorization: `Bearer ${token}` } : {}), ...(cookie ? { cookie } : {}),
        ...(origin ? { origin: origin === "same" ? base.origin : origin } : {}),
        ...(body === undefined ? {} : { "content-type": "application/json" }), ...headers,
      }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
      const wire = await response.text(), value = wire ? JSON.parse(wire) : null;
      trace.push({ path, method, expected, observed: response.status, result: value });
      assert.equal(response.status, expected, wire); if (path.startsWith("/v1/services")) assert.equal(response.headers.get("cache-control"), "no-store");
      return value;
    }
    const catalog = await call("/v1/services"); assert.ok(catalog.services.some(s => s.id === "totp"));
    const beforeLinks = dispatches.length;
    for (const kind of ["login", "api_key", "card", "address", "phone", "totp"]) {
      const link = await call('/v1/services/links?service=vault&action=enroll&kind=' + kind);
      assert.equal(new URL(link.url).origin, 'https://nanocodex.gakonst.workers.dev');
      assert.equal(new URL(link.url).searchParams.get('add'), kind);
      assert.equal(new URL(link.url).searchParams.has('enrollment_origin'), false);
    }
    const picker = await call('/v1/services/links?service=vault&action=select&app_origin=https%3A%2F%2Fclient.example&state=synthetic-callback-state');
    assert.equal(new URL(picker.url).searchParams.get('service'), 'select');
    assert.equal(picker.appOrigin, 'https://client.example');
    await call('/v1/services/links?service=phone&operation_id=11111111-1111-4111-8111-111111111111');
    for (const query of ['kind=password', 'host=https://attacker.example', 'kind=card&kind=login', 'app_origin=javascript:alert(1)', 'state=orphan-state-value']) await call('/v1/services/links?' + query, { expected: 400 });
    await call('/v1/services/links', { token: null, expected: 401 });
    assert.equal(dispatches.length, beforeLinks, 'Navigation must not mutate provider or Vault state');
    assert.equal((await call("/v1/services/vault")).vault[0].id, vaultId);
    await call("/v1/services/vault", { token: null, expected: 401 });
    const envelope = { vault_id: vaultId, url: "https://service.example/login", method: "POST", body: "{{NANOCODEX_VAULT_TOTP}}" };
    assert.deepEqual(await call("/v1/services/vault/request", { method: "POST", body: envelope }), { status: 204, ok: true });
    await call("/v1/services/vault/request", { method: "POST", body: envelope, token: reader.token, expected: 403 });
    await call("/v1/services/vault/request", { method: "POST", body: envelope, token: null, cookie: alice.cookie, origin: "https://attacker.example", expected: 403 });
    await call("/v1/services/phone/numbers", { token: bob.token });
    assert.ok(dispatches.at(-1).path.includes(secondOwner));
    await call("/v1/services/phone/numbers", { method: "POST", body: { operation_id: crypto.randomUUID() }, headers: { "x-nanocodex-phone-human-approval": "true" } });
    assert.equal(dispatches.at(-1).human_approval, false, "API headers cannot self-approve a purchase");
    const approve = "/v1/services/phone/requests/11111111-1111-4111-8111-111111111111/approve";
    const before = dispatches.length;
    await call(approve, { method: "POST", body: {}, expected: 403 });
    await call(approve, { method: "POST", body: {}, token: null, cookie: alice.cookie, origin: "https://attacker.example", expected: 403 });
    assert.equal(dispatches.length, before);
    await call(approve, { method: "POST", body: {}, token: null, cookie: alice.cookie, origin: "same" });
    assert.equal(dispatches.at(-1).human_approval, true);
    loseReply = true;
    const count = dispatches.length;
    await call("/v1/services/phone/numbers", { method: "POST", body: { operation_id: crypto.randomUUID() }, expected: 502 });
    assert.equal(dispatches.length, count + 1, "mutations never automatically retry after ambiguity");
    loseReply = false;
    await call(`/v1/api-keys/${alice.metadata.id}`, { method: "DELETE", token: null, cookie: alice.cookie, origin: "same", expected: 204 });
    await call("/v1/services", { expected: 401 });
    console.log(JSON.stringify({ evidence: output, real_account_auth: true, agent_namespaces: 0, no_mutation_retries: true }));
  } finally {
    await mf.dispose();
    await writeFile(join(output, "trace.json"), JSON.stringify({ command: "node --test test/services-api-journey.test.mjs", trace, dispatches, provenance }, null, 2) + "\n");
  }
});
