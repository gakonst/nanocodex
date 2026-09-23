import { ACCOUNT_DISCOVERY_TTL_MS, discoveryMetadata, type DiscoveryRead } from "./account-discovery";
export { ACCOUNT_DISCOVERY_TTL_MS } from "./account-discovery";
import { consumeRpcData } from "nanocodex/cloudflare/rpc";
import type { CloudflareAccountMetadataBinding } from "nanocodex/cloudflare/egress";
import { accountVaultMetadata, type VaultEntry } from "./account-info";
import { fetchResponseWithDeadline, withHardDeadline } from "./deadline";
import { performanceCache, performanceStage } from "./performance";

/** Discovery freshness is independent of authentication and live hand presence. */
const MAX_DISCOVERY_SNAPSHOTS = 64;
type DiscoverySnapshot = {
  expiresAt: number;
  readonly discovery: DiscoveryRead;
  readonly catalog: Promise<unknown>;
  vault?: Promise<readonly VaultEntry[]>;
};
const snapshots = new WeakMap<object, Map<string, DiscoverySnapshot>>();

/** Discovery only. Raw metadata is projected for each caller; execution checks live authority. */
export class AccountCatalogCache {
  #reload = false;
  #current?: { entries: Map<string, DiscoverySnapshot>; key: string };

  invalidate(): void {
    this.#reload = true;
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
        "account vault", 10_000, signal => accountVaultMetadata(broker, userId, signal, entry.discovery),
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
    const reload = this.#reload;
    this.#reload = false;
    if (reload) entries.delete(key);
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
    const discovery: DiscoveryRead = { authorityKey, reload,
      // Mutate only this entry: an old pending read cannot replace a refreshed one.
      adoptExpiry: expiresAt => { entry.expiresAt = Math.min(entry.expiresAt, expiresAt); },
    };
    const entry: DiscoverySnapshot = {
      expiresAt: now + ACCOUNT_DISCOVERY_TTL_MS,
      discovery,
      catalog: accountCatalog(broker, userId, discovery),
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

/** Live by default; L1 opts into discovery and retains the original backend expiry. */
export function accountCatalog(broker: Fetcher, userId: string, discovery?: DiscoveryRead): Promise<unknown> {
  const metadata: CloudflareAccountMetadataBinding = broker;
  if (discovery && typeof metadata.readAccountDiscovery === "function") {
    return performanceStage("account.catalog", () => withHardDeadline("account catalog", 10_000, async () =>
      validateCatalog(await discoveryMetadata(metadata, userId, "catalog", discovery))));
  }
  // Fetch-only adapters remain compatible; RPC errors never trigger an HTTP retry.
  // Service bindings resolve methods dynamically, so deploy egress before managed.
  const readAccountCatalog = metadata.readAccountCatalog;
  if (typeof readAccountCatalog === "function") {
    return performanceStage("account.catalog", () => withHardDeadline("account catalog", 10_000, async () => {
      const result = consumeRpcData(await Reflect.apply(readAccountCatalog, metadata, [userId]));
      if (result.status !== 200) throw new Error(`account catalog failed with HTTP ${result.status}`);
      return validateCatalog(result.catalog);
    }));
  }
  return performanceStage("account.catalog", () => fetchResponseWithDeadline(
    broker,
    `https://broker.internal/users/${encodeURIComponent(userId)}/catalog`,
    {},
    10_000,
    "account catalog",
    async (response) => {
      if (!response.ok) throw new Error(`account catalog failed with HTTP ${response.status}`);
      return validateCatalog(await response.json());
    },
  ));
}

function validateCatalog(value: unknown): unknown {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || !("connectors" in value) || !value.connectors || typeof value.connectors !== "object"
    || Array.isArray(value.connectors)
    || !("mcp_connections" in value) || !Array.isArray(value.mcp_connections)) {
    throw new Error("account catalog returned an invalid response");
  }
  return value;
}
