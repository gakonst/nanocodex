import { accountVaultMetadata, type VaultEntry } from "./account-info";
import { fetchResponseWithDeadline, withHardDeadline } from "./deadline";
import { performanceCache, performanceStage } from "./performance";

/** Discovery freshness is independent of authentication and live hand presence. */
export const ACCOUNT_DISCOVERY_TTL_MS = 15 * 60_000;
const MAX_DISCOVERY_SNAPSHOTS = 64;
type DiscoverySnapshot = {
  readonly expiresAt: number;
  readonly catalog: Promise<unknown>;
  vault?: Promise<readonly VaultEntry[]>;
};
const snapshots = new WeakMap<object, Map<string, DiscoverySnapshot>>();

/** Discovery only. Raw metadata is projected for each caller; execution checks live authority. */
export class AccountCatalogCache {
  #current?: { entries: Map<string, DiscoverySnapshot>; key: string };

  invalidate(): void {
    // Another Session may have replaced the entry since our last read. An
    // explicit refresh still invalidates this authority's current shared copy.
    if (this.#current) this.#current.entries.delete(this.#current.key);
    this.#current = undefined;
  }

  get(broker: Fetcher, userId: string, authorityKey: string): Promise<unknown> {
    return this.#snapshot(broker, userId, authorityKey).catalog;
  }

  vault(broker: Fetcher, userId: string, authorityKey: string): Promise<readonly VaultEntry[]> {
    const entry = this.#snapshot(broker, userId, authorityKey);
    if (!entry.vault) {
      const { entries, key } = this.#current!;
      // Startup requests both components in the same stack, so they run in
      // parallel. Catalog-only discovery never starts an unnecessary vault read.
      entry.vault = performanceStage("account.vault", () => withHardDeadline(
        "account vault", 10_000, signal => accountVaultMetadata(broker, userId, signal),
      ));
      void entry.vault.catch(() => {
        if (entries.get(key) === entry) entries.delete(key);
      });
    }
    return entry.vault;
  }

  #snapshot(broker: Fetcher, userId: string, authorityKey: string): DiscoverySnapshot {
    let entries = snapshots.get(broker);
    if (!entries) {
      entries = new Map();
      snapshots.set(broker, entries);
    }
    const key = JSON.stringify([userId, authorityKey]);
    this.#current = { entries, key };
    const now = Date.now();
    for (const [entryKey, entry] of entries) {
      if (entry.expiresAt <= now) entries.delete(entryKey);
    }
    const current = entries.get(key);
    if (current) {
      // Refresh recency, never the original metadata expiry.
      entries.delete(key);
      entries.set(key, current);
      performanceCache("account.discovery", "hit", now - (current.expiresAt - ACCOUNT_DISCOVERY_TTL_MS), current.expiresAt - now);
      return current;
    }
    performanceCache("account.discovery", "miss", 0, ACCOUNT_DISCOVERY_TTL_MS);
    const entry: DiscoverySnapshot = {
      expiresAt: now + ACCOUNT_DISCOVERY_TTL_MS,
      catalog: accountCatalog(broker, userId),
    };
    entries.set(key, entry);
    while (entries.size > MAX_DISCOVERY_SNAPSHOTS) entries.delete(entries.keys().next().value!);
    const evict = () => {
      // An expired, invalidated or evicted read never removes its replacement.
      if (entries.get(key) === entry) entries.delete(key);
    };
    void entry.catalog.catch(evict);
    return entry;
  }
}

/** One live read shared by runtime discovery and first-turn environment context. */
export function accountCatalog(broker: Fetcher, userId: string): Promise<unknown> {
  return performanceStage("account.catalog", () => fetchResponseWithDeadline(
    broker,
    `https://broker.internal/users/${encodeURIComponent(userId)}/catalog`,
    {},
    10_000,
    "account catalog",
    async (response) => {
      if (!response.ok) throw new Error(`account catalog failed with HTTP ${response.status}`);
      const value: unknown = await response.json();
      if (!value || typeof value !== "object" || Array.isArray(value)
        || !("connectors" in value) || !value.connectors || typeof value.connectors !== "object"
        || Array.isArray(value.connectors)
        || !("mcp_connections" in value) || !Array.isArray(value.mcp_connections)) {
        throw new Error("account catalog returned an invalid response");
      }
      return value;
    },
  ));
}
