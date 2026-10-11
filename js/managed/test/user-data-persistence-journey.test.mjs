import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { mkdir, writeFile, rm } from "node:fs/promises";
import { build } from "esbuild";
import { Miniflare } from "miniflare";
import { fetch } from "./support/miniflare-fetch.mjs";

// Only initial identity enrollment is synthetic. API-key issuance/validation,
// account proxy, storage routing, SQLite, R2, and workerd restarts are real.
const source = `
import { UserAccount, Organization, ApiKeyRecord, NonceStorage, ensureAccount, createApiKey } from "./src/account-auth.ts";
import { routeUserDataRequest } from "./src/user-data-route.ts";
import { routeManaged } from "../account/worker/managedProxy.ts";
export { UserDataScope } from "./src/user-data-scope.ts";
export { UserAccount, Organization, ApiKeyRecord, NonceStorage };
export default { async fetch(request, env) {
  const url = new URL(request.url);
  if (env.EDGE) return await routeManaged(request, env, url) ?? new Response(null, {status:404});
  if (url.pathname === "/__fixture") {
    const input = await request.json();
    await ensureAccount(env, input.user, true);
    const stub = env.NANOCODEX_USERS.getByName(input.user);
    const auth = await (await stub.fetch("https://user.internal/authorization")).json();
    return Response.json(await createApiKey(env, {
      kind:"api_key", userId:input.user, ...auth.grant,
      subjectId:"api_key:"+input.user, credentialId:"fixture",
      capabilities:input.capabilities ?? auth.grant.capabilities,
    }, "synthetic-personal-storage"));
  }
  return await routeUserDataRequest(request, env, url) ?? new Response(null, {status:404});
}};
`;

test("personal storage over account HTTP: real API keys, tenant isolation and durable restart", { timeout: 90_000 }, async () => {
  const trace = [];
  const output = new URL("../../../output/user-data/", import.meta.url);
  const persistence = fileURLToPath(new URL("store-" + crypto.randomUUID(), output));
  const bundled = await build({
    stdin: { contents: source, resolveDir: fileURLToPath(new URL("..", import.meta.url)) },
    bundle: true, write: false, format: "esm", target: "es2022", platform: "browser",
    external: ["cloudflare:workers", "node:*"],
    alias: { "node-rsa": "./node_modules/nanocodex/tools/browser/unsupportedNodeRsa.mjs" },
  });
  const script = bundled.outputFiles[0].text;
  const options = {
    durableObjectsPersist: persistence + "/sqlite", r2Persist: persistence + "/r2",
    workers: [
      { name: "edge", script, modules: true, compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"],
        bindings: { EDGE: true }, serviceBindings: { NANOCODEX_BACKEND: "managed" } },
      { name: "managed", script, modules: true, compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"],
        durableObjects: {
          NANOCODEX_AUTH: { className: "NonceStorage", useSQLite: true },
          NANOCODEX_USERS: { className: "UserAccount", useSQLite: true },
          NANOCODEX_ORGANIZATIONS: { className: "Organization", useSQLite: true },
          NANOCODEX_API_KEYS: { className: "ApiKeyRecord", useSQLite: true },
          NANOCODEX_USER_DATA: { className: "UserDataScope", useSQLite: true },
        }, r2Buckets: ["NANOCODEX_USER_DATA_OBJECTS"] },
    ],
  };
  let mf = new Miniflare(options);
  try {
    let base = await mf.ready;
    const backend = await mf.getWorker("managed");
    async function issue(user, capabilities) {
      const response = await backend.fetch("https://fixture.test/__fixture", { method: "POST", body: JSON.stringify({ user, capabilities }) });
      assert.equal(response.status, 200, await response.clone().text());
      return (await response.json()).token;
    }
    const aliceId = crypto.randomUUID(), bobId = crypto.randomUUID();
    const alice = await issue(aliceId);
    const bob = await issue(bobId);
    const read = await issue(aliceId, ["data:read"]);
    const write = await issue(aliceId, ["data:write"]);
    const legacy = await issue(aliceId, ["agents:read", "agents:write"]);
    async function call(input, expected = 200, token = alice, headers = {}) {
      const response = await fetch(new URL("/v1/data", base), { method: "POST", headers: {
        authorization: "Bearer " + token, "content-type": "application/json", ...headers,
      }, body: JSON.stringify(input) });
      const result = await response.json();
      trace.push({ operation: input.operation, expected, observed: response.status,
        result: JSON.stringify(result).slice(0, 1800) });
      assert.equal(response.status, expected, JSON.stringify(result));
      assert.equal(response.headers.get("cache-control"), "no-store");
      assert.ok(!JSON.stringify(result).includes("r2_key"));
      return result;
    }
    const key = "com.example/persistent";
    const put = { operation: "document_put", key, value: { owner: "alice", nested: [null, false, "é"] } };
    const object = { operation: "object_put", key, content: "/wAB", encoding: "base64", content_type: "application/octet-stream", metadata: { fixture: true } };
    const series = { operation: "timeseries_write", series: key, points: [{ timestamp_ms: 0, value: 0 }, { timestamp_ms: 1000, value: 2 }] };
    await call(put);
    await call(object);
    await call(series);
    await call(put, 403, read);
    await call({ operation: "document_get", key }, 403, write);
    await call({ operation: "document_get", key }, 403, legacy);
    await call({ operation: "document_get", key }, 401, "ncx_live_invalid");
    for (const operation of ["document_get", "object_get", "document_delete", "object_delete"]) {
      await call({ operation, key }, 404, bob, { "x-nanocodex-user-id": aliceId });
    }
    for (const [operation, field] of [["document_list", "documents"], ["object_list", "objects"], ["timeseries_list", "series"]]) {
      assert.deepEqual((await call({ operation }, 200, bob))[field], []);
    }
    assert.deepEqual((await call({ operation: "timeseries_query", series: key }, 200, bob)).points, []);
    await call({ ...put, value: "bob" }, 200, bob);
    await call({ ...object, content: "Ym9i" }, 200, bob);
    await call({ ...series, points: [{ timestamp_ms: 0, value: 99 }] }, 200, bob);
    assert.deepEqual((await call({ operation: "document_get", key }, 200, read)).document.value, put.value);
    assert.equal((await call({ operation: "object_get", key, encoding: "base64" })).object.content, object.content);
    assert.deepEqual((await call({ operation: "timeseries_query", series: key })).points.map(p => p.value), [0, 2]);
    // One MiB is supported at the boundary, including base64 transport overhead.
    const bytes = Buffer.alloc(1024 * 1024, 255).toString("base64");
    await call({ ...object, key: key + "/max", content: bytes });
    assert.equal((await call({ operation: "object_get", key: key + "/max", encoding: "base64" })).object.content, bytes);
    await call({ ...object, key: key + "/empty", content: "" });
    assert.equal((await call({ operation: "object_get", key: key + "/empty" })).object.content, "");
    assert.deepEqual(await call({ operation: "object_delete", key: key + "/empty" }, 200, write), {
      operation: "object_delete", object: { key: key + "/empty", version: 1 }, deleted: true,
    });
    // Restart all Workers against the same on-disk SQLite and R2 stores.
    await mf.dispose();
    mf = new Miniflare(options);
    base = await mf.ready;
    trace.push({ case: "workerd_restart", expected: "same data and credentials", observed: "new runtime" });
    assert.deepEqual((await call({ operation: "document_get", key })).document.value, put.value);
    assert.equal((await call({ operation: "object_get", key, encoding: "base64" })).object.content, object.content);
    assert.deepEqual((await call({ operation: "timeseries_query", series: key })).points.map(p => p.value), [0, 2]);
    assert.equal((await call({ ...object, key: key + "/empty", content: "" })).object.version, 2);
    assert.equal((await call({ operation: "document_get", key }, 200, bob)).document.value, "bob");
    await call({ ...put, value: "updated", if_version: 1 });
    await call({ ...put, value: "stale", if_version: 1 }, 409);
    await call({ operation: "sql", query: "SELECT * FROM user_documents" }, 400);
  } finally {
    await mkdir(output, { recursive: true });
    await writeFile(new URL("persistence-http-trace.json", output), JSON.stringify({
      command: "pnpm --dir js/managed test:user-data", expected: "account proxy, real key capabilities, isolation, max-size binary, restart persistence and tombstone versions", trace,
    }, null, 2));
    await mf.dispose();
    await rm(persistence, { recursive: true, force: true });
  }
});
