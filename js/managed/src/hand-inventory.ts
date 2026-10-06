import type { HostedMachine } from "nanocodex-tools/hosted";

export type HandInventoryEntry = Readonly<{
  id: string;
  name: string;
  kind: "hand" | "workspace" | "vm";
  online: boolean | null;
  health: "connected" | "offline" | "unknown";
}>;
export type HandInventory = Readonly<{
  data: HandInventoryEntry[];
  coverage: "known_account_and_workspace";
  complete: boolean;
}>;

export function inventoryEntry(machine: Pick<HostedMachine, "id" | "name" | "capabilities">,
  online: boolean | null, workspace = false): HandInventoryEntry {
  const vm = machine.capabilities.some(capability => capability === "vm" || capability === "virtual_machine")
    || machine.id.startsWith("vm:");
  return { id: machine.id, name: machine.name, kind: vm ? "vm" : workspace ? "workspace" : "hand",
    online, health: online === true ? "connected" : online === false ? "offline" : "unknown" };
}

/** Prefer verified live presence, then uncertainty over a stale offline assertion. */
export function mergeInventory(sources: readonly (readonly HandInventoryEntry[])[], complete: boolean): HandInventory {
  const entries = new Map<string, HandInventoryEntry>();
  const rank = (entry: HandInventoryEntry) => entry.online === true ? 2 : entry.online === null ? 1 : 0;
  for (const source of sources) for (const entry of source) {
    const previous = entries.get(entry.id);
    if (!previous || rank(entry) > rank(previous)) entries.set(entry.id, entry);
  }
  return { data: [...entries.values()].sort((a, b) => a.id.localeCompare(b.id)),
    coverage: "known_account_and_workspace", complete };
}

// Lifetime retention: no TTL evicts offline workspace identities. The cap bounds
// menu polling and storage. Overflow is durable and visibly fences completeness;
// it never silently represents a partial registry as a complete account list.
export const WORKSPACE_INVENTORY_SESSION_LIMIT = 64;
export const WORKSPACE_INVENTORY_CONCURRENCY = 8;
export const HAND_INVENTORY_DEADLINE_MS = 4_000;

export class WorkspaceHandRegistry {
  constructor(readonly storage: DurableObjectStorage) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS workspace_hand_inventory (
      session_id TEXT PRIMARY KEY, entries_json TEXT NOT NULL
    )`);
  }
  register(sessionId: string, entries: readonly HandInventoryEntry[]): boolean {
    if (entries.length === 0) return true;
    const present = this.storage.sql.exec("SELECT session_id FROM workspace_hand_inventory WHERE session_id=?", sessionId).toArray().length > 0;
    const count = this.storage.sql.exec<{ count: number }>("SELECT count(*) AS count FROM workspace_hand_inventory").one().count;
    if (!present && count >= WORKSPACE_INVENTORY_SESSION_LIMIT) {
      this.storage.kv.put("workspace_hand_inventory_overflow", true);
      return false;
    }
    this.storage.sql.exec(`INSERT INTO workspace_hand_inventory(session_id,entries_json) VALUES (?,?)
      ON CONFLICT(session_id) DO UPDATE SET entries_json=excluded.entries_json`, sessionId, JSON.stringify(entries));
    return true;
  }
  entries(): { sessionId: string; entries: HandInventoryEntry[] }[] {
    return this.storage.sql.exec<{ session_id: string; entries_json: string }>(
      "SELECT session_id,entries_json FROM workspace_hand_inventory ORDER BY session_id").toArray()
      .map(row => ({ sessionId: row.session_id, entries: JSON.parse(row.entries_json) }));
  }
  get complete(): boolean { return this.storage.kv.get("workspace_hand_inventory_overflow") !== true; }
}
