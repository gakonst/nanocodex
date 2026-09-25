import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import test from "node:test";

const appId = "djbooth";
const appOrigin = "https://djbooth-library.gakonst.workers.dev";
const accountAddress = `0x${"1".repeat(40)}`;
const agentId = "11111111-1111-4111-8111-111111111111";
const digest = `0x${"a".repeat(64)}`;
const sandboxResource = "urn:nanocodex:agent:execution:sandbox";
const resources = [
  sandboxResource,
  "urn:nanocodex:agent:run",
  `urn:nanocodex:app:${appId}`,
  `urn:nanocodex:origin:${encodeURIComponent(appOrigin)}`,
  "urn:nanocodex:authorization:hosted",
  "urn:nanocodex:connector:chatgpt",
  "urn:nanocodex:agent:visibility:reply,actions",
  `urn:nanocodex:app-tool-catalog:sha256:${digest.slice(2)}`,
];

test("generic hosted apps exchange non-spending approvals into bound agent grants", async (t) => {
  const outdir = await mkdtemp(path.join(os.tmpdir(), "nanocodex-hosted-worker-"));
  t.after(() => rm(outdir, { recursive: true, force: true }));
  await promisify(execFile)(
    process.platform === "win32" ? "npx.cmd" : "npx",
    ["wrangler", "deploy", "--dry-run", "--config", "./wrangler.jsonc", "--outdir", outdir],
    { cwd: new URL("..", import.meta.url) },
  );
  const { default: worker, ConnectNonceStorage } = await import(new URL(`file://${path.join(outdir, "index.js")}`));
  const entries = new Map();
  const storage = {
    get: async (key) => entries.get(key),
    put: async (key, value) => { entries.set(key, structuredClone(value)); },
    delete: async (key) => { entries.delete(key); },
    transaction: async (operation) => operation(storage),
  };
  const state = new ConnectNonceStorage({ storage });
  let exchanged = 0;
  let expectedSandbox = "true";
  let rejectAccount = false;
  const env = {
    CONNECT_STATE: {
      idFromName: (name) => name,
      get: () => ({ fetch: (input, init) => state.fetch(new Request(input, init)) }),
    },
    ACCOUNTS: { fetch: async (request) => {
      const url = new URL(request.url);
      if (url.pathname === "/connect/hosted-authorizations/exchange") {
        exchanged++;
        const body = await request.json();
        if (rejectAccount) return new Response(null, { status: 403 });
        return Response.json({ linked: true, user_id: accountAddress, account_address: accountAddress, resources: body.resources });
      }
      assert.equal(request.headers.get("x-nanocodex-connect-sandbox-execution"), expectedSandbox);
      if (url.pathname === `/v1/agents/${agentId}/_connect-existence`) return new Response(null, { status: 204 });
      assert.equal(url.pathname, "/v1/agents");
      return Response.json({ agent_id: agentId });
    } },
    EGRESS: { fetch: async (request) => {
      const url = new URL(request.url);
      if (url.pathname.endsWith("/connectors")) return Response.json({ connectors: {} });
      if (url.pathname.endsWith("/credentials")) return Response.json({ chatgpt: { connected: true } });
      if (url.pathname.startsWith("/subjects/")) return new Response(null, { status: 204 });
      assert.fail(`Unexpected broker request: ${url.pathname}`);
    } },
  };
  const pending = [];
  const context = { waitUntil(promise) { pending.push(promise); } };
  const authorize = (approved = resources, fields = {}) => worker.fetch(new Request("https://connect.test/v1/hosted-authorizations", {
    method: "POST",
    headers: { origin: "https://nanocodex.gakonst.workers.dev", "content-type": "application/json" },
    body: JSON.stringify({ app_id: appId, app_origin: appOrigin, account_address: accountAddress, code: "c".repeat(43), resources: approved, ...fields }),
  }), env, context);
  const approvalResponse = await authorize();
  assert.equal(approvalResponse.status, 200, await approvalResponse.clone().text());
  const approval = await approvalResponse.json();
  assert.equal(exchanged, 1);
  assert.deepEqual(entries.get(`connect-approval:${approval.approval_id}`).value.resources, resources);
  const connect = (fields = {}, origin = appOrigin) => worker.fetch(new Request("https://connect.test/v1/connections", {
    method: "POST",
    headers: { origin, "content-type": "application/json", "x-nanocodex-app-id": appId },
    body: JSON.stringify({ app_id: appId, account_address: accountAddress, approval_id: approval.approval_id,
      authorization_mode: "hosted", permission: "agent.run", requested_connectors: ["chatgpt"],
      requested_app_tool_catalog_digest: digest, ...fields }),
  }), env, context);

  for (const [fields, origin, code] of [
    [{}, "https://other.example", "app_not_approved"],
    [{ requested_connectors: ["chatgpt", "spotify"] }, appOrigin, "connector_not_approved"],
    [{ requested_app_tool_catalog_digest: `0x${"b".repeat(64)}` }, appOrigin, "app_tool_catalog_mismatch"],
    [{ key_authorization: {} }, appOrigin, "hosted_authorization_denied"],
  ]) {
    const response = await connect(fields, origin);
    assert.equal(response.status, 403, await response.clone().text());
    assert.equal((await response.json()).error.code, code);
  }
  const connected = await connect();
  assert.equal(connected.status, 201, await connected.clone().text());
  const grants = [...entries].filter(([key]) => key.startsWith("grant:"));
  assert.equal(grants.length, 1);
  const grant = grants[0][1].value;
  assert.equal(grant.agentId, agentId);
  assert.equal(grant.appId, appId);
  assert.equal(grant.appOrigin, appOrigin);
  assert.equal(grant.accountAddress, accountAddress);
  assert.equal(grant.appToolCatalogDigest, digest);
  assert(grant.capabilities.includes("chatgpt"));
  assert(grant.capabilities.includes("agent.execution.sandbox"));
  assert(!grant.capabilities.includes("mpp.mach"));
  assert(!grant.capabilities.includes("mercator.boost"));
  assert.equal(grant.accessKey, undefined);
  assert.equal((await connect()).status, 403, "approval cannot be replayed");

  for (const [approved, fields, code] of [
    [resources.filter((r) => r !== "urn:nanocodex:agent:run"), {}, "capability_not_approved"],
    [resources, { app_id: "other-app" }, "app_identity_mismatch"],
    [resources, { app_origin: "https://other.example" }, "app_identity_mismatch"],
    [[...resources, "urn:nanocodex:mpp:machusd:spend"], {}, "hosted_authorization_denied"],
  ]) {
    const response = await authorize(approved, fields);
    assert.equal(response.status, 403, await response.clone().text());
    assert.equal((await response.json()).error.code, code);
  }
  assert.equal(exchanged, 1, "invalid approvals never reach the account exchange");
  expectedSandbox = null;
  const unscoped = await (await authorize(resources.filter(r => r !== sandboxResource))).json();
  const forged = await connect({ approval_id: unscoped.approval_id, capabilities: ["agent.execution.sandbox"], sandboxExecution: true });
  assert.equal(forged.status, 400, "forged capability fields are rejected before grant creation");
  const unscopedConnected = await connect({ approval_id: unscoped.approval_id });
  assert.equal(unscopedConnected.status, 201, await unscopedConnected.clone().text());
  const unscopedGrant = [...entries].filter(([key]) => key.startsWith("grant:")).map(([, record]) => record.value).find(value => value.id !== grant.id);
  assert(unscopedGrant);
  assert(!unscopedGrant.capabilities.includes("agent.execution.sandbox"), "caller fields cannot elevate the approved resource set");
  rejectAccount = true;
  assert.equal((await authorize()).status, 403, "account service still must approve the exact resources");
  await Promise.all(pending);
});
