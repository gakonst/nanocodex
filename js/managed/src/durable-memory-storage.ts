import { createHash } from "node:crypto";
import { normalizeMemoryIdentity } from "./durable-memory";
import { initializeTurnInputs, readTurnInput, storeTurnInput } from "./managed-turn-input";

const TABLE = "durable_memory_content_chunks";
export function memoryIdentityDigest(content: string): string {
  return createHash("sha256").update(normalizeMemoryIdentity(content)).digest("hex");
}

export function initializeMemoryContent(storage: DurableObjectStorage): void {
  initializeTurnInputs(storage, TABLE);
  storage.transactionSync(() => {
    const legacy = storage.sql.exec<{ name: string }>("PRAGMA table_info(durable_memories)")
      .toArray().some((column) => column.name === "content");
    // Build the new identity index separately: a legacy literal could itself
    // equal another record's SHA256. Updating a mixed-domain UNIQUE column in
    // place would otherwise make a valid scope impossible to migrate.
    const legacySequence = legacy ? storage.sql.exec<{ seq: number }>(
      "SELECT seq FROM sqlite_sequence WHERE name = 'durable_memories'",
    ).toArray()[0]?.seq ?? 0 : 0;
    if (legacy) storage.sql.exec(`DROP TRIGGER IF EXISTS durable_memories_ad;
      DROP INDEX IF EXISTS durable_memories_owner_team_id;
      DROP INDEX IF EXISTS durable_memories_owner_created;
      ALTER TABLE durable_memories RENAME TO durable_memories_legacy;`);
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS durable_memories (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      version INTEGER NOT NULL CHECK(version > 0), owner_team_id TEXT NOT NULL,
      content_json TEXT NOT NULL, identity_digest TEXT NOT NULL UNIQUE,
      created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
      last_scanned_at_ms INTEGER, scan_count INTEGER NOT NULL DEFAULT 0,
      last_used_at_ms INTEGER, use_count INTEGER NOT NULL DEFAULT 0, probation_until_ms INTEGER
    );
    CREATE INDEX IF NOT EXISTS durable_memories_owner_team_id ON durable_memories(owner_team_id, id);`);
    if (legacy) {
      for (const row of storage.sql.exec<{
        id: number; version: number; owner_team_id: string; content: string;
        created_at_ms: number; updated_at_ms: number; last_scanned_at_ms: number | null;
        scan_count: number; last_used_at_ms: number | null; use_count: number; probation_until_ms: number | null;
      }>("SELECT * FROM durable_memories_legacy")) storage.sql.exec(
        `INSERT INTO durable_memories VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
        row.id, row.version, row.owner_team_id,
        storeTurnInput(storage, String(row.id), JSON.stringify(row.content), TABLE), memoryIdentityDigest(row.content),
        row.created_at_ms, row.updated_at_ms, row.last_scanned_at_ms, row.scan_count,
        row.last_used_at_ms, row.use_count, row.probation_until_ms);
      storage.sql.exec("DROP TABLE durable_memories_legacy");
      // Preserve monotonically allocated keys even if the highest IDs were
      // deleted before the schema upgrade (including an empty scope).
      const sequence = storage.sql.exec<{ seq: number }>(
        "SELECT seq FROM sqlite_sequence WHERE name = 'durable_memories'",
      ).toArray()[0];
      if (sequence) storage.sql.exec("UPDATE sqlite_sequence SET seq = MAX(seq, ?) WHERE name = 'durable_memories'", legacySequence);
      else storage.sql.exec("INSERT INTO sqlite_sequence (name, seq) VALUES ('durable_memories', ?)", legacySequence);
    }
    storage.sql.exec(`CREATE TRIGGER IF NOT EXISTS durable_memories_ad AFTER DELETE ON durable_memories BEGIN
      DELETE FROM durable_memory_content_chunks WHERE turn_id = CAST(old.id AS TEXT);
    END;
    CREATE INDEX IF NOT EXISTS durable_memories_owner_created ON durable_memories(owner_team_id, created_at_ms, id);`);
  });
}

export function storeMemoryContent(storage: DurableObjectStorage, id: number, content: string): void {
  storage.sql.exec(`DELETE FROM ${TABLE} WHERE turn_id = ?`, String(id));
  storage.sql.exec("UPDATE durable_memories SET content_json = ? WHERE id = ?",
    storeTurnInput(storage, String(id), JSON.stringify(content), TABLE), id);
}

export function readMemoryContent(storage: DurableObjectStorage, row: { id: number; content_json: string }): string {
  const content: unknown = JSON.parse(readTurnInput(storage, String(row.id), row.content_json, TABLE));
  if (typeof content !== "string") throw new Error("invalid durable memory content");
  return content;
}
