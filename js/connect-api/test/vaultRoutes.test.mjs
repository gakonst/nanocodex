import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import test from "node:test";

const execFileAsync = promisify(execFile);
const appId = "atlas-workspace";
const appOrigin = "https://nanocodex-connect-playground.gakonst.workers.dev";
const accountAddress = `0x${"1".repeat(40)}`;
const grantId = `0x${"2".repeat(64)}`;
const grantToken = "t".repeat(43);
const vaultId = "v".repeat(32);

test("Connect account-info projects Vault metadata and Vault egress stays reference-only", async (t) => {
  const outdir = await mkdtemp(path.join(os.tmpdir(), "nanocodex-connect-vault-"));
  t.after(() => rm(outdir, { recursive: true, force: true }));
  await execFileAsync(process.platform === "win32" ? "npx.cmd" : "npx", [
    "wrangler", "deploy", "--dry-run", "--config", "./wrangler.jsonc", "--outdir", outdir,
  ], { cwd: new URL("..", import.meta.url) });

  const worker = (await import(new URL(`file://${path.join(outdir, "index.js")}`))).default;
  const grant = {
    id: grantId,
    appId,
    appOrigin,
    accountAddress,
    brokerUserId: accountAddress,
    agentId: "agent-test",
    permission: "agent.run",
    status: "active",
    expiresAt: Math.floor(Date.now() / 1000) + 3_600,
    capabilities: [],
    spentAtomics: "0",
    egressSubject: "s".repeat(43),
  };
  const principal = { accountAddress, appId, appOrigin, grantId };
  const privateRequests = [];
  const durableStub = {
    async fetch(input) {
      const url = new URL(input instanceof Request ? input.url : input);
      if (url.pathname === "/resolve-grant") return Response.json({ grant, principal });
      if (url.pathname === "/get") return Response.json({ value: undefined });
      return Response.json({ error: "unexpected durable operation" }, { status: 500 });
    },
  };
  const env = {
    CONNECT_STATE: { idFromName: (name) => name, get: () => durableStub },
    EGRESS: {
      async fetch(request) {
        const url = new URL(request.url);
        if (url.pathname.endsWith("/connectors")) return Response.json({ connectors: {} });
        if (url.pathname.endsWith("/credentials")) return Response.json({
          chatgpt: {},
          vault: [{
            id: vaultId,
            kind: "login",
            name: "Example",
            created_at: 123,
            username: "person@example.test",
            password: "must-not-escape",
            unknown: "must-not-escape",
          }],
        });
        if (url.origin === "https://vault-egress.internal" && url.pathname === "/v1/request") {
          privateRequests.push(request.clone());
          return Response.json({ status: 204 }, { status: 200 });
        }
        return Response.json({ error: "unexpected egress operation" }, { status: 500 });
      },
    },
  };
  const headers = {
    authorization: `Bearer ${grantToken}`,
    origin: appOrigin,
    "x-nanocodex-app-id": appId,
  };
  const nativeFetch = globalThis.fetch;
  globalThis.fetch = async (request) => {
    assert.equal(new URL(request instanceof Request ? request.url : request).hostname, "api.tempo.xyz");
    return Response.json({ jsonrpc: "2.0", id: 1, result: "0x0" });
  };
  t.after(() => { globalThis.fetch = nativeFetch; });

  const account = await worker.fetch(new Request(
    "https://nanocodex.gakonst.workers.dev/v1/agent/account-info",
    { headers },
  ), env, { waitUntil() {} });
  assert.equal(account.status, 200);
  assert.deepEqual((await account.json()).vault, [{
    id: vaultId,
    kind: "login",
    name: "Example",
    created_at: 123,
    username: "person@example.test",
  }]);

  globalThis.fetch = async () => { throw new Error("Vault egress must not use public fetch"); };
  const vaultResponse = await worker.fetch(new Request(
    "https://nanocodex.gakonst.workers.dev/v1/egress",
    {
      method: "POST",
      headers: { ...headers, "content-type": "application/json" },
      body: JSON.stringify({
        thread_id: "00000000-0000-4000-8000-000000000000",
        url: "https://example.com/login",
        method: "POST",
        headers: {
          "x-nanocodex-vault-id": vaultId,
          authorization: "Basic {{NANOCODEX_VAULT_BASIC}}",
        },
      }),
    },
  ), env, { waitUntil() {} });
  assert.equal(vaultResponse.status, 200);
  assert.equal(privateRequests.length, 1);
  assert.equal(privateRequests[0].url, "https://vault-egress.internal/v1/request");
  assert.equal(privateRequests[0].method, "POST");
  assert.equal(privateRequests[0].headers.get("content-type"), "application/json");
  assert.equal(privateRequests[0].headers.get("x-nanocodex-subject"), grant.egressSubject);
  assert.deepEqual(await privateRequests[0].json(), {
    vault_id: vaultId,
    url: "https://example.com/login",
    method: "POST",
    headers: { authorization: "Basic {{NANOCODEX_VAULT_BASIC}}" },
  });
});
