import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { generateKeyPairSync, createHash } from "node:crypto";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import test from "node:test";
import { sshCredentialImportDigest, sshTargetResource, sshCredentialImportResourcePrefix } from "../src/sshCredentialImport.mts";
import { cliApp } from "../src/devicePolicy.mts";

const account = `0x${"1".repeat(40)}`;
const user = "11111111-1111-4111-8111-111111111111";
const key = generateKeyPairSync("rsa", { modulusLength: 2048 }).privateKey.export({ type: "pkcs8", format: "pem" });
const credential = { reference: "synthetic-lab", hostname: "server.example.com", port: 2222, username: "deploy", host_key_sha256: `SHA256:${"a".repeat(43)}`, private_key: key };

// Independent wire encoding protects the cross-language CLI/Worker contract.
function commitment(value) {
  const fields = [Buffer.from("nanocodex/ssh-credential-import/v1\0")];
  for (const name of ["reference", "hostname", "username", "host_key_sha256", "private_key"]) {
    const bytes = Buffer.from(value[name]); const length = Buffer.alloc(4); length.writeUInt32BE(bytes.length);
    fields.push(length, bytes);
  }
  const port = Buffer.alloc(4); port.writeUInt32BE(value.port); fields.push(port);
  return sshCredentialImportResourcePrefix + createHash("sha256").update(Buffer.concat(fields)).digest("base64url");
}

test("public Connect SSH import binds approval, writes once, sanitizes failures and consumes uncertain outcomes", async (t) => {
  const outdir = await mkdtemp(path.join(os.tmpdir(), "nanocodex-ssh-worker-"));
  t.after(() => rm(outdir, { recursive: true, force: true }));
  await promisify(execFile)("pnpm", ["exec", "wrangler", "deploy", "--dry-run", "--config", "./wrangler.jsonc", "--outdir", outdir], { cwd: new URL("..", import.meta.url) });
  const { default: worker, ConnectNonceStorage } = await import(new URL(`file://${path.join(outdir, "index.js")}`));
  assert.equal(sshCredentialImportResourcePrefix + await sshCredentialImportDigest(credential), commitment(credential));
  const entries = new Map();
  let transactionTail = Promise.resolve();
  const storage = { get: async k => entries.get(k), put: async (k,v) => { entries.set(k, structuredClone(v)); }, delete: async k => { entries.delete(k); }, transaction: async fn => {
    const previous = transactionTail; let release;
    transactionTail = new Promise(resolve => { release = resolve; });
    await previous;
    try { return await fn(storage); } finally { release(); }
  } };
  const state = new ConnectNonceStorage({ storage });
  let writes = 0;
  let brokerMode = "success";
  const env = {
    CONNECT_STATE: { idFromName: n => n, get: () => ({ fetch: (input, init) => state.fetch(new Request(input, init)) }) },
    ACCOUNTS: { fetch: async request => {
      const url = new URL(request.url);
      if (url.pathname === "/connect/hosted-authorizations/exchange") {
        const body = await request.json();
        return Response.json({ linked: true, user_id: user, account_address: account, resources: body.resources });
      }
      if (url.pathname === "/connect/account-links/resolve") return Response.json({ linked: true, user_id: user });
      if (url.pathname.endsWith("/_connect-existence")) return new Response(null, { status: 204 });
      if (url.pathname === "/v1/agents") return Response.json({ agent_id: user });
      assert.fail(`Unexpected account request ${url.pathname}`);
    } },
    EGRESS: { fetch: async request => {
      const url = new URL(request.url);
      if (url.pathname.includes("/credentials/ssh/")) {
        writes++;
        assert.equal(url.pathname, `/users/${user}/credentials/ssh/${credential.reference}`);
        assert.equal(request.method, "PUT");
        const { reference, ...payload } = credential;
        assert.deepEqual(await request.json(), payload);
        assert(![...entries.keys()].some(k => k.startsWith("connect-approval:")), "approval consumed before broker mutation");
        if (brokerMode === "unknown") throw new Error(key);
        return new Response(brokerMode === "success" ? null : key, { status: brokerMode === "success" ? 204 : brokerMode === "exists" ? 409 : 500 });
      }
      if (url.pathname.startsWith("/subjects/")) return new Response(null, { status: 204 });
      assert.fail(`Unexpected broker request ${url.pathname}`);
    } },
  };
  const pending = [];
  const context = { waitUntil: p => pending.push(p) };
  const resourcesFor = (app = cliApp) => ["urn:nanocodex:agent:run", "urn:nanocodex:authorization:hosted", `urn:nanocodex:app:${app.id}`, `urn:nanocodex:origin:${encodeURIComponent(app.origin)}`, commitment(credential), sshTargetResource(credential)];
  const register = resources => worker.fetch(new Request("https://nanocodex-connect-api.gakonst.workers.dev/v1/device/register", {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({
      code_challenge: "a".repeat(43), code_challenge_method: "S256",
      message: { type: "rpc-requests", payload: [{ jsonrpc: "2.0", id: 1, method: "wallet_connect", params: [{ capabilities: {
        auth: { resources },
        authorizeAccessKey: {
          address: "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266",
          publicKey: "0x048318535b54105d4a7aae60c08fc45f9687181b4fdfc625bd1a753fa7397fed753547f11ca8696646f2f3acb08e31016afac23e630c5d11f59f61fef57b0d2aa5",
          keyType: "secp256k1", chainId: "0x1079", expiry: Math.floor(Date.now()/1000) + 3600,
          limits: [
            { token: "0x20c000000000000000000000f37de3740adec032", limit: "0x0", period: 0 },
            { token: "0x20c000000000000000000000b9537d11c60e8b50", limit: "0x0", period: 0 },
          ], scopes: [],
        },
      } }] }] },
    }),
  }), env, context);
  const registered = await register(resourcesFor());
  assert.equal(registered.status, 200, await registered.clone().text());
  for (const resources of [
    resourcesFor().slice(0, -1), resourcesFor().filter(r => !r.startsWith(sshCredentialImportResourcePrefix)),
    [...resourcesFor(), `urn:nanocodex:credential-import:chatgpt:codex-auth-v1:sha256:${"b".repeat(43)}`, "urn:nanocodex:connector:chatgpt"],
    resourcesFor().map(r => r.startsWith(sshCredentialImportResourcePrefix) ? r + "=" : r),
    resourcesFor().map(r => r.startsWith("urn:nanocodex:ssh-target:") ? r.replace(":2222:", ":02222:") : r),
    resourcesFor().map(r => r.startsWith("urn:nanocodex:ssh-target:")
      ? `urn:nanocodex:ssh-target:${"r".repeat(64)}:${["a".repeat(63), "b".repeat(63), "c".repeat(63), "d".repeat(61)].join(".")}:2222:${"u".repeat(128)}:SHA256%3A${"a".repeat(43)}` : r),
  ]) {
    const response = await register(resources);
    assert.equal(response.status, 400, await response.clone().text());
  }
  assert.equal(writes, 0);
  const authorize = async (resources = resourcesFor(), app = cliApp) => {
    const response = await worker.fetch(new Request("https://connect.test/v1/hosted-authorizations", {
      method: "POST", headers: { origin: "https://nanocodex.gakonst.workers.dev", "content-type": "application/json" },
      body: JSON.stringify({ app_id: app.id, app_origin: app.origin, account_address: account, code: "c".repeat(43), resources }),
    }), env, context);
    assert.equal(response.status, 200, await response.clone().text());
    return (await response.json()).approval_id;
  };
  const connect = (id, fields = {}, app = cliApp) => worker.fetch(new Request("https://connect.test/v1/connections", {
    method: "POST", headers: { origin: app.origin, "content-type": "application/json", "x-nanocodex-app-id": app.id },
    body: JSON.stringify({ app_id: app.id, account_address: account, approval_id: id, authorization_mode: "hosted", permission: "agent.run", requested_connectors: [], ssh_credential_import: credential, ...fields }),
  }), env, context);
  const denied = async (id, fields, app = cliApp, status = 403) => {
    const before = writes; const response = await connect(id, fields, app);
    assert.equal(response.status, status, await response.clone().text());
    assert.equal(writes, before); assert(!(await response.text()).includes(key));
  };
  let id = await authorize();
  await denied(id, { ssh_credential_import: { ...credential, username: "other" } });
  await denied(id, { ssh_credential_import: undefined });
  await denied(id, { ssh_credential_import: { ...credential, private_key: "-----BEGIN OPENSSH PRIVATE KEY-----\naaaa\n-----END OPENSSH PRIVATE KEY-----" } }, cliApp, 400);
  await denied(id, { ssh_credential_import: { ...credential, private_key: key + "a".repeat(65536) } }, cliApp, 400);
  const concurrent = await Promise.all([connect(id), connect(id)]);
  assert.deepEqual(concurrent.map(r => r.status).sort(), [201, 403]);
  const success = concurrent.find(r => r.status === 201);
  assert.equal(writes, 1);
  assert(!(await success.text()).includes(key));
  assert(!JSON.stringify([...entries]).includes(key), "private key is absent from retained grants/approvals");
  await denied(id, {});
  for (const [mode, expectedCode] of [["exists", "ssh_reference_exists"], ["rejected", "ssh_import_failed"], ["unknown", "ssh_import_outcome_unknown"]]) {
    brokerMode = mode; id = await authorize(); const before = writes;
    const response = await connect(id); const result = await response.json();
    assert.equal(result.error.code, expectedCode); assert(!JSON.stringify(result).includes(key));
    assert.equal(writes, before + 1); await denied(id, {});
  }
  brokerMode = "success";
  const other = { id: "other-app", origin: "https://other.example.com" };
  id = await authorize(resourcesFor(other), other); await denied(id, {}, other);
  // Discard unused synthetic approvals to keep the pre-mutation assertion strict.
  entries.delete(`connect-approval:${id}`);
  for (const resources of [resourcesFor().slice(0, -1), resourcesFor().filter(r => !r.startsWith(sshCredentialImportResourcePrefix)), resourcesFor().map(r => r.startsWith("urn:nanocodex:ssh-target:") ? sshTargetResource({ ...credential, port: 22 }) : r)]) {
    id = await authorize(resources); await denied(id, {}); entries.delete(`connect-approval:${id}`);
  }
  await Promise.all(pending);
  t.diagnostic(`public /v1/hosted-authorizations -> /v1/connections: success, target/body/missing-resource/app/PEM denial, sanitized 409/500/transport exception; ${writes} broker PUTs total; each approval max one write and no private key in retained state`);
});
