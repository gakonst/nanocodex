import { describe, expect, it, vi } from "vitest";
import { AccountCatalogCache, ACCOUNT_DISCOVERY_TTL_MS, accountCatalog } from "../src/account-catalog";
import { accountInfo, accountVaultMetadata, projectAccountInfo } from "../src/account-info";
import type { CloudflareAccountCatalogResult, CloudflareAccountVaultResult } from "nanocodex/cloudflare/egress";

const catalog = { connectors: {}, mcp_connections: [] };
const vault = [{ id: "V".repeat(32), kind: "api_key", name: "Synthetic API", created_at: 1 }];
function fixture() {
  const binding = {
    fetch: vi.fn(async () => { throw new Error("unexpected HTTP fallback"); }),
    readAccountCatalog: vi.fn(async (_userId: string): Promise<CloudflareAccountCatalogResult> => ({ status: 200, catalog })),
    readAccountVault: vi.fn(async (_userId: string): Promise<CloudflareAccountVaultResult> => ({ status: 200, vault })),
  };
  return { ...binding, broker: binding as unknown as Fetcher };
}

describe("account metadata RPC discovery", () => {
  it("uses one RPC per component and still projects the caller's current connection authority", async () => {
    const f = fixture();
    const id = "C".repeat(43);
    f.readAccountCatalog.mockResolvedValue({ status: 200, catalog: {
      connectors: { github: { connected: true, account: "Synthetic", connections: [
        { id, label: "Synthetic", account_id: "synthetic", capabilities: ["github"] },
      ] } }, mcp_connections: [],
    } });
    const cache = new AccountCatalogCache();
    const options = { enabled: true, catalog: cache.get(f.broker, "owner", "authority"),
      vault: cache.vault(f.broker, "owner", "authority") };
    const owner = await accountInfo(f.broker, "owner", options);
    expect(owner.authenticated).toEqual(["github"]);
    expect(owner.vault).toEqual(vault);
    expect(projectAccountInfo(owner, ["github"], { github: [] }).authenticated).toEqual([]);
    expect(projectAccountInfo(owner, [], {}).connectorAccounts).toEqual({});
    expect(f.readAccountCatalog).toHaveBeenCalledExactlyOnceWith("owner");
    expect(f.readAccountVault).toHaveBeenCalledExactlyOnceWith("owner");
    expect(f.fetch).not.toHaveBeenCalled();
  });

  it("starts vault RPC while the catalog is pending", async () => {
    const f = fixture();
    let release!: (result: CloudflareAccountCatalogResult) => void;
    f.readAccountCatalog.mockImplementationOnce(() => new Promise(resolve => { release = resolve; }));
    const cache = new AccountCatalogCache();
    const pending = cache.get(f.broker, "owner", "authority");
    expect(await cache.vault(f.broker, "owner", "authority")).toEqual(vault);
    expect(f.readAccountCatalog).toHaveBeenCalledOnce();
    release({ status: 200, catalog });
    await pending;
  });

  it("keeps fixed TTL, shared identity, owner/authority partitions and explicit invalidation", async () => {
    const f = fixture();
    let now = 1_000;
    const clock = vi.spyOn(Date, "now").mockImplementation(() => now);
    const first = new AccountCatalogCache();
    const second = new AccountCatalogCache();
    const read = async (cache: AccountCatalogCache, broker = f.broker, owner = "owner", authority = "authority") =>
      Promise.all([cache.get(broker, owner, authority), cache.vault(broker, owner, authority)]);
    try {
      await read(first);
      now += ACCOUNT_DISCOVERY_TTL_MS - 1;
      await read(second);
      expect(f.readAccountCatalog).toHaveBeenCalledTimes(1);
      expect(f.readAccountVault).toHaveBeenCalledTimes(1);
      now += 1;
      await read(second);
      expect(f.readAccountCatalog).toHaveBeenCalledTimes(2);
      expect(f.readAccountVault).toHaveBeenCalledTimes(2);
      await read(first, f.broker, "other-owner");
      await read(first, f.broker, "owner", "new-authority");
      await read(first, { ...f.broker } as Fetcher);
      expect(f.readAccountCatalog).toHaveBeenCalledTimes(5);
      await read(second);
      second.invalidate();
      await read(second);
      expect(f.readAccountCatalog).toHaveBeenCalledTimes(6);
      expect(f.readAccountVault).toHaveBeenCalledTimes(6);
    } finally { clock.mockRestore(); }
  });

  it("evicts failed RPC discovery and does not retry it through HTTP", async () => {
    const f = fixture();
    f.readAccountCatalog.mockRejectedValueOnce(new Error("synthetic RPC failure"));
    const cache = new AccountCatalogCache();
    await expect(cache.get(f.broker, "owner", "authority")).rejects.toThrow("synthetic RPC failure");
    expect(await cache.get(f.broker, "owner", "authority")).toEqual(catalog);
    f.readAccountVault.mockResolvedValueOnce({ status: 503, vault: null });
    await expect(cache.vault(f.broker, "owner", "authority")).rejects.toThrow("503");
    await cache.vault(f.broker, "owner", "authority");
    expect(f.readAccountCatalog).toHaveBeenCalledTimes(3);
    expect(f.readAccountVault).toHaveBeenCalledTimes(2);
    expect(f.fetch).not.toHaveBeenCalled();
  });

  it("validates RPC metadata and fails closed when vault fields include a secret", async () => {
    const f = fixture();
    f.readAccountCatalog.mockResolvedValueOnce({ status: 503, catalog });
    await expect(accountCatalog(f.broker, "owner")).rejects.toThrow("503");
    f.readAccountCatalog.mockResolvedValueOnce({ status: 200, catalog: { connectors: {} } });
    await expect(accountCatalog(f.broker, "owner")).rejects.toThrow("invalid response");
    f.readAccountVault.mockResolvedValueOnce({ status: 200, vault: [{ ...vault[0], api_key: "synthetic-secret" }] });
    await expect(accountVaultMetadata(f.broker, "owner")).rejects.toThrow("invalid metadata");
    const entries = await accountVaultMetadata(f.broker, "owner");
    expect(Object.isFrozen(entries)).toBe(true);
    expect(Object.isFrozen(entries[0])).toBe(true);
    expect(f.fetch).not.toHaveBeenCalled();
  });

  it("retains fetch-only compatibility and the same public projections", async () => {
    const fetch = vi.fn(async (input: RequestInfo | URL) => Response.json(
      String(input).endsWith("/catalog") ? catalog : { vault },
    ));
    const broker = { fetch } as unknown as Fetcher;
    expect(await accountCatalog(broker, "owner")).toEqual(catalog);
    expect(await accountVaultMetadata(broker, "owner")).toEqual(vault);
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it("bounds RPC discovery to the existing deadline without an HTTP retry", async () => {
    vi.useFakeTimers();
    const f = fixture();
    f.readAccountCatalog.mockImplementationOnce(() => new Promise(() => {}));
    f.readAccountVault.mockImplementationOnce(() => new Promise(() => {}));
    const cache = new AccountCatalogCache();
    try {
      const catalogFailure = expect(cache.get(f.broker, "owner", "authority")).rejects.toThrow("timed out after 10000ms");
      const vaultFailure = expect(cache.vault(f.broker, "owner", "authority")).rejects.toThrow("timed out after 10000ms");
      await vi.advanceTimersByTimeAsync(10_000);
      await Promise.all([catalogFailure, vaultFailure]);
      await cache.get(f.broker, "owner", "authority");
      expect(f.readAccountCatalog).toHaveBeenCalledTimes(2);
      expect(f.fetch).not.toHaveBeenCalled();
    } finally { vi.useRealTimers(); }
  });
});

describe("discovery RPC result ownership", () => {
  it("disposes both owners once and retains detached metadata across cache hits", async () => {
    const f = fixture();
    const catalogDispose = vi.fn(), vaultDispose = vi.fn();
    const rawCatalog = { status: 200, catalog: structuredClone(catalog), [Symbol.dispose]: catalogDispose };
    const rawVault = { status: 200, vault: structuredClone(vault), [Symbol.dispose]: vaultDispose };
    f.readAccountCatalog.mockResolvedValueOnce(rawCatalog);
    f.readAccountVault.mockResolvedValueOnce(rawVault);
    const cache = new AccountCatalogCache();
    const first = await cache.get(f.broker, "owner", "authority");
    const entries = await cache.vault(f.broker, "owner", "authority");
    expect(catalogDispose).toHaveBeenCalledOnce(); expect(vaultDispose).toHaveBeenCalledOnce();
    expect(Object.getOwnPropertySymbols(first as object)).toEqual([]);
    rawCatalog.catalog.connectors = { github: { connected: false } };
    rawVault.vault[0]!.name = "changed after disposal";
    expect(await cache.get(f.broker, "owner", "authority")).toBe(first);
    expect(first).toEqual(catalog); expect(entries).toEqual(vault);
    expect(f.readAccountCatalog).toHaveBeenCalledOnce(); expect(f.readAccountVault).toHaveBeenCalledOnce();
  });

  it("disposes status errors and invalid projections before rejecting", async () => {
    const f = fixture();
    for (const result of [{ status: 503, catalog: null }, { status: 200, catalog: {} }]) {
      const dispose = vi.fn();
      const owned = { ...result, [Symbol.dispose]: dispose };
      f.readAccountCatalog.mockResolvedValueOnce(owned);
      await expect(accountCatalog(f.broker, "owner")).rejects.toThrow();
      expect(dispose).toHaveBeenCalledOnce();
    }
    for (const result of [{ status: 503, vault: null }, { status: 200, vault: [{}] }]) {
      const dispose = vi.fn();
      const owned = { ...result, [Symbol.dispose]: dispose };
      f.readAccountVault.mockResolvedValueOnce(owned);
      await expect(accountVaultMetadata(f.broker, "owner")).rejects.toThrow();
      expect(dispose).toHaveBeenCalledOnce();
    }
  });

  it.each([200, 503])("disposes late catalog replies after deadline eviction (status=%s)", async status => {
    vi.useFakeTimers();
    const f = fixture();
    let release!: (value: CloudflareAccountCatalogResult) => void;
    const dispose = vi.fn();
    f.readAccountCatalog.mockImplementationOnce(() => new Promise(resolve => { release = resolve; }));
    const cache = new AccountCatalogCache();
    try {
      const expired = expect(cache.get(f.broker, "owner", "authority")).rejects.toThrow("timed out");
      await vi.advanceTimersByTimeAsync(10_000); await expired;
      const replacement = await cache.get(f.broker, "owner", "authority");
      release({ status, catalog, [Symbol.dispose]: dispose } as CloudflareAccountCatalogResult);
      await vi.advanceTimersByTimeAsync(0);
      expect(dispose).toHaveBeenCalledOnce();
      expect(await cache.get(f.broker, "owner", "authority")).toBe(replacement);
      expect(f.readAccountCatalog).toHaveBeenCalledTimes(2);
    } finally { vi.useRealTimers(); }
  });
});

describe("late vault RPC ownership", () => {
  it.each([200, 503])("disposes late vault replies even when their signal is aborted (status=%s)", async status => {
    vi.useFakeTimers();
    const f = fixture();
    let release!: (value: CloudflareAccountVaultResult) => void;
    const dispose = vi.fn();
    f.readAccountVault.mockImplementationOnce(() => new Promise(resolve => { release = resolve; }));
    const cache = new AccountCatalogCache();
    try {
      const expired = expect(cache.vault(f.broker, "owner", "authority")).rejects.toThrow("timed out");
      await vi.advanceTimersByTimeAsync(10_000); await expired;
      const replacement = await cache.vault(f.broker, "owner", "authority");
      release({ status, vault, [Symbol.dispose]: dispose } as CloudflareAccountVaultResult);
      await vi.advanceTimersByTimeAsync(0);
      expect(dispose).toHaveBeenCalledOnce();
      expect(await cache.vault(f.broker, "owner", "authority")).toBe(replacement);
      expect(f.readAccountVault).toHaveBeenCalledTimes(2);
    } finally { vi.useRealTimers(); }
  });
});
