import { createSqliteDurabilityStore, sqliteDurabilitySchema } from "./durability-store.mjs";

const DIRECT_PAYLOAD_BYTES = 1_000_000;
const PAYLOAD_CHUNK_CODE_UNITS = 256_000;
const encoder = new TextEncoder();

const cloudflareDurabilitySchema = Object.freeze([
  ...sqliteDurabilitySchema,
  `CREATE TABLE IF NOT EXISTS nanocodex_durable_chunk_heads (
     state_id TEXT PRIMARY KEY,
     revision TEXT NOT NULL,
     chunk_count INTEGER NOT NULL CHECK (chunk_count > 0)
   )`,
  `CREATE TABLE IF NOT EXISTS nanocodex_durable_state_chunks (
     state_id TEXT NOT NULL,
     revision TEXT NOT NULL,
     chunk_index INTEGER NOT NULL,
     payload TEXT NOT NULL,
     PRIMARY KEY (state_id, revision, chunk_index)
   )`,
]);

/** Adapts one Cloudflare Durable Object's colocated SQLite to Nanocodex. */
export function createCloudflareDurabilityStore(storage) {
  if (
    !storage?.sql
    || typeof storage.sql.exec !== "function"
    || typeof storage.transactionSync !== "function"
  ) {
    throw new TypeError("Cloudflare durability requires Durable Object storage with SQLite");
  }
  const raw = (sql, args) => {
    const cursor = storage.sql.exec(sql, ...args);
    if (typeof cursor?.[Symbol.iterator] !== "function") return cursor.toArray();
    return [...cursor];
  };
  for (const statement of cloudflareDurabilitySchema) storage.sql.exec(statement);
  validateSchema(raw);
  const query = (sql, args) => {
    if (sql.startsWith("INSERT INTO nanocodex_durable_states")) {
      return replaceState(raw, sql, args);
    }
    const rows = raw(sql, args);
    return sql.startsWith("SELECT revision, payload FROM nanocodex_durable_states")
      ? hydrateState(raw, args[0], rows)
      : rows;
  };
  return createSqliteDurabilityStore({
    transaction: (callback) => storage.transactionSync(() => callback(query)),
  });
}

function validateSchema(query) {
  const tables = [
    ["nanocodex_durable_owners", [
      ["state_id", "TEXT", 0, 1], ["owner_id", "TEXT", 1, 0], ["fence", "TEXT", 1, 0],
    ]],
    ["nanocodex_durable_states", [
      ["state_id", "TEXT", 0, 1], ["revision", "TEXT", 1, 0], ["payload", "TEXT", 1, 0],
    ]],
    ["nanocodex_durable_chunk_heads", [
      ["state_id", "TEXT", 0, 1], ["revision", "TEXT", 1, 0], ["chunk_count", "INTEGER", 1, 0],
    ]],
    ["nanocodex_durable_state_chunks", [
      ["state_id", "TEXT", 1, 1], ["revision", "TEXT", 1, 2],
      ["chunk_index", "INTEGER", 1, 3], ["payload", "TEXT", 1, 0],
    ]],
  ];
  for (const [table, expected] of tables) {
    const rows = query(`PRAGMA table_info('${table}')`, []);
    const valid = rows.length === expected.length && rows.every((row, index) => {
      const shape = expected[index];
      return row.name === shape[0]
        && String(row.type).toUpperCase() === shape[1]
        && Number(row.notnull) === shape[2]
        && Number(row.pk) === shape[3];
    });
    if (!valid) {
      throw new Error(`incompatible Cloudflare durability schema for ${table}; recreate it`);
    }
  }
}

function replaceState(query, sql, [stateId, revision, payload]) {
  if (typeof payload !== "string") {
    throw new TypeError("durability state payload must be a string");
  }
  query("DELETE FROM nanocodex_durable_chunk_heads WHERE state_id = ?", [stateId]);
  query("DELETE FROM nanocodex_durable_state_chunks WHERE state_id = ?", [stateId]);
  // UTF-8 needs at least one byte per UTF-16 code unit. Do not allocate
  // another full-state byte buffer just to decide an already-large payload
  // needs chunks. The remaining encoding is bounded by the inline threshold.
  if (payload.length <= DIRECT_PAYLOAD_BYTES
    && encoder.encode(payload).byteLength <= DIRECT_PAYLOAD_BYTES) {
    return query(sql, [stateId, revision, payload]);
  }
  const result = query(sql, [stateId, revision, ""]);
  const chunks = payloadChunks(payload);
  query(
    `INSERT INTO nanocodex_durable_chunk_heads (state_id, revision, chunk_count)
     VALUES (?, ?, ?)`,
    [stateId, revision, chunks.length],
  );
  for (let index = 0; index < chunks.length; index += 1) {
    query(
      `INSERT INTO nanocodex_durable_state_chunks
       (state_id, revision, chunk_index, payload) VALUES (?, ?, ?, ?)`,
      [stateId, revision, index, chunks[index]],
    );
  }
  return result;
}

function hydrateState(query, stateId, rows) {
  if (rows.length === 0) return rows;
  if (rows.length !== 1) throw new Error("Cloudflare durability retained duplicate states");
  const row = rows[0];
  const head = query(
    `SELECT revision, chunk_count FROM nanocodex_durable_chunk_heads
     WHERE state_id = ?`,
    [stateId],
  )[0];
  if (!head) return rows;
  if (head.revision !== row.revision || row.payload !== "") {
    throw new Error("invalid Cloudflare durability chunk head");
  }
  const chunks = query(
    `SELECT revision, chunk_index, payload FROM nanocodex_durable_state_chunks
     WHERE state_id = ? ORDER BY chunk_index`,
    [stateId],
  );
  if (chunks.length !== head.chunk_count) {
    throw new Error(`missing Cloudflare durability chunks for revision ${row.revision}`);
  }
  for (let index = 0; index < chunks.length; index += 1) {
    const chunk = chunks[index];
    if (chunk.revision !== row.revision || chunk.chunk_index !== index
      || typeof chunk.payload !== "string") {
      throw new Error(`invalid Cloudflare durability chunks for revision ${row.revision}`);
    }
  }
  return [{ ...row, payload: chunks.map((chunk) => chunk.payload).join("") }];
}

function payloadChunks(payload) {
  const chunks = [];
  for (let offset = 0; offset < payload.length;) {
    let end = Math.min(offset + PAYLOAD_CHUNK_CODE_UNITS, payload.length);
    if (
      end < payload.length
      && isHighSurrogate(payload.charCodeAt(end - 1))
      && isLowSurrogate(payload.charCodeAt(end))
    ) {
      end -= 1;
    }
    chunks.push(payload.slice(offset, end));
    offset = end;
  }
  return chunks;
}

function isHighSurrogate(codeUnit) {
  return codeUnit >= 0xd800 && codeUnit <= 0xdbff;
}

function isLowSurrogate(codeUnit) {
  return codeUnit >= 0xdc00 && codeUnit <= 0xdfff;
}
