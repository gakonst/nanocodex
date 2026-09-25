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
const agentId = "11111111-1111-4111-8111-111111111111";
const artifactId = "a".repeat(64);
const contentPath = `/artifacts/${artifactId}/content`;
const inputPath = "/inputs/22222222-2222-4222-8222-222222222222/model.stl";

// Public Worker boundary coverage: output capability bypasses, noncanonical
// routes, stale/wrong principals, forged input authority, and altered file bytes
// or missing download metadata are independent authorization/protocol failures.
test("Connect durable files preserve scoped authority and exact artifact bytes", async (t) => {
  // Node's Fetch implementation requires duplex for streamed request bodies;
  // Workers accepts the same Request without that Node-only option.
  const NativeRequest = globalThis.Request;
  globalThis.Request = class extends NativeRequest {
    constructor(input, init) {
      super(input, init?.body instanceof ReadableStream ? { ...init, duplex: "half" } : init);
    }
  };
  t.after(() => { globalThis.Request = NativeRequest; });
  const outdir = await mkdtemp(path.join(os.tmpdir(), "nanocodex-managed-files-"));
  t.after(() => rm(outdir, { recursive: true, force: true }));
  await promisify(execFile)(process.execPath, [
    new URL("../node_modules/wrangler/bin/wrangler.js", import.meta.url).pathname,
    "deploy", "--dry-run", "--config", "./wrangler.jsonc", "--outdir", outdir,
  ], { cwd: new URL("..", import.meta.url) });
  const worker = (await import(new URL(`file://${path.join(outdir, "index.js")}`))).default;
  const base = { id: grantId, appId, appOrigin, accountAddress,
    brokerUserId: "11111111-1111-4111-8111-111111111111", agentId,
    permission: "agent.run", status: "active", expiresAt: Math.floor(Date.now() / 1000) + 3600,
    capabilities: ["agent.output.final", "agent.execution.sandbox"], spentAtomics: "0", egressSubject: "s".repeat(43) };
  let grant = { ...base };
  const forwarded = [];
  let reply = () => Response.json({ data: [{ id: artifactId, turn_id: "turn-1", name: "model.stl" }] });
  const env = {
    CONNECT_STATE: { idFromName: name => name, get: () => ({ fetch: async input => {
      assert.equal(new URL(input).pathname, "/resolve-grant");
      return Response.json(new URL(input).searchParams.get("token") === token
        ? { grant, principal: { accountAddress, appId, appOrigin, grantId } } : {});
    } }) },
    ACCOUNTS: { fetch: async request => { forwarded.push(request.clone()); return reply(); } },
  };
  const context = { waitUntil() {} };
  const request = (suffix, method = "GET", body, headers = {}, agent = agentId) => new Request(
    `https://connect.example/v1/grants/${grantId}/agents/${agent}${suffix}`, { method,
      headers: { authorization: `Bearer ${token}`, origin: appOrigin, "x-nanocodex-app-id": appId,
        "content-type": "application/json", ...headers },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
  const run = (...args) => worker.fetch(request(...args), env, context);
  await t.test("exact artifact reads require final output and a live matching app grant", async () => {
    const response = await run("/artifacts?turn_id=turn-1");
    assert.equal(response.status, 200, await response.clone().text());
    assert.deepEqual(await response.json(), { data: [{ id: artifactId, turn_id: "turn-1", name: "model.stl" }] });
    const upstream = forwarded.at(-1);
    assert.equal(upstream.url, `https://nanocodex.internal/v1/agents/${agentId}/artifacts?turn_id=turn-1`);
    assert.equal(upstream.method, "GET");
    assert.equal(upstream.headers.has("authorization"), false);
    assert.equal(upstream.headers.get("x-nanocodex-connect-grant-id"), grantId);
    const count = forwarded.length;
    for (const capabilities of [[], ["agent.output.actions"], ["agent.history.read", "agent.trace.read"]]) {
      grant = { ...base, capabilities };
      for (const suffix of ["/artifacts?turn_id=turn-1", contentPath]) {
        assert.equal((await run(suffix)).status, 403, `${suffix}: ${capabilities}`);
      }
    }
    for (const changed of [{ status: "revoked" }, { expiresAt: 1 }]) {
      grant = { ...base, ...changed };
      assert.ok((await run(contentPath)).status >= 400);
    }
    grant = { ...base };
    for (const headers of [{ authorization: `Bearer ${"u".repeat(43)}` }, { origin: "https://other.example" }, { "x-nanocodex-app-id": "other-app" }]) {
      assert.ok((await run(contentPath, "GET", undefined, headers)).status >= 400);
    }
    assert.equal((await run(contentPath, "GET", undefined, {}, "22222222-2222-4222-8222-222222222222")).status, 403);
    for (const [suffix, method] of [
      ["/artifacts?turn_id=turn-1", "POST"], [contentPath, "HEAD"], [contentPath, "PUT"],
      ["/artifacts", "GET"], ["/artifacts?turn_id=", "GET"],
      ["/artifacts?turn_id=turn-1&turn_id=turn-2", "GET"], ["/artifacts?turn_id=turn-1&path=/brain/secret", "GET"],
      [`${contentPath}?path=/brain/secret`, "GET"], ["/artifacts/", "GET"],
      [`${contentPath}/extra`, "GET"], ["/artifacts/arbitrary", "GET"],
    ]) assert.ok((await run(suffix, method)).status >= 400, `${method} ${suffix}`);
    assert.equal(forwarded.length, count, "rejected requests never reach account storage");
  });
  await t.test("artifact content keeps bytes, media type and safe download headers", async () => {
    grant = { ...base };
    for (const [contentType, bytes] of [
      ["application/octet-stream", Uint8Array.from([0, 255, 13, 10, 128])],
      ["application/json", new TextEncoder().encode('{ "type": "turn_completed", "final_message": "data", "input": "keep" }\n')],
      ["text/event-stream", new TextEncoder().encode('data: {"type":"turn_accepted","input":"keep"}\n\n')],
    ]) {
      const headers = { "content-type": contentType, "content-length": String(bytes.length),
        etag: '"artifact-sha256"', "cache-control": "no-store", "x-content-type-options": "nosniff",
        "content-disposition": 'attachment; filename="model.stl"', "set-cookie": "private=secret", "x-internal-secret": "secret" };
      reply = () => new Response(bytes, { headers });
      const response = await run(contentPath);
      assert.equal(response.status, 200);
      assert.deepEqual(new Uint8Array(await response.arrayBuffer()), bytes);
      for (const name of ["content-type", "content-length", "etag", "cache-control", "x-content-type-options", "content-disposition"]) {
        assert.equal(response.headers.get(name), headers[name], name);
      }
      assert.equal(response.headers.has("set-cookie"), false);
      assert.equal(response.headers.has("x-internal-secret"), false);
      for (const name of ["etag", "content-disposition"]) {
        assert.ok(response.headers.get("access-control-expose-headers").split(/,\s*/).includes(name));
      }
    }
  });
  await t.test("input PUT preserves bounded JSON and only grant-derived sandbox authority", async () => {
    const body = { data_base64: "c29saWQgbW9kZWw=", sha256: "b".repeat(64) };
    reply = () => Response.json({ path: `/brain/connect/${grantId}${inputPath}` }, { status: 201 });
    for (const capabilities of [base.capabilities, ["agent.output.final"]]) {
      grant = { ...base, capabilities };
      const response = await run(inputPath, "PUT", body, {
        "x-nanocodex-connect-sandbox-execution": "true", "x-nanocodex-connect-user": "forged" });
      assert.equal(response.status, 201);
      const upstream = forwarded.at(-1);
      assert.equal(upstream.url, `https://nanocodex.internal/v1/agents/${agentId}${inputPath}`);
      assert.equal(upstream.method, "PUT");
      assert.deepEqual(await upstream.json(), body);
      assert.equal(upstream.headers.get("x-nanocodex-connect-user"), base.brokerUserId);
      assert.equal(upstream.headers.get("x-nanocodex-connect-sandbox-execution"),
        capabilities.includes("agent.execution.sandbox") ? "true" : null);
    }
    reply = () => Response.json({ error: "sandbox_execution_required" }, { status: 403 });
    assert.equal((await run(inputPath, "PUT", body)).status, 403, "managed input rejection passes through");
  });
});
