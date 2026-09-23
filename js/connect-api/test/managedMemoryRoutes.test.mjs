import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import test from "node:test";

const appId = "nanocodex-cli";
const appOrigin = "https://cli.nanocodex.xyz";
const token = "t".repeat(43);
const grantId = `0x${"a".repeat(64)}`;
const accountAddress = `0x${"1".repeat(40)}`;

// Exercise the public grant boundary, not just an endpoint-name helper.
test("CLI Connect forwards canonical memory APIs with live scoped authority", async (t) => {
  const outdir = await mkdtemp(path.join(os.tmpdir(), "nanocodex-memory-routes-"));
  t.after(() => rm(outdir, { recursive: true, force: true }));
  await promisify(execFile)(process.execPath, [
    new URL("../node_modules/wrangler/bin/wrangler.js", import.meta.url).pathname,
    "deploy", "--dry-run", "--config", "./wrangler.jsonc", "--outdir", outdir,
  ], { cwd: new URL("..", import.meta.url) });
  const worker = (await import(new URL(`file://${path.join(outdir, "index.js")}`))).default;
  const base = { id: grantId, appId, appOrigin, accountAddress,
    brokerUserId: "11111111-1111-4111-8111-111111111111", agentId: "memory-route-test",
    permission: "agent.run", status: "active", expiresAt: Math.floor(Date.now() / 1000) + 3600,
    capabilities: ["memory:read", "memory:write", "history:read"], spentAtomics: "0", egressSubject: "s".repeat(43) };
  let grant = { ...base };
  const forwarded = [];
  let reply = () => Response.json({ preserved: true });
  const env = {
    CONNECT_STATE: { idFromName: name => name, get: () => ({ fetch: async input => {
      assert.equal(new URL(input).pathname, "/resolve-grant");
      return Response.json(new URL(input).searchParams.get("token") === token
        ? { grant, principal: { accountAddress, appId, appOrigin, grantId } } : {});
    } }) },
    ACCOUNTS: { fetch: async request => { forwarded.push(request.clone()); return reply(); } },
  };
  const context = { waitUntil() {} };
  const request = (method, body = {}, headers = {}, verb = "POST") => new Request(
    `https://connect.example/v1/memories/${method}`, { method: verb,
      headers: { authorization: `Bearer ${token}`, origin: appOrigin, "x-nanocodex-app-id": appId,
        "content-type": "application/json", ...headers },
      ...(verb === "POST" ? { body: JSON.stringify(body) } : {}) });
  const cases = [
    ["list", { path: "memory", max_results: 10 }],
    ["read", { path: "USER.md", line_offset: 2, max_lines: 3 }],
    ["search", { queries: ["canary"], match_mode: { type: "all_on_same_line" } }],
    ["add_ad_hoc_note", { filename: "2026-09-23T12-00-00-canary.md", note: "verbatim canary\n".repeat(2000) }],
    ["write", { operation: "put", path: "MEMORY.md", content: "canary", user_requested: true }],
    ["status", {}],
  ];
  for (const [method, body] of cases) {
    const response = await worker.fetch(request(method, body), env, context);
    assert.equal(response.status, 200, await response.clone().text());
    assert.deepEqual(await response.json(), { preserved: true });
    const upstream = forwarded.at(-1);
    assert.equal(upstream.url, `https://nanocodex.internal/v1/memories/${method}`);
    assert.equal(upstream.method, "POST");
    assert.deepEqual(await upstream.json(), body);
    assert.equal(upstream.headers.has("authorization"), false);
    assert.equal(upstream.headers.get("x-nanocodex-connect-grant-id"), grantId);
  }
  const count = forwarded.length;
  for (const [method] of cases) {
    grant = { ...base, capabilities: [method === "write" || method === "add_ad_hoc_note" ? "memory:read" : "memory:write"] };
    assert.equal((await worker.fetch(request(method), env, context)).status, 403);
  }
  grant = { ...base };
  for (const headers of [{ authorization: `Bearer ${"u".repeat(43)}` }, { origin: "https://other.example" }, { "x-nanocodex-app-id": "other" }]) {
    assert.ok((await worker.fetch(request("read", {}, headers), env, context)).status >= 400);
  }
  for (const changed of [{ status: "revoked" }, { expiresAt: 1 }]) {
    grant = { ...base, ...changed };
    assert.ok((await worker.fetch(request("read"), env, context)).status >= 400);
  }
  grant = { ...base };
  assert.equal((await worker.fetch(request("read", {}, {}, "GET"), env, context)).status, 405);
  assert.equal((await worker.fetch(request("read?scope=personal"), env, context)).status, 400);
  assert.equal(forwarded.length, count, "rejected requests never reach account storage");
  reply = () => new Response(null, { status: 302, headers: { location: "https://other.example" } });
  assert.equal((await worker.fetch(request("read"), env, context)).status, 502);
  reply = () => new Response("not JSON");
  assert.equal((await worker.fetch(request("read"), env, context)).status, 502);
});
