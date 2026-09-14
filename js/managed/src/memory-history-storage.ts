import { inputChunks } from "./managed-turn-input";

// Overlap keeps the longest accepted history query searchable across a storage
// boundary. It is not extra conversation text: reads remove it exactly.
const SEARCH_OVERLAP = 4_096;
export type HistorySegment = {
  segment_id: string;
  turn_segment_id: string;
  field: "user" | "assistant";
  chunk_index: number;
  overlap: number;
  content: string;
  ai_item_id: string | null;
};

export function initializeHistoryStorage(storage: DurableObjectStorage, indexAi: boolean): void {
  const legacy = storage.sql.exec<{ name: string }>("PRAGMA table_info(memory_turns)")
    .toArray().some((column) => column.name === "user_text");
  storage.transactionSync(() => {
    if (legacy) storage.sql.exec(`
      DROP TRIGGER IF EXISTS memory_turns_ai;
      DROP TRIGGER IF EXISTS memory_turns_ad;
      DROP TRIGGER IF EXISTS memory_turns_au;
      DROP TABLE IF EXISTS memory_turns_fts;
      DROP INDEX IF EXISTS memory_turns_thread_created;
      ALTER TABLE memory_turns RENAME TO memory_turns_legacy;
      ALTER TABLE memory_ai_outbox RENAME TO memory_ai_outbox_legacy;
    `);
    storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS memory_turns (
        segment_id TEXT PRIMARY KEY, thread_id TEXT NOT NULL, turn_id TEXT NOT NULL,
        source_cursor INTEGER NOT NULL, created_at INTEGER NOT NULL,
        FOREIGN KEY (thread_id) REFERENCES memory_threads(thread_id) ON DELETE CASCADE,
        UNIQUE (thread_id, turn_id)
      );
      CREATE INDEX IF NOT EXISTS memory_turns_thread_created ON memory_turns(thread_id, created_at);
      CREATE TABLE IF NOT EXISTS memory_segments (
        segment_id TEXT PRIMARY KEY, turn_segment_id TEXT NOT NULL,
        field TEXT NOT NULL CHECK(field IN ('user', 'assistant')),
        chunk_index INTEGER NOT NULL, overlap INTEGER NOT NULL, content TEXT NOT NULL,
        ai_item_id TEXT,
        FOREIGN KEY (turn_segment_id) REFERENCES memory_turns(segment_id) ON DELETE CASCADE,
        UNIQUE(turn_segment_id, field, chunk_index)
      );
      CREATE TABLE IF NOT EXISTS memory_ai_outbox (
        operation_id TEXT PRIMARY KEY,
        operation TEXT NOT NULL CHECK(operation IN ('upsert', 'delete')),
        segment_id TEXT NOT NULL, ai_item_id TEXT,
        attempt_count INTEGER NOT NULL DEFAULT 0, retry_at INTEGER NOT NULL DEFAULT 0
      );
      CREATE INDEX IF NOT EXISTS memory_ai_outbox_retry ON memory_ai_outbox(retry_at);
      CREATE VIRTUAL TABLE IF NOT EXISTS memory_segments_fts USING fts5(
        content, content='memory_segments', content_rowid='rowid', tokenize='unicode61'
      );
      CREATE TRIGGER IF NOT EXISTS memory_segments_ai AFTER INSERT ON memory_segments BEGIN
        INSERT INTO memory_segments_fts(rowid, content) VALUES (new.rowid, new.content);
      END;
      CREATE TRIGGER IF NOT EXISTS memory_segments_ad AFTER DELETE ON memory_segments BEGIN
        INSERT INTO memory_segments_fts(memory_segments_fts, rowid, content)
          VALUES ('delete', old.rowid, old.content);
      END;
    `);
    if (!legacy) return;
    // One old row at a time: never hydrate all historical conversations during
    // a cold start. The transaction keeps schema + content + outbox atomic.
    // This one-time conversion has bounded JS memory but CPU proportional to
    // existing history; the physical platform event CPU budget still applies.
    for (const row of storage.sql.exec<{
      segment_id: string; thread_id: string; turn_id: string; source_cursor: number;
      created_at: number; user_text: string; assistant_text: string; ai_item_id: string | null;
    }>(`SELECT segment_id, thread_id, turn_id, source_cursor, created_at,
               user_text, assistant_text, ai_item_id FROM memory_turns_legacy`)) {
      storage.sql.exec(`INSERT INTO memory_turns VALUES (?, ?, ?, ?, ?)`,
        row.segment_id, row.thread_id, row.turn_id, row.source_cursor, row.created_at);
      storeHistorySegments(storage, row.segment_id, row.user_text, row.assistant_text, indexAi);
      if (indexAi) queueHistorySegmentDelete(storage, row.segment_id, row.ai_item_id);
    }
    if (indexAi) for (const row of storage.sql.exec<{ segment_id: string; ai_item_id: string | null }>(
      "SELECT segment_id, ai_item_id FROM memory_ai_outbox_legacy",
    )) queueHistorySegmentDelete(storage, row.segment_id, row.ai_item_id);
    storage.sql.exec("DROP TABLE memory_turns_legacy; DROP TABLE memory_ai_outbox_legacy;");
  });
}

/** Within the projection transaction. Each immutable segment is also its AI job. */
export function storeHistorySegments(
  storage: DurableObjectStorage, turnId: string, user: string, assistant: string, indexAi: boolean,
): void {
  for (const [field, text] of [["user", user], ["assistant", assistant]] as const) {
    let previous = "";
    let index = 0;
    for (const chunk of inputChunks(text)) {
      const segmentId = crypto.randomUUID();
      storage.sql.exec(`INSERT INTO memory_segments
        (segment_id, turn_segment_id, field, chunk_index, overlap, content)
        VALUES (?, ?, ?, ?, ?, ?)`,
      segmentId, turnId, field, index++, previous.length, previous + chunk);
      if (indexAi) storage.sql.exec(`INSERT INTO memory_ai_outbox
        (operation_id, operation, segment_id) VALUES (?, 'upsert', ?)`,
      `upsert:${segmentId}`, segmentId);
      previous = chunk.slice(-SEARCH_OVERLAP);
      // Avoid splitting a surrogate pair in the search-only overlap prefix.
      if (/^[\uDC00-\uDFFF]/u.test(previous)) previous = previous.slice(1);
    }
  }
}

export function readHistoryText(storage: DurableObjectStorage, turnId: string, field: "user" | "assistant"): string {
  const parts: string[] = [];
  for (const row of storage.sql.exec<Pick<HistorySegment, "chunk_index" | "overlap" | "content">>(
    `SELECT chunk_index, overlap, content FROM memory_segments
     WHERE turn_segment_id = ? AND field = ? ORDER BY chunk_index`, turnId, field,
  )) {
    if (row.chunk_index !== parts.length) throw new Error("history segment sequence is incomplete");
    parts.push(row.content.slice(row.overlap));
  }
  return parts.join("");
}

export function queueHistorySegmentDelete(storage: DurableObjectStorage, id: string, aiItemId: string | null): void {
  storage.sql.exec("DELETE FROM memory_ai_outbox WHERE operation_id = ?", `upsert:${id}`);
  storage.sql.exec(`INSERT INTO memory_ai_outbox (operation_id, operation, segment_id, ai_item_id)
    VALUES (?, 'delete', ?, ?) ON CONFLICT(operation_id) DO NOTHING`, `delete:${id}`, id, aiItemId);
}

export function deleteHistorySegments(storage: DurableObjectStorage, turnId: string, indexAi: boolean): void {
  if (indexAi) for (const row of storage.sql.exec<{ segment_id: string; ai_item_id: string | null }>(
    "SELECT segment_id, ai_item_id FROM memory_segments WHERE turn_segment_id = ?", turnId,
  )) queueHistorySegmentDelete(storage, row.segment_id, row.ai_item_id);
  storage.sql.exec("DELETE FROM memory_segments WHERE turn_segment_id = ?", turnId);
}
