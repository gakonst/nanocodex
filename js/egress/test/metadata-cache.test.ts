import { env, exports } from "cloudflare:workers";
import { createExecutionContext, SELF, waitOnExecutionContext } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import Egress from "../src/egress";
import type { EgressEnv } from "../src/egress";
import { cachedAccountMetadata as readMetadata, metadataCacheKey, METADATA_CACHE_NAME, METADATA_TTL_MS, safeMetadata, SourceCatalogSnapshots } from "../src/metadata-cache";

const catalog = { connectors: { github: { connected: false, connections: [] } }, mcp_connections: [] };
const service = (exports as unknown as { default: Egress }).default;
const runtime = env as unknown as EgressEnv;
const options = { authorityKey: "synthetic-epoch" };
const vault = [{ id: "V".repeat(32), kind: "api_key", name: "Synthetic API", created_at: 1 }];

// Sequential cache assertions wait for the explicitly registered background work.
async function cachedAccountMetadata(
  ownerId: string,
  component: Parameters<typeof readMetadata>[1],
  options: Parameters<typeof readMetadata>[2],
  live: Parameters<typeof readMetadata>[3],
) {
  const ctx = createExecutionContext();
  try { return await readMetadata(ownerId, component, options, live, ctx); }
  finally { await waitOnExecutionContext(ctx); }
}

async function expectStoredMetadata(user: string, component: "catalog" | "vault", expected: unknown) {
  const namespace = component === "catalog" ? runtime.USER_CONNECTORS : runtime.USER_CREDENTIALS;
  const key = await metadataCacheKey(namespace.idFromName(user).toString(), component, options.authorityKey);
  await vi.waitFor(async () => {
    const stored = await (await caches.open(METADATA_CACHE_NAME)).match(key);
    expect(stored).toBeDefined();
    expect(await stored!.json()).toEqual(expected);
  });
}

describe("bounded metadata Cache API", () => {
  it("observes hit, miss, reload and unavailable with only bounded content-free fields", async () => {
    const owner = "private-observation-owner", authorityKey = "private-observation-authority";
    const privateCatalog = { connectors: {}, mcp_connections: [{ id: "P".repeat(43), name: "private-observation-data", status: "connected" }] };
    const live = vi.fn(async () => ({ status: 200, data: privateCatalog }));
    const log = vi.spyOn(console, "log").mockImplementation(() => {});
    try {
      await cachedAccountMetadata(owner, "catalog", { authorityKey }, live);
      await cachedAccountMetadata(owner, "catalog", { authorityKey }, live);
      await cachedAccountMetadata(owner, "catalog", { authorityKey, reload: true }, live);
      const open = vi.spyOn(caches, "open").mockRejectedValueOnce(new Error("private-observation-error"));
      try { await cachedAccountMetadata(owner, "catalog", { authorityKey }, live); }
      finally { open.mockRestore(); }
      expect(live).toHaveBeenCalledTimes(3);
      const events = log.mock.calls.map(([event]) => event);
      expect(events.map(event => event.cache_state)).toEqual(["miss", "hit", "reload", "unavailable"]);
      expect(events.map(event => event.write_scheduled)).toEqual([true, false, true, false]);
      const expectedKeys = ["type", "component", "cache_state", "cache_read_ms", "backend_ms", "write_scheduled", "remaining_ttl_ms"].sort();
      for (const event of events) {
        expect(Object.keys(event).sort()).toEqual(expectedKeys);
        expect(event.type).toBe("egress.metadata_cache"); expect(event.component).toBe("catalog");
        expect(typeof event.write_scheduled).toBe("boolean");
        for (const key of ["cache_read_ms", "backend_ms", "remaining_ttl_ms"]) {
          expect(Number.isSafeInteger(event[key])).toBe(true); expect(event[key]).toBeGreaterThanOrEqual(0);
        }
        expect(event.remaining_ttl_ms).toBeLessThanOrEqual(METADATA_TTL_MS);
      }
      expect(events[1]).toMatchObject({ backend_ms: 0, write_scheduled: false });
      const key = await metadataCacheKey(owner, "catalog", authorityKey);
      expect(JSON.stringify(events)).not.toContain("private-observation");
      expect(JSON.stringify(events)).not.toContain(key);
      log.mockImplementation(() => { throw new Error("synthetic logger failure"); });
      expect((await cachedAccountMetadata(owner, "catalog", { authorityKey }, live)).data).toEqual(privateCatalog);
      await expect(cachedAccountMetadata("failed-observation", "vault", { authorityKey }, async () => { throw new Error("backend failure preserved"); })).rejects.toThrow("backend failure preserved");
    } finally { log.mockRestore(); }
  });

  it("uses real cache entries partitioned by owner namespace, authority and component", async () => {
    const owner = runtime.USER_CONNECTORS.idFromName("cache-partitions").toString();
    const otherBinding = runtime.USER_CREDENTIALS.idFromName("cache-partitions").toString();
    expect(otherBinding).not.toBe(owner);
    const live = vi.fn(async () => ({ status: 200, data: catalog }));
    const first = await cachedAccountMetadata(owner, "catalog", options, live);
    expect(await cachedAccountMetadata(owner, "catalog", options, live)).toEqual(first);
    expect(live).toHaveBeenCalledOnce();
    await cachedAccountMetadata(otherBinding, "catalog", options, live);
    await cachedAccountMetadata(owner, "catalog", { authorityKey: "different-epoch" }, live);
    await cachedAccountMetadata(runtime.USER_CONNECTORS.idFromName("different-owner").toString(), "catalog", options, live);
    const vaultLive = vi.fn(async () => ({ status: 200, data: vault }));
    await cachedAccountMetadata(owner, "vault", options, vaultLive);
    expect(live).toHaveBeenCalledTimes(4); expect(vaultLive).toHaveBeenCalledOnce();
    const key = await metadataCacheKey(owner, "catalog", options.authorityKey);
    expect(key).not.toContain(owner); expect(key).not.toContain(options.authorityKey);
    expect(await caches.default.match(key)).toBeUndefined();
    expect(await (await caches.open("unrelated-private-cache")).match(key)).toBeUndefined();
  });

  it("reload bypasses L2 and late writes retain the old original expiry", async () => {
    let now = Date.now();
    const clock = vi.spyOn(Date, "now").mockImplementation(() => now);
    const owner = "late-write-owner";
    let release!: (result: { status: number; data: unknown }) => void;
    const live = vi.fn(() => new Promise<{ status: number; data: unknown }>(resolve => { release = resolve; }));
    try {
      const old = cachedAccountMetadata(owner, "catalog", options, live);
      await vi.waitFor(() => expect(live).toHaveBeenCalledOnce());
      const originalExpiry = now + METADATA_TTL_MS;
      now += 2000;
      const fresh = { ...catalog, mcp_connections: [{ id: "M".repeat(43), name: "Fresh", status: "connected" }] };
      expect((await cachedAccountMetadata(owner, "catalog", { ...options, reload: true }, async () => ({ status: 200, data: fresh }))).data).toEqual(fresh);
      release({ status: 200, data: catalog });
      expect((await old).expiresAt).toBe(originalExpiry);
      const cached = await (await caches.open(METADATA_CACHE_NAME)).match(await metadataCacheKey(owner, "catalog", options.authorityKey));
      expect((await cached!.json<{ expiresAt: number }>()).expiresAt).toBe(originalExpiry);
      expect(cached!.headers.get("cache-control")).toBe("max-age=898");
      now = originalExpiry;
      const reread = vi.fn(async () => ({ status: 200, data: fresh }));
      expect((await cachedAccountMetadata(owner, "catalog", options, reread)).data).toEqual(fresh);
      expect(reread).toHaveBeenCalledOnce();
    } finally { clock.mockRestore(); }
  });

  it("treats malformed, expired, wrong-version, secret-bearing and oversized stored entries as misses", async () => {
    const cache = await caches.open(METADATA_CACHE_NAME);
    const owner = "corrupt-owner", key = await metadataCacheKey(owner, "catalog", options.authorityKey);
    const good = { schema: 1, status: 200, expiresAt: Date.now() + 100_000, data: catalog };
    const live = vi.fn(async () => ({ status: 200, data: catalog }));
    const bodies = ["{", JSON.stringify({ ...good, schema: 2 }), JSON.stringify({ ...good, status: 503 }),
      JSON.stringify({ ...good, expiresAt: Date.now() - 1 }), JSON.stringify({ ...good, expiresAt: Date.now() + 2 * METADATA_TTL_MS }),
      JSON.stringify({ ...good, data: { ...catalog, access_token: "synthetic-secret" } }), " ".repeat(256 * 1024 + 1)];
    for (const body of bodies) {
      await cache.put(key, new Response(body, { headers: { "cache-control": "max-age=900" } }));
      expect((await cachedAccountMetadata(owner, "catalog", options, live)).data).toEqual(catalog);
    }
    expect(live).toHaveBeenCalledTimes(bodies.length);
  });

  it("does not cache backend failures or invalid and oversized live data", async () => {
    for (const data of [{ ...catalog, token: "synthetic-secret" }, { ...catalog, mcp_connections: Array(257).fill({ id: "M".repeat(43), name: "name", status: "connected" }) },
      { connectors: Object.fromEntries(Array.from({ length: 32 }, (_, i) => [`provider${i}`, { connected: true, connections: Array(100).fill({ id: "C".repeat(43), label: "L".repeat(512), account_id: "A".repeat(512), capabilities: ["github"] }) }])), mcp_connections: [] }]) {
      const live = vi.fn(async () => ({ status: 200, data }));
      await cachedAccountMetadata("invalid-live", "catalog", options, live);
      await cachedAccountMetadata("invalid-live", "catalog", options, live);
      expect(live).toHaveBeenCalledTimes(2);
    }
    const live = vi.fn(async () => ({ status: 503, data: null }));
    await cachedAccountMetadata("failed-live", "catalog", options, live); await cachedAccountMetadata("failed-live", "catalog", options, live);
    expect(live).toHaveBeenCalledTimes(2);
    await expect(cachedAccountMetadata("throw-live", "catalog", options, async () => { throw new Error("backend failed"); })).rejects.toThrow("backend failed");
    expect(safeMetadata("vault", [{ ...vault[0], api_key: "synthetic-secret" }])).toBe(false);
    expect(safeMetadata("vault", [{ ...vault[0], kind: "login", username: "synthetic", password: "synthetic" }])).toBe(false);
  });

  it.each(["open", "match", "put"] as const)("falls back to fresh backend metadata when cache %s fails", async operation => {
    const real = await caches.open(METADATA_CACHE_NAME);
    const live = vi.fn(async () => ({ status: 200, data: catalog }));
    const spy = vi.spyOn(caches, "open").mockImplementation(async () => {
      if (operation === "open") throw new Error("synthetic cache failure");
      return { match: async () => { if (operation === "match") throw new Error("synthetic match failure"); return undefined; },
        put: async () => { if (operation === "put") throw new Error("synthetic put failure"); }, delete: real.delete.bind(real) } as Cache;
    });
    try { expect((await cachedAccountMetadata(`failure-${operation}`, "catalog", options, live)).data).toEqual(catalog); expect(live).toHaveBeenCalledOnce(); }
    finally { spy.mockRestore(); }
  });
});

describe("discovery service RPC with real owning DOs", () => {
  it.each(["resolve", "reject"] as const)("returns the RPC result before a pending cache put can %s", async completion => {
    const real = await caches.open(METADATA_CACHE_NAME);
    const ctx = createExecutionContext();
    const schedule = vi.spyOn(ctx, "waitUntil");
    const rpc = new Egress(ctx, runtime);
    let now = Date.now();
    const originalExpiry = now + METADATA_TTL_MS;
    const clock = vi.spyOn(Date, "now").mockImplementation(() => now);
    let finish!: () => void, fail!: (error: Error) => void;
    const pending = new Promise<void>((resolve, reject) => { finish = resolve; fail = reject; });
    let stored!: Response;
    const put = vi.fn((_key: unknown, response: Response) => { stored = response; return pending; });
    const open = vi.spyOn(caches, "open").mockResolvedValue({
      match: async () => undefined, put, delete: real.delete.bind(real),
    } as Cache);
    const backend = vi.fn(async () => {
      now += 2000;
      return { schema: 1 as const, status: 200, data: catalog, expiresAt: originalExpiry };
    });
    const owner = vi.spyOn(runtime.USER_CONNECTORS, "getByName").mockReturnValue({
      readDiscoveryCatalog: backend,
    } as unknown as ReturnType<typeof runtime.USER_CONNECTORS.getByName>);
    const log = vi.spyOn(console, "log").mockImplementation(() => {});
    let timeout: ReturnType<typeof setTimeout> | undefined;
    const response = rpc.readAccountDiscovery("pending-put-owner", "catalog", options);
    try {
      // The write is still unresolved: an awaited put would fail this assertion.
      const received = await Promise.race([response, new Promise<never>((_, reject) => {
        timeout = setTimeout(() => reject(new Error("RPC waited for cache storage")), 1000);
      })]);
      clearTimeout(timeout);
      expect(received).toEqual({ schema: 1, status: 200, data: catalog, expiresAt: originalExpiry });
      expect(backend).toHaveBeenCalledOnce();
      expect(put).toHaveBeenCalledOnce();
      expect(schedule).toHaveBeenCalledOnce();
      expect(await stored.clone().json()).toEqual(received);
      expect(stored.headers.get("cache-control")).toBe("max-age=898");
      expect(stored.headers.get("expires")).toBe(new Date(originalExpiry).toUTCString());
      expect(log.mock.calls.map(([event]) => event)).toEqual([{
        type: "egress.metadata_cache", component: "catalog", cache_state: "miss",
        cache_read_ms: 0, backend_ms: 2000, write_scheduled: true, remaining_ttl_ms: 898_000,
      }]);
      let drained = false;
      const drain = waitOnExecutionContext(ctx).then(() => { drained = true; });
      await Promise.resolve();
      expect(drained).toBe(false);
      now += 20_000;
      if (completion === "reject") fail(new Error("private-cache-write-error"));
      else finish();
      await drain;
      expect((await response).expiresAt).toBe(originalExpiry);
      expect(log).toHaveBeenCalledOnce();
    } finally {
      clearTimeout(timeout);
      finish();
      await response;
      await waitOnExecutionContext(ctx);
      log.mockRestore(); owner.mockRestore(); open.mockRestore(); clock.mockRestore(); schedule.mockRestore();
    }
  });

  it("accepts and stores the actual connected-provider metadata schema", async () => {
    const user = "discovery-connected-owner";
    const post = (path: string, body: unknown) => SELF.fetch(`https://broker.internal/users/${user}${path}`, {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body),
    });
    const start = await post("/connectors/github", { redirect_uri: "https://nanocodex.test/v1/connectors/callback", return_to: "/agent" });
    expect(start.status).toBe(200);
    const state = new URL((await start.json<{ authorization_url: string }>()).authorization_url).searchParams.get("state");
    expect((await post("/connectors/github/callback", { code: "github-code", state })).status).toBe(200);
    const result = await service.readAccountDiscovery(user, "catalog", options);
    expect(result.data).toMatchObject({ connectors: { github: { connected: true, connections: [{ capabilities: ["github"] }] } } });
    await expectStoredMetadata(user, "catalog", result);
    expect(JSON.stringify(result)).not.toMatch(/access_token|refresh_token|github-connector-access/);
  });

  it("caches actual Google OAuth scopes while disconnected credentials stay unusable", async () => {
    const user = "discovery-google-owner", subject = "G".repeat(43);
    const control = (path: string, method = "GET", body?: unknown) => SELF.fetch(`https://broker.internal${path}`, {
      method, ...(body === undefined ? {} : { headers: { "content-type": "application/json" }, body: JSON.stringify(body) }),
    });
    const started = await control(`/users/${user}/connectors/google`, "POST", {
      redirect_uri: "https://nanocodex.test/v1/connectors/callback", return_to: "/agent",
    });
    expect(started.status).toBe(200);
    const state = new URL((await started.json<{ authorization_url: string }>()).authorization_url).searchParams.get("state");
    const callback = await control(`/users/${user}/connectors/google/callback`, "POST", { code: "google-alpha-code", state });
    expect(callback.status).toBe(200);
    const { connection_id: connection } = await callback.json<{ connection_id: string }>();
    const first = await service.readAccountDiscovery(user, "catalog", options);
    const expectedConnection = { id: connection, label: "alpha@example.test", account_id: "google-alpha-account",
      capabilities: ["gmail", "gdrive"], scopes: ["openid", "email", "https://mail.google.com/", "https://www.googleapis.com/auth/drive"] };
    expect(first.data).toMatchObject({ connectors: {
      gmail: { connected: true, connections: [expectedConnection] },
      gdrive: { connected: true, connections: [expectedConnection] },
    } });
    await expectStoredMetadata(user, "catalog", first);
    expect(JSON.stringify(first)).not.toMatch(/access_token|refresh_token|google-alpha-access|google-alpha-refresh/);
    expect((await control(`/subjects/${subject}`, "PUT", { user_id: user })).status).toBe(200);
    const headers = { authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "x-nanocodex-subject": subject,
      "x-nanocodex-connector-connection": connection };
    expect((await SELF.fetch("https://gmail.googleapis.com/gmail/v1/users/me/messages", { headers })).status).toBe(200);
    expect((await control(`/users/${user}/connectors/google/connections/${connection}`, "DELETE")).status).toBe(204);
    // Delete the exact colo entry: the real service->DO RPC must now use the
    // source snapshot, not fresh credentials or a renewed 15-minute deadline.
    const cache = await caches.open(METADATA_CACHE_NAME);
    const key = await metadataCacheKey(runtime.USER_CONNECTORS.idFromName(user).toString(), "catalog", options.authorityKey);
    expect(await cache.delete(key)).toBe(true);
    expect(await cache.match(key)).toBeUndefined();
    expect(await service.readAccountDiscovery(user, "catalog", options)).toEqual(first);
    await expectStoredMetadata(user, "catalog", first);
    expect((await service.readAccountCatalog(user)).catalog).toMatchObject({ connectors: { gmail: { connected: false } } });
    expect(await (await control(`/users/${user}/catalog`)).json()).toMatchObject({ connectors: { gmail: { connected: false } } });
    expect((await service.readAccountDiscovery(user, "catalog", { authorityKey: "different-epoch" })).data)
      .toMatchObject({ connectors: { gmail: { connected: false } } });
    expect((await service.readAccountDiscovery("discovery-other-google-owner", "catalog", options)).data)
      .toMatchObject({ connectors: { gmail: { connected: false } } });
    const denied = await SELF.fetch("https://gmail.googleapis.com/gmail/v1/users/me/messages", { headers });
    expect(denied.status).toBe(404);
    expect(await denied.json()).toEqual({ error: "connector_connection_not_found" });
    const refreshed = await service.readAccountDiscovery(user, "catalog", { ...options, reload: true });
    expect(refreshed.data).toMatchObject({ connectors: { gmail: { connected: false, connections: [] }, gdrive: { connected: false, connections: [] } } });
    console.log("GOOGLE_DISCOVERY_JOURNEY_EVIDENCE", JSON.stringify({ oauthConnected: true, publicScopesCached: true,
      forcedL2MissSourceHitOriginalExpiry: true, liveCatalogDisconnected: true, ownerAndAuthorityIsolated: true, liveInvocationDenied: true, explicitReloadDisconnected: true }));
  });

  it("opts in explicitly, reloads backend changes, and leaves the default RPC and HTTP live", async () => {
    const user = "discovery-real-owner", id = "D".repeat(43);
    const first = await service.readAccountDiscovery(user, "catalog", options);
    expect(first).toMatchObject({ schema: 1, status: 200, data: { mcp_connections: [] } });
    await expectStoredMetadata(user, "catalog", first);
    const put = await SELF.fetch(`https://broker.internal/users/${user}/mcp-connections/${id}`, { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify({ endpoint: "https://mcp.linear.app/mcp", name: "Synthetic new MCP" }) });
    expect(put.status).toBe(200);
    expect(await service.readAccountDiscovery(user, "catalog", options)).toEqual(first);
    expect((await service.readAccountCatalog(user)).catalog).toMatchObject({ mcp_connections: [{ id }] });
    expect(await (await SELF.fetch(`https://broker.internal/users/${user}/catalog`)).json()).toMatchObject({ mcp_connections: [{ id }] });
    expect((await service.readAccountDiscovery(user, "catalog", { ...options, reload: true })).data).toMatchObject({ mcp_connections: [{ id }] });
    expect((await SELF.fetch("https://account-discovery.internal/v1/anything")).status).toBe(403);
    for (const args of [[user, "machines", options], [user, "vault", {}], [user, "vault", { authorityKey: "a".repeat(4097) }], ["../other", "catalog", options]]) {
      expect(await service.readAccountDiscovery(...args as [unknown, unknown, unknown])).toMatchObject({ status: 400 });
    }
  });

  it("caches detached safe vault metadata, preserving safe PII but never secrets", async () => {
    const user = "discovery-vault-owner";
    expect((await SELF.fetch(`https://broker.internal/users/${user}/credentials/vault/login`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ name: "Synthetic login", username: "person@example.test", password: "synthetic-private-password" }) })).status).toBe(201);
    const first = await service.readAccountDiscovery(user, "vault", options);
    expect(first).toMatchObject({ status: 200, data: [{ username: "person@example.test" }] });
    await expectStoredMetadata(user, "vault", first);
    expect(JSON.stringify(first)).not.toMatch(/password|synthetic-private/);
    expect(await service.readAccountDiscovery(user, "vault", options)).toEqual(first);
    expect(Object.getOwnPropertySymbols(first.data as object)).toEqual([]);
  });
});

// The public journeys above exercise both RPC legs. These narrow checks cover
// pending completion order and admission boundaries that have no HTTP control.
describe("source snapshot boundaries", () => {
  it.each(["resolve", "reject"] as const)("coalesces pending reads and preserves reload after an older %s", async completion => {
    const snapshots = new SourceCatalogSnapshots();
    let finish!: (value: { status: number; data: unknown; cacheable: boolean }) => void;
    let fail!: (reason: Error) => void;
    const pending = new Promise<{ status: number; data: unknown; cacheable: boolean }>((resolve, reject) => { finish = resolve; fail = reject; });
    const fresh = vi.fn(() => pending);
    const old = snapshots.read(options, fresh);
    const coalesced = snapshots.read(options, fresh);
    const oldOutcome = Promise.allSettled([old, coalesced]);
    const newer = { connectors: {}, mcp_connections: [{ id: "N".repeat(43), name: "New generation", status: "connected" }] };
    const refreshed = await snapshots.read({ ...options, reload: true }, async () => ({ status: 200, data: newer, cacheable: true }));
    expect(fresh).toHaveBeenCalledOnce();
    if (completion === "resolve") finish({ status: 200, data: catalog, cacheable: true });
    else fail(new Error("old failure"));
    const outcomes = await oldOutcome;
    expect(outcomes.map(value => value.status)).toEqual([completion === "resolve" ? "fulfilled" : "rejected", completion === "resolve" ? "fulfilled" : "rejected"]);
    const unexpected = vi.fn(async () => { throw new Error("replaced reload"); });
    expect(await snapshots.read(options, unexpected)).toEqual(refreshed);
    expect(unexpected).not.toHaveBeenCalled();
    (refreshed.data as typeof newer).mcp_connections[0].name = "mutated caller";
    expect((await snapshots.read(options, unexpected)).data).toEqual(newer);
  });

  it("bounds authority slots, retains original expiry, and retries unsafe/degraded/error results", async () => {
    const snapshots = new SourceCatalogSnapshots();
    let now = Date.now();
    const clock = vi.spyOn(Date, "now").mockImplementation(() => now);
    const fresh = vi.fn(async () => ({ status: 200, data: catalog, cacheable: true }));
    try {
      const first = await snapshots.read(options, fresh);
      now += 1000;
      expect(await snapshots.read(options, fresh)).toEqual(first);
      expect(fresh).toHaveBeenCalledOnce();
      now = first.expiresAt;
      expect((await snapshots.read(options, fresh)).expiresAt).toBe(now + METADATA_TTL_MS);
      expect(fresh).toHaveBeenCalledTimes(2);
      for (let i = 0; i < 16; i++) await snapshots.read({ authorityKey: `slot-${i}` }, fresh);
      await snapshots.read(options, fresh);
      expect(fresh).toHaveBeenCalledTimes(19);
      // 256 individually safe MCP rows can still exceed the envelope byte limit.
      const oversized = { connectors: {}, mcp_connections: Array.from({ length: 256 }, () => ({
        id: "O".repeat(43), name: "\\".repeat(512), status: "connected",
      })) };
      expect(safeMetadata("catalog", oversized)).toBe(true);
      for (const result of [
        { status: 503, data: null, cacheable: true },
        { status: 200, data: catalog, cacheable: false },
        { status: 200, data: { ...catalog, access_token: "synthetic-secret" }, cacheable: true },
        { status: 200, data: oversized, cacheable: true },
      ]) {
        const read = vi.fn(async () => result);
        expect((await snapshots.read({ authorityKey: "unsafe" }, read)).expiresAt).toBe(0);
        expect((await snapshots.read({ authorityKey: "unsafe" }, read)).expiresAt).toBe(0);
        expect(read).toHaveBeenCalledTimes(2);
      }
      const failed = vi.fn(async () => { throw new Error("source unavailable"); });
      await expect(snapshots.read({ authorityKey: "failed" }, failed)).rejects.toThrow("source unavailable");
      expect((await snapshots.read({ authorityKey: "failed" }, fresh)).status).toBe(200);
    } finally { clock.mockRestore(); }
  });

  it.each([0, -1, NaN, Infinity, 1.5])("never grants fresh L2 lifetime to source expiry %s", async expiresAt => {
    const result = await cachedAccountMetadata(`source-expiry-${expiresAt}`, "catalog", options,
      async () => ({ status: 200, data: catalog, expiresAt }));
    expect(result.expiresAt).toBe(0);
    const cache = await caches.open(METADATA_CACHE_NAME);
    expect(await cache.match(await metadataCacheKey(`source-expiry-${expiresAt}`, "catalog", options.authorityKey))).toBeUndefined();
  });
});

it("expires the actual source DO snapshot across forced colo misses", async () => {
  const user = "source-expiry-rpc-owner", id = "E".repeat(43);
  let now = Date.now();
  const clock = vi.spyOn(Date, "now").mockImplementation(() => now);
  try {
    const first = await service.readAccountDiscovery(user, "catalog", options);
    expect(first.expiresAt).toBe(now + METADATA_TTL_MS);
    await expectStoredMetadata(user, "catalog", first);
    expect((await SELF.fetch(`https://broker.internal/users/${user}/mcp-connections/${id}`, {
      method: "PUT", headers: { "content-type": "application/json" },
      body: JSON.stringify({ endpoint: "https://mcp.linear.app/mcp", name: "After original snapshot" }),
    })).status).toBe(200);
    const cache = await caches.open(METADATA_CACHE_NAME);
    const key = await metadataCacheKey(runtime.USER_CONNECTORS.idFromName(user).toString(), "catalog", options.authorityKey);
    now += 60_000;
    expect(await cache.delete(key)).toBe(true);
    expect(await service.readAccountDiscovery(user, "catalog", options)).toEqual(first);
    await expectStoredMetadata(user, "catalog", first);
    const stored = await cache.match(key);
    expect(stored!.headers.get("cache-control")).toBe("max-age=840");
    now = first.expiresAt + 1;
    expect(await cache.delete(key)).toBe(true);
    const expired = await service.readAccountDiscovery(user, "catalog", options);
    expect(expired.expiresAt).toBe(now + METADATA_TTL_MS);
    expect(expired.data).toMatchObject({ mcp_connections: [{ id, name: "After original snapshot" }] });
    console.log("SOURCE_DISCOVERY_EXPIRY_EVIDENCE", JSON.stringify({ forcedColoMissOriginalExpiry: true, remainingSeconds: 840, realDOExpiryRefresh: true }));
  } finally { clock.mockRestore(); }
});

it("keeps pending and degraded Link catalogs uncached through real service and DO RPC", async () => {
  const user = "source-degraded-link-owner";
  let now = Date.now(), polls = 0;
  const clock = vi.spyOn(Date, "now").mockImplementation(() => now);
  // Only the external Link API is synthetic; Egress and both DOs run in workerd.
  const upstream = vi.spyOn(globalThis, "fetch").mockImplementation(async input => {
    const url = new URL(new Request(input).url);
    if (url.href === "https://login.link.com/device/code") return Response.json({
      device_code: "synthetic-device", user_code: "synthetic-code",
      verification_uri_complete: "https://app.link.com/verify?code=synthetic", expires_in: 600, interval: 5,
    });
    if (url.href === "https://login.link.com/device/token") {
      polls++;
      if (polls === 1) throw new Error("synthetic Link outage");
      return Response.json({ access_token: "synthetic-link-access", refresh_token: "synthetic-link-refresh", token_type: "Bearer", expires_in: 3600 });
    }
    if (url.href === "https://api.link.com/userinfo") return Response.json({ email: "synthetic@example.test" });
    throw new Error(`unexpected fixture request ${url.origin}`);
  });
  try {
    expect((await SELF.fetch(`https://broker.internal/users/${user}/connectors/link`, { method: "POST" })).status).toBe(200);
    const pending = await service.readAccountDiscovery(user, "catalog", options);
    expect(pending.expiresAt).toBe(0);
    expect(polls).toBe(0);
    now += 5001;
    const degraded = await service.readAccountDiscovery(user, "catalog", options);
    expect(degraded).toMatchObject({ status: 200, expiresAt: 0, data: { connectors: { link: { connected: false } } } });
    expect(polls).toBe(1);
    const cache = await caches.open(METADATA_CACHE_NAME);
    const key = await metadataCacheKey(runtime.USER_CONNECTORS.idFromName(user).toString(), "catalog", options.authorityKey);
    expect(await cache.match(key)).toBeUndefined();
    now += 5001;
    const recovered = await service.readAccountDiscovery(user, "catalog", options);
    expect(recovered).toMatchObject({ status: 200, expiresAt: 0, data: { connectors: { link: { connected: true } } } });
    expect(polls).toBe(2);
    const stable = await service.readAccountDiscovery(user, "catalog", options);
    expect(stable.expiresAt).toBe(now + METADATA_TTL_MS);
    await expectStoredMetadata(user, "catalog", stable);
    expect(JSON.stringify(stable)).not.toMatch(/synthetic-link-access|synthetic-link-refresh|synthetic-device/);
    console.log("SOURCE_DISCOVERY_LINK_EVIDENCE", JSON.stringify({ pendingExpiryZero: true, upstreamFailureExpiryZero: true, nextDueReadRecovers: true, stableReadCached: true }));
  } finally { upstream.mockRestore(); clock.mockRestore(); }
});
