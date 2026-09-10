// SQLite strings/rows are limited to 2 MB. This is storage placement, not an
// admission limit: one input may occupy any number of rows.
const CHUNK_CODE_UNITS = 256_000;
const REFERENCE_PREFIX = "chunks:";
type InputChunkTable = "managed_turn_input_chunks" | "managed_cron_input_chunks" | "durable_memory_content_chunks" | "managed_turn_terminal_chunks" | "managed_history_projection_chunks";

export function* inputChunks(input: string): Generator<string> {
  for (let offset = 0; offset < input.length;) {
    let end = Math.min(offset + CHUNK_CODE_UNITS, input.length);
    if (end < input.length && input.charCodeAt(end - 1) >= 0xd800
      && input.charCodeAt(end - 1) <= 0xdbff && input.charCodeAt(end) >= 0xdc00
      && input.charCodeAt(end) <= 0xdfff) end -= 1;
    yield input.slice(offset, end);
    offset = end;
  }
}

export function initializeTurnInputs(storage: DurableObjectStorage, table: InputChunkTable = "managed_turn_input_chunks"): void {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS ${table} (
    turn_id TEXT NOT NULL, chunk_index INTEGER NOT NULL, input_json TEXT NOT NULL,
    PRIMARY KEY (turn_id, chunk_index)
  )`);
}

/** Call inside the admission transaction; returned reference stays in the head. */
export function storeTurnInput(storage: DurableObjectStorage, id: string, input: string, table: InputChunkTable = "managed_turn_input_chunks"): string {
  if (input.length <= CHUNK_CODE_UNITS) return input;
  let count = 0;
  for (const chunk of inputChunks(input)) {
    storage.sql.exec(`INSERT INTO ${table} (turn_id, chunk_index, input_json)
      VALUES (?, ?, ?)`, id, count++, chunk);
  }
  return `${REFERENCE_PREFIX}${count}`;
}

export function readTurnInput(storage: DurableObjectStorage, id: string, input: string, table: InputChunkTable = "managed_turn_input_chunks"): string {
  if (!input.startsWith(REFERENCE_PREFIX)) return input;
  const count = Number(input.slice(REFERENCE_PREFIX.length));
  const chunks: string[] = [];
  for (const chunk of storage.sql.exec<{ chunk_index: number; input_json: string }>(
    `SELECT chunk_index, input_json FROM ${table} WHERE turn_id = ? ORDER BY chunk_index`, id,
  )) {
    if (chunk.chunk_index !== chunks.length) throw new Error(`managed turn ${id} has invalid input chunks`);
    chunks.push(chunk.input_json);
  }
  if (!Number.isSafeInteger(count) || count < 1 || chunks.length !== count) {
    throw new Error(`managed turn ${id} has missing input chunks`);
  }
  return chunks.join("");
}

/** Metadata scans never load bodies; dispatch/receipts load only their own turn. */
export function lazyTurnInput<T extends { id: string; input_json: string; terminal_json?: string | null }>(storage: DurableObjectStorage, row: T): T {
  for (const [field, table] of [
    ["input_json", "managed_turn_input_chunks"],
    ["terminal_json", "managed_turn_terminal_chunks"],
  ] as const) {
    const reference = row[field];
    let hydrated: string | undefined;
    if (typeof reference === "string" && reference.startsWith(REFERENCE_PREFIX)) Object.defineProperty(row, field, {
      enumerable: true,
      get: () => hydrated ??= readTurnInput(storage, row.id, reference, table),
    });
  }
  return row;
}
