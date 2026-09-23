import { describe, expect, it, vi } from "vitest";
import { AccountCatalogCache, accountCatalog, ACCOUNT_DISCOVERY_TTL_MS } from "../src/account-catalog";
import { accountVaultMetadata, accountInfo, projectAccountInfo } from "../src/account-info";
import type { CloudflareAccountDiscoveryResult, CloudflareAccountMetadataComponent } from "nanocodex/cloudflare/egress";

const catalog = { connectors: {}, mcp_connections: [] };
const vault = [{ id: "V".repeat(32), kind: "api_key", name: "Synthetic", created_at: 1 }];
function fixture() {
  const readAccountDiscovery = vi.fn(async (_owner: string, component: CloudflareAccountMetadataComponent, _options: unknown): Promise<CloudflareAccountDiscoveryResult> => ({
    schema: 1, status: 200, data: component === "catalog" ? catalog : vault, expiresAt: Date.now() + ACCOUNT_DISCOVERY_TTL_MS,
  }));
  const binding = { readAccountDiscovery, readAccountCatalog: vi.fn(async () => ({ status: 200, catalog })),
    readAccountVault: vi.fn(async () => ({ status: 200, vault })), fetch: vi.fn(async () => { throw new Error("unexpected fetch"); }) };
  return { ...binding, broker: binding as unknown as Fetcher };
}

describe("opt-in L2 promotion", () => {
  it.each(["catalog", "vault"] as const)("expires at the earlier original %s expiry, never 15+15 minutes", async earlier => {
    let now = 1_000;
    const clock = vi.spyOn(Date, "now").mockImplementation(() => now);
    const f = fixture(), cache = new AccountCatalogCache();
    f.readAccountDiscovery.mockImplementation(async (_owner, component) => ({ schema: 1, status: 200,
      data: component === "catalog" ? catalog : vault, expiresAt: component === earlier ? 2_000 : 4_000 }));
    const read = () => Promise.all([cache.get(f.broker, "owner", "authority"), cache.vault(f.broker, "owner", "authority")]);
    try {
      await read(); now = 1_999; await read(); expect(f.readAccountDiscovery).toHaveBeenCalledTimes(2);
      now = 2_000; await read(); expect(f.readAccountDiscovery).toHaveBeenCalledTimes(4);
    } finally { clock.mockRestore(); }
  });

  it("reloads both components even before first use and leaves live APIs live", async () => {
    const f = fixture(), cache = new AccountCatalogCache();
    cache.invalidate();
    await Promise.all([cache.get(f.broker, "owner", "authority"), cache.vault(f.broker, "owner", "authority")]);
    for (const component of ["catalog", "vault"]) expect(f.readAccountDiscovery).toHaveBeenCalledWith("owner", component, { authorityKey: "authority", reload: true });
    await accountCatalog(f.broker, "owner"); await accountVaultMetadata(f.broker, "owner");
    expect(f.readAccountCatalog).toHaveBeenCalledOnce(); expect(f.readAccountVault).toHaveBeenCalledOnce();
    expect(f.readAccountDiscovery).toHaveBeenCalledTimes(2);
  });

  it.each([200, 503])("old pending reads cannot overwrite or evict a fresh local reload (status=%s)", async status => {
    const f = fixture(), cache = new AccountCatalogCache();
    let release!: (value: CloudflareAccountDiscoveryResult) => void;
    f.readAccountDiscovery.mockImplementationOnce(() => new Promise(resolve => { release = resolve; }));
    const old = cache.get(f.broker, "owner", "authority");
    const result = old.catch(() => null);
    cache.invalidate();
    const fresh = await cache.get(f.broker, "owner", "authority");
    release({ schema: 1, status, data: { connectors: {}, mcp_connections: [{ id: "old" }] }, expiresAt: Date.now() + 100 });
    await result;
    expect(await cache.get(f.broker, "owner", "authority")).toBe(fresh);
    expect(f.readAccountDiscovery).toHaveBeenCalledTimes(2);
    expect(f.readAccountDiscovery.mock.calls[1]![2]).toEqual({ authorityKey: "authority", reload: true });
  });

  it("disposes invalid envelopes and failures, evicts them, and never retries through live RPC or HTTP", async () => {
    const f = fixture(), cache = new AccountCatalogCache();
    for (const patch of [{ schema: 2 }, { expiresAt: NaN }, { expiresAt: Date.now() + 2 * ACCOUNT_DISCOVERY_TTL_MS }, { status: 503 }]) {
      const dispose = vi.fn();
      f.readAccountDiscovery.mockResolvedValueOnce({ schema: 1, status: 200, data: catalog, expiresAt: Date.now() + 1000, ...patch, [Symbol.dispose]: dispose } as unknown as CloudflareAccountDiscoveryResult);
      await expect(cache.get(f.broker, "owner", "authority")).rejects.toThrow();
      expect(dispose).toHaveBeenCalledOnce();
    }
    expect(await cache.get(f.broker, "owner", "authority")).toEqual(catalog);
    expect(f.readAccountDiscovery).toHaveBeenCalledTimes(5);
    expect(f.readAccountCatalog).not.toHaveBeenCalled(); expect(f.fetch).not.toHaveBeenCalled();
  });

  it.each(["catalog", "vault"] as const)("disposes a late %s discovery reply after timeout without replacing a fresh entry", async component => {
    vi.useFakeTimers();
    const f = fixture(), cache = new AccountCatalogCache();
    let release!: (value: CloudflareAccountDiscoveryResult) => void;
    const normal = f.readAccountDiscovery.getMockImplementation()!;
    let pending = true;
    f.readAccountDiscovery.mockImplementation((owner, part, options) => {
      if (part === component && pending) { pending = false; return new Promise(resolve => { release = resolve; }); }
      return normal(owner, part, options);
    });
    const read = () => component === "catalog" ? cache.get(f.broker, "owner", "authority") : cache.vault(f.broker, "owner", "authority");
    const dispose = vi.fn();
    try {
      const failed = expect(read()).rejects.toThrow("timed out");
      await vi.advanceTimersByTimeAsync(10_000); await failed;
      const fresh = await read();
      release({ schema: 1, status: 200, data: component === "catalog" ? catalog : vault,
        expiresAt: Date.now() + 1000, [Symbol.dispose]: dispose } as unknown as CloudflareAccountDiscoveryResult);
      await vi.advanceTimersByTimeAsync(0);
      expect(dispose).toHaveBeenCalledOnce(); expect(await read()).toBe(fresh);
      expect(f.fetch).not.toHaveBeenCalled();
    } finally { vi.useRealTimers(); }
  });

  it("projects current caller authority after an L2 read", async () => {
    const f = fixture(), cache = new AccountCatalogCache();
    const id = "C".repeat(43);
    f.readAccountDiscovery.mockImplementation(async (_owner, component) => ({ schema: 1, status: 200, expiresAt: Date.now() + 1000,
      data: component === "vault" ? vault : { connectors: { github: { connected: true, connections: [{ id, label: "Synthetic", account_id: "synthetic", capabilities: ["github"] }] } }, mcp_connections: [] } }));
    const info = await accountInfo(f.broker, "owner", { enabled: true, catalog: cache.get(f.broker, "owner", "authority"), vault: cache.vault(f.broker, "owner", "authority") });
    expect(info.authenticated).toEqual(["github"]);
    expect(projectAccountInfo(info, ["github"], { github: [] }).authenticated).toEqual([]);
    expect(projectAccountInfo(info, [], {}).connectorAccounts).toEqual({});
  });
});
