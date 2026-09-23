import assert from "node:assert/strict";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { Miniflare } from "miniflare";

// Distinct workerd Workers have separate JS globals/L1s and share the real Cache API.
// All data and identities are synthetic; no production binding or network is used.
const source = `
import { DurableObject } from "cloudflare:workers";
import { AsyncLocalStorage } from "node:async_hooks";
import { AccountCatalogCache } from "./src/account-catalog.ts";
import { cachedAccountMetadata, metadataCacheKey } from "../egress/src/metadata-cache.ts";
export class Owner extends DurableObject {}
export class OtherOwner extends DurableObject {}
export class VaultOwner extends DurableObject {}
let isolate, environment, logicalNow;
const requestAsyncLocalStorage = new AsyncLocalStorage();
const pendingWrites = new Set();
const clock = Date.now;
Date.now = () => logicalNow ?? clock();
const l1 = new AccountCatalogCache();
async function read(owner, component, options, partition = "primary") {
  const namespace = component === "vault" ? environment.VAULT : partition === "other" ? environment.OTHER : environment.OWNER;
  const ownerId = namespace.idFromName(owner).toString();
  const ctx = requestAsyncLocalStorage.getStore();
  if (!ctx) throw new Error("missing fixture request context");
  return cachedAccountMetadata(ownerId, component, options, async () => {
    const response = await environment.LIVE.fetch("https://fixture.test/" + owner + "/" + component);
    return { status: response.status, data: await response.json() };
  }, ctx);
}
// AccountCatalogCache keys L1 by this stable per-isolate binding identity.
const broker = { fetch() { throw new Error("unexpected live HTTP"); }, readAccountDiscovery: read };
export default {
  async fetch(request, env, ctx) {
    environment = env;
    isolate ??= crypto.randomUUID();
    const input = await request.json();
    if (input.mode === "drain") {
      await Promise.all(pendingWrites);
      return new Response(null, { status: 204 });
    }
    const cacheCtx = {
      waitUntil(promise) {
        const tracked = promise.finally(() => pendingWrites.delete(tracked));
        pendingWrites.add(tracked);
        ctx.waitUntil(tracked);
      },
    };
    return requestAsyncLocalStorage.run(cacheCtx, async () => {
      logicalNow = input.now;
      const owner = input.owner ?? "synthetic-owner";
      const authorityKey = input.authority ?? "synthetic-authority";
      const component = input.component ?? "catalog";
      if (input.mode === "raw") {
        const snapshot = await read(owner, component, { authorityKey, reload: input.reload }, input.partition);
        return Response.json({ isolate, snapshot });
      }
      if (input.mode === "namespaces") {
        const key = await metadataCacheKey(env.OWNER.idFromName(owner).toString(), component, authorityKey);
        return Response.json({ defaultHit: Boolean(await caches.default.match(key)), otherHit: Boolean(await (await caches.open("other-cache")).match(key)) });
      }
      if (input.reload) l1.invalidate();
      const [catalog, vault] = await Promise.all([l1.get(broker, owner, authorityKey), l1.vault(broker, owner, authorityKey)]);
      return Response.json({ isolate, catalog, vault });
    });
  }
}`;

async function bundled() {
  const result = await build({ stdin: { contents: source, resolveDir: fileURLToPath(new URL("..", import.meta.url)) },
    bundle: true, write: false, format: "esm", target: "es2022", platform: "browser", external: ["cloudflare:workers", "node:*"] });
  return result.outputFiles[0].text;
}

test("two distinct workerd isolates share L2 with private partitions and bounded cross-isolate refresh staleness", { timeout: 30_000 }, async () => {
  let revision = 1;
  let calls = 0;
  let raceStarted, releaseRace;
  const started = new Promise(resolve => { raceStarted = resolve; });
  const gate = new Promise(resolve => { releaseRace = resolve; });
  let holdRace = true;
  const live = async request => {
    calls++;
    const [owner, component] = new URL(request.url).pathname.slice(1).split("/");
    const observed = revision;
    if (owner === "race" && component === "catalog" && holdRace) {
      holdRace = false; raceStarted(); await gate;
    }
    return Response.json(component === "vault"
      ? [{ id: "V".repeat(32), kind: "api_key", name: "revision-" + observed, created_at: 1 }]
      : { connectors: {}, mcp_connections: [{ id: "M".repeat(43), name: "revision-" + observed, status: "connected" }] });
  };
  const script = await bundled();
  const mf = new Miniflare({ workers: ["a", "b"].map(name => ({ name, script, modules: true, compatibilityDate: "2026-07-29", compatibilityFlags: ["nodejs_compat"],
    serviceBindings: { LIVE: live }, durableObjects: {
      OWNER: { className: "Owner", scriptName: "a" }, OTHER: { className: "OtherOwner", scriptName: "a" }, VAULT: { className: "VaultOwner", scriptName: "a" },
    } })) });
  try {
    const a = await mf.getWorker("a"), b = await mf.getWorker("b");
    const t0 = Date.now();
    const send = async (worker, input = {}) => {
      const response = await worker.fetch("https://discovery.test", { method: "POST", body: JSON.stringify({ now: t0, ...input }) });
      assert.equal(response.status, 200, await response.clone().text());
      return response.json();
    };
    const drain = async worker => {
      const response = await worker.fetch("https://discovery.test", { method: "POST", body: JSON.stringify({ mode: "drain" }) });
      assert.equal(response.status, 204);
    };
    const first = await send(a);
    assert.equal(calls, 2);
    // Only persistence-dependent assertions drain the real waitUntil writes.
    await drain(a);
    const promoted = await send(b, { now: t0 + 899_000 });
    assert.notEqual(first.isolate, promoted.isolate);
    assert.deepEqual(promoted.catalog, first.catalog);
    assert.equal(calls, 2, "second isolate must hit L2 for both components");
    await send(b, { now: t0 + 900_001 });
    assert.equal(calls, 4, "L1 promotion must not restart the original 15-minute lifetime");

    const common = { mode: "raw", owner: "partition" };
    const original = await send(a, common);
    await drain(a);
    const hit = await send(b, common);
    assert.deepEqual(hit.snapshot, original.snapshot);
    assert.equal(calls, 5);
    await send(b, { ...common, authority: "different" });
    await send(b, { ...common, partition: "other" });
    await send(b, { ...common, owner: "different-owner" });
    await send(b, { ...common, component: "vault" });
    assert.equal(calls, 9, "authority, namespace, owner and component must each miss");
    assert.deepEqual(await send(b, { ...common, mode: "namespaces" }), { defaultHit: false, otherHit: false });

    const oldPending = send(a, { owner: "race" });
    await started;
    revision = 2;
    const fresh = await send(a, { owner: "race", reload: true, now: t0 + 1000 });
    assert.equal(fresh.catalog.mcp_connections[0].name, "revision-2");
    assert.equal(fresh.vault[0].name, "revision-2");
    await drain(a);
    releaseRace();
    const old = await oldPending;
    assert.equal(old.catalog.mcp_connections[0].name, "revision-1");
    await drain(a);
    const beforeLocal = calls;
    const local = await send(a, { owner: "race", now: t0 + 2000 });
    assert.equal(calls, beforeLocal, "local refreshed L1 must avoid backend reads");
    assert.equal(local.catalog.mcp_connections[0].name, "revision-2", "old pending promise must not replace local refreshed L1");
    const other = await send(b, { owner: "race", now: t0 + 2000 });
    assert.equal(other.catalog.mcp_connections[0].name, "revision-1", "late put permits bounded stale reads in another isolate");
    const stale = await send(b, { owner: "race", mode: "raw", now: t0 + 2000 });
    assert.equal(stale.snapshot.expiresAt, t0 + 900_000);
    const expired = await send(b, { owner: "race", now: t0 + 900_001 });
    assert.equal(expired.catalog.mcp_connections[0].name, "revision-2", "cross-isolate stale data must expire at its original deadline");
    console.log("DISCOVERY_ISOLATE_EVIDENCE", JSON.stringify({ distinctIsolates: first.isolate !== promoted.isolate,
      promotionBackendReads: 0, partitions: ["owner", "DO namespace", "authority", "component", "cache namespace"],
      promotionKeepsOriginalTTL: true, explicitReloadBothFresh: true, oldPendingCannotReplaceLocalFresh: true,
      crossIsolateLateWriteStalenessObserved: true, oldExpiryRetained: true, expiredStaleRereadsBackend: true, backendReads: calls }));
  } finally { releaseRace(); await mf.dispose(); }
});
