import { afterEach, describe, expect, it, vi } from "vitest";
import { ACCOUNT_DISCOVERY_TTL_MS, AccountCatalogCache } from "../src/account-catalog";
import { accountInfo, accountVaultMetadata, type VaultEntry } from "../src/account-info";
import { performanceScope } from "../src/performance";

const catalog = { connectors: {}, mcp_connections: [] };
const vault: readonly VaultEntry[] = [{ id: "a".repeat(22), kind: "api_key", name: "Example", created_at: 1 }];
const authority = JSON.stringify(["organization-a", "team-a", 1]);
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(accept => { resolve = accept; });
  return { promise, resolve };
}
function isVault(input: RequestInfo | URL): boolean { return String(input).endsWith("/credentials/vault"); }
function brokerFixture() {
  const fetch = vi.fn(async (input: RequestInfo | URL) => Response.json(isVault(input) ? { vault } : catalog));
  return { broker: { fetch } as unknown as Fetcher, fetch };
}
function read(cache: AccountCatalogCache, broker: Fetcher, owner = "owner", scope = authority) {
  return Promise.all([cache.get(broker, owner, scope), cache.vault(broker, owner, scope)]);
}
afterEach(() => { vi.restoreAllMocks(); });

describe("shared account discovery snapshots", () => {
  it("coalesces both reads across instances without making catalog wait for vault", async () => {
    const catalogRead = deferred<Response>();
    const vaultRead = deferred<Response>();
    const fetch = vi.fn((input: RequestInfo | URL) => isVault(input) ? vaultRead.promise : catalogRead.promise);
    const broker = { fetch } as unknown as Fetcher;
    const first = new AccountCatalogCache();
    const second = new AccountCatalogCache();
    const firstCatalog = first.get(broker, "owner", authority);
    const firstVault = first.vault(broker, "owner", authority);
    expect(second.get(broker, "owner", authority)).toBe(firstCatalog);
    expect(second.vault(broker, "owner", authority)).toBe(firstVault);
    let vaultSettled = false;
    void firstVault.then(() => { vaultSettled = true; });
    catalogRead.resolve(Response.json(catalog));
    await expect(firstCatalog).resolves.toEqual(catalog);
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(vaultSettled).toBe(false);
    vaultRead.resolve(Response.json({ vault }));
    await expect(firstVault).resolves.toEqual(vault);
  });

  it("loads vault lazily without granting a new expiry to the later read", async () => {
    let now = 1_000;
    vi.spyOn(Date, "now").mockImplementation(() => now);
    const { broker, fetch } = brokerFixture();
    const cache = new AccountCatalogCache();
    await cache.get(broker, "owner", authority);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(isVault(fetch.mock.calls[0]![0])).toBe(false);
    now += ACCOUNT_DISCOVERY_TTL_MS - 1;
    await cache.vault(broker, "owner", authority);
    expect(fetch).toHaveBeenCalledTimes(2);
    now += 1;
    await read(cache, broker);
    expect(fetch).toHaveBeenCalledTimes(4);
  });

  it("reports only bounded cache timing and reuse, without authority keys or metadata", async () => {
    let now = 1_000;
    vi.spyOn(Date, "now").mockImplementation(() => now);
    const log = vi.spyOn(console, "info").mockImplementation(() => {});
    const { broker } = brokerFixture();
    const cache = new AccountCatalogCache();
    await performanceScope("fixture-trace", "fixture-admission", async () => {
      await read(cache, broker, "private-owner", "private-authority");
      now += 25;
      await cache.get(broker, "private-owner", "private-authority");
    });
    const events = log.mock.calls.map(([event]) => event).filter(event => event.stage === "account.discovery");
    expect(events.map(({ cache_state, cache_age_ms, remaining_ttl_ms }) => ({ cache_state, cache_age_ms, remaining_ttl_ms })))
      .toEqual([
        { cache_state: "miss", cache_age_ms: 0, remaining_ttl_ms: ACCOUNT_DISCOVERY_TTL_MS },
        { cache_state: "hit", cache_age_ms: 0, remaining_ttl_ms: ACCOUNT_DISCOVERY_TTL_MS },
        { cache_state: "hit", cache_age_ms: 25, remaining_ttl_ms: ACCOUNT_DISCOVERY_TTL_MS - 25 },
      ]);
    expect(JSON.stringify(events)).not.toMatch(/private-owner|private-authority|Example/);
  });

  it("isolates binding, owner, organization, team and authorization epoch", async () => {
    const { broker, fetch } = brokerFixture();
    const cache = new AccountCatalogCache();
    const scopes = [authority, JSON.stringify(["organization-b", "team-a", 1]),
      JSON.stringify(["organization-a", "team-b", 1]), JSON.stringify(["organization-a", "team-a", 2])];
    for (const scope of scopes) await read(cache, broker, "owner", scope);
    await read(cache, broker, "other-owner");
    await read(cache, { fetch } as unknown as Fetcher);
    await read(new AccountCatalogCache(), broker);
    expect(fetch).toHaveBeenCalledTimes(12);
  });

  it("expires both components at the original 15-minute deadline, without extending on hits or completion", async () => {
    let now = 1_000;
    vi.spyOn(Date, "now").mockImplementation(() => now);
    const vaultRead = deferred<Response>();
    let initialVault = true;
    const fetch = vi.fn(async (input: RequestInfo | URL) => {
      if (!isVault(input)) return Response.json(catalog);
      if (initialVault) { initialVault = false; return vaultRead.promise; }
      return Response.json({ vault });
    });
    const broker = { fetch } as unknown as Fetcher;
    const first = new AccountCatalogCache();
    const initialCatalog = first.get(broker, "owner", authority);
    const initialMetadata = first.vault(broker, "owner", authority);
    await initialCatalog;
    now += ACCOUNT_DISCOVERY_TTL_MS - 1;
    vaultRead.resolve(Response.json({ vault }));
    await initialMetadata;
    const second = new AccountCatalogCache();
    await read(second, broker);
    expect(fetch).toHaveBeenCalledTimes(2);
    now += 1;
    await read(second, broker);
    expect(fetch).toHaveBeenCalledTimes(4);
    expect(ACCOUNT_DISCOVERY_TTL_MS).toBe(900_000);
  });

  it("bounds each binding to 64 snapshots and evicts least recently read entries", async () => {
    const { broker, fetch } = brokerFixture();
    const cache = new AccountCatalogCache();
    for (let index = 0; index < 64; index += 1) await read(cache, broker, `owner-${index}`);
    await read(cache, broker, "owner-0");
    await read(cache, broker, "owner-64");
    expect(fetch).toHaveBeenCalledTimes(130);
    await read(cache, broker, "owner-0");
    expect(fetch).toHaveBeenCalledTimes(130);
    await read(cache, broker, "owner-1");
    expect(fetch).toHaveBeenCalledTimes(132);
  });

  it("makes invalidation visible to other instances, including one holding an older copy", async () => {
    const { broker, fetch } = brokerFixture();
    const first = new AccountCatalogCache();
    const second = new AccountCatalogCache();
    await read(first, broker);
    await read(second, broker);
    first.invalidate();
    await read(first, broker);
    second.invalidate();
    await read(first, broker);
    expect(fetch).toHaveBeenCalledTimes(6);
  });

  it.each(["unavailable", "malformed"])("evicts %s vault reads while keeping the caller's empty display fallback", async failure => {
    let failed = false;
    const fetch = vi.fn(async (input: RequestInfo | URL) => {
      if (!isVault(input)) return Response.json(catalog);
      if (failed) return Response.json({ vault });
      failed = true;
      return failure === "unavailable" ? new Response(null, { status: 503 })
        : Response.json({ vault: [{ ...vault[0], secret: "fixture-secret" }] });
    });
    const broker = { fetch } as unknown as Fetcher;
    const cache = new AccountCatalogCache();
    const options = () => ({ enabled: true, catalog: cache.get(broker, "owner", authority),
      vault: cache.vault(broker, "owner", authority) });
    await expect(accountInfo(broker, "owner", options())).resolves.toMatchObject({ status: "ready", vault: [] });
    await expect(accountInfo(broker, "owner", options())).resolves.toMatchObject({ status: "ready", vault });
    expect(fetch).toHaveBeenCalledTimes(4);
  });

  it("retries a failed catalog instead of caching unavailable discovery", async () => {
    let failed = false;
    const fetch = vi.fn(async (input: RequestInfo | URL) => {
      if (isVault(input)) return Response.json({ vault });
      if (failed) return Response.json(catalog);
      failed = true;
      return new Response(null, { status: 503 });
    });
    const broker = { fetch } as unknown as Fetcher;
    const cache = new AccountCatalogCache();
    const first = await Promise.allSettled([cache.get(broker, "owner", authority), cache.vault(broker, "owner", authority)]);
    expect(first[0].status).toBe("rejected");
    await read(cache, broker);
    expect(fetch).toHaveBeenCalledTimes(4);
  });

  it.each([200, 503])("a late %s response cannot restore an invalidated snapshot or evict its replacement", async status => {
    const oldRead = deferred<Response>();
    let initial = true;
    const fetch = vi.fn(async (input: RequestInfo | URL) => {
      if (isVault(input)) return Response.json({ vault });
      if (initial) { initial = false; return oldRead.promise; }
      return Response.json({ ...catalog, marker: "current" });
    });
    const broker = { fetch } as unknown as Fetcher;
    const cache = new AccountCatalogCache();
    const old = cache.get(broker, "owner", authority);
    const settled = Promise.allSettled([old]);
    await cache.vault(broker, "owner", authority);
    cache.invalidate();
    await read(cache, broker);
    oldRead.resolve(status === 200 ? Response.json({ ...catalog, marker: "old" }) : new Response(null, { status }));
    await settled;
    await expect(cache.get(broker, "owner", authority)).resolves.toMatchObject({ marker: "current" });
    expect(fetch).toHaveBeenCalledTimes(4);
  });

  it("projects the same raw snapshot for each authorization without sharing runtime machines", async () => {
    const id = "a".repeat(43);
    const fetch = vi.fn(async (input: RequestInfo | URL) => Response.json(isVault(input) ? { vault } : {
      connectors: { github: { connected: true, connections: [{ id, label: "work", capabilities: ["github"] }] } },
      mcp_connections: [],
    }));
    const broker = { fetch } as unknown as Fetcher;
    const cache = new AccountCatalogCache();
    const discovery = { catalog: cache.get(broker, "owner", authority), vault: cache.vault(broker, "owner", authority) };
    const machines = [{ id: "user:desktop", name: "Desktop", kind: "user" as const,
      mount: "/desktop", workspace: "/desktop", capabilities: ["shell"], online: true }];
    const permitted = await accountInfo(broker, "owner", { enabled: true, ...discovery, machines,
      allowedConnectors: ["github"], allowedConnections: { github: [id] } });
    const restricted = await accountInfo(broker, "owner", { enabled: true, ...discovery,
      allowedConnectors: ["github"], allowedConnections: { github: [] } });
    expect(permitted.authenticated).toEqual(["github"]);
    expect(restricted.authenticated).toEqual([]);
    expect(permitted.machines).toEqual(machines);
    expect(restricted.machines).toEqual([]);
    expect(permitted.vault).toEqual(vault);
    expect(restricted.vault).toEqual(vault);
    expect(fetch).toHaveBeenCalledTimes(2);
    await accountInfo(broker, "owner", { enabled: true });
    expect(fetch).toHaveBeenCalledTimes(4); // No overrides means an explicit live read.
  });

  it("does not let one consumer's cancellation abort another's shared reads", async () => {
    const catalogRead = deferred<Response>();
    const vaultRead = deferred<Response>();
    const controller = new AbortController();
    const reason = new Error("cancel one consumer");
    const fetch = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      expect(init?.signal).not.toBe(controller.signal);
      return isVault(input) ? vaultRead.promise : catalogRead.promise;
    });
    const broker = { fetch } as unknown as Fetcher;
    const cache = new AccountCatalogCache();
    const discovery = { catalog: cache.get(broker, "owner", authority), vault: cache.vault(broker, "owner", authority) };
    const first = accountInfo(broker, "owner", { enabled: true, ...discovery, signal: controller.signal });
    const rejected = expect(first).rejects.toBe(reason);
    const second = accountInfo(broker, "owner", { enabled: true, ...discovery });
    controller.abort(reason);
    catalogRead.resolve(Response.json(catalog));
    vaultRead.resolve(Response.json({ vault }));
    await rejected;
    await expect(second).resolves.toMatchObject({ status: "ready", vault });
    expect(fetch).toHaveBeenCalledTimes(2);
  });
});

describe("strict cacheable vault metadata", () => {
  it("accepts a truly empty vault and freezes only safe projected metadata", async () => {
    for (const entries of [[], vault]) {
      const fetch = vi.fn(async () => Response.json({ vault: entries, ignored: "not retained" }));
      const value = await accountVaultMetadata({ fetch }, "owner/with space");
      expect(value).toEqual(entries);
      expect(Object.isFrozen(value)).toBe(true);
      if (value[0]) expect(Object.isFrozen(value[0])).toBe(true);
      expect(fetch).toHaveBeenCalledWith("https://broker.internal/users/owner%2Fwith%20space/credentials/vault");
    }
  });

  it.each(["api_key", "secret", "password", "value"])("rejects unexpected %s material rather than treating it as an empty success", async field => {
    await expect(accountVaultMetadata({ fetch: async () => Response.json({ vault: [
      { ...vault[0], [field]: "fixture-secret" },
    ] }) }, "owner")).rejects.toThrow("invalid metadata");
  });

  it.each([undefined, {}, Array.from({ length: 101 }, () => vault[0]), [
    { id: "l".repeat(22), kind: "login", name: "Example", username: "person", created_at: 1, browser_origin: "https://example.com/path" },
  ]])("rejects invalid or oversized vault metadata %j", async value => {
    await expect(accountVaultMetadata({ fetch: async () => Response.json({ vault: value }) }, "owner"))
      .rejects.toThrow("invalid metadata");
  });

  it("reprojects a supplied override instead of exposing unexpected fields", async () => {
    const fetch = vi.fn();
    const info = await accountInfo({ fetch }, "owner", { enabled: true, catalog: Promise.resolve(catalog),
      vault: Promise.resolve([{ ...vault[0], password: "fixture-secret" }] as unknown as readonly VaultEntry[]) });
    expect(info.vault).toEqual([]);
    expect(JSON.stringify(info)).not.toContain("fixture-secret");
    expect(fetch).not.toHaveBeenCalled();
  });
});
