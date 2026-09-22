import { fetchResponseWithDeadline } from "./deadline";
import { performanceStage } from "./performance";
import { MANAGED_ACCESS_TTL_MS } from "./managed-access";

/** Discovery metadata only; tool execution still checks current authority. */
export class AccountCatalogCache {
  #entry?: { key: string; expiresAt: number; promise: Promise<unknown> };

  invalidate(): void { this.#entry = undefined; }

  get(broker: Fetcher, userId: string, authorityKey: string): Promise<unknown> {
    const key = JSON.stringify([userId, authorityKey]);
    const now = Date.now();
    if (this.#entry?.key === key && this.#entry.expiresAt > now) return this.#entry.promise;
    const entry = { key, expiresAt: now + MANAGED_ACCESS_TTL_MS, promise: accountCatalog(broker, userId) };
    this.#entry = entry;
    void entry.promise.catch(() => { if (this.#entry === entry) this.#entry = undefined; });
    return entry.promise;
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
