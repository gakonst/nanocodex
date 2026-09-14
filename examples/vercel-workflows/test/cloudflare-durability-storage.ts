import type { CloudflareDurableObjectStorage } from "nanocodex/durability/cloudflare";

type OwnerRow = { owner_id: string; fence: string };
type StateRow = { revision: string; payload: string };
type ChunkHeadRow = { revision: string; chunk_count: number };
type ChunkRow = {
  stateId: string;
  revision: string;
  chunk_index: number;
  payload: string;
};

/** In-memory Durable Object SQLite boundary used by the portability integration test. */
export function cloudflareDurabilityStorage(): CloudflareDurableObjectStorage {
  const owners = new Map<string, OwnerRow>();
  const states = new Map<string, StateRow>();
  const heads = new Map<string, ChunkHeadRow>();
  const chunks: ChunkRow[] = [];
  const storage = {
    sql: {
      exec(sql: string, ...args: unknown[]) {
        const stateId = args[0] as string;
        let rows: unknown[];
        if (sql.startsWith("CREATE TABLE")) {
          rows = [];
        } else if (sql.startsWith("PRAGMA table_info")) {
          rows = pragmaRows(sql);
        } else if (sql.startsWith("SELECT owner_id, fence FROM nanocodex_durable_owners")) {
          const stored = owners.get(stateId);
          rows = stored ? [stored] : [];
        } else if (sql.startsWith("INSERT INTO nanocodex_durable_owners")) {
          owners.set(stateId, { owner_id: args[1] as string, fence: args[2] as string });
          rows = [];
        } else if (sql.startsWith("SELECT revision FROM nanocodex_durable_states")) {
          const stored = states.get(stateId);
          rows = stored ? [{ revision: stored.revision }] : [];
        } else if (sql.startsWith("SELECT revision, payload FROM nanocodex_durable_states")) {
          const stored = states.get(stateId);
          rows = stored ? [stored] : [];
        } else if (sql.startsWith("INSERT INTO nanocodex_durable_states")) {
          states.set(stateId, { revision: args[1] as string, payload: args[2] as string });
          rows = [];
        } else if (sql.startsWith("DELETE FROM nanocodex_durable_chunk_heads")) {
          heads.delete(stateId);
          rows = [];
        } else if (sql.startsWith("INSERT INTO nanocodex_durable_chunk_heads")) {
          heads.set(stateId, { revision: args[1] as string, chunk_count: args[2] as number });
          rows = [];
        } else if (sql.startsWith("SELECT revision, chunk_count FROM nanocodex_durable_chunk_heads")) {
          const stored = heads.get(stateId);
          rows = stored ? [stored] : [];
        } else if (sql.startsWith("DELETE FROM nanocodex_durable_state_chunks")) {
          removeChunks(chunks, stateId);
          rows = [];
        } else if (sql.startsWith("INSERT INTO nanocodex_durable_state_chunks")) {
          chunks.push({
            stateId,
            revision: args[1] as string,
            chunk_index: args[2] as number,
            payload: args[3] as string,
          });
          rows = [];
        } else if (sql.startsWith(
          "SELECT revision, chunk_index, payload FROM nanocodex_durable_state_chunks",
        )) {
          rows = chunks.filter((chunk) => chunk.stateId === stateId);
        } else {
          throw new Error(`unexpected Cloudflare durability SQL: ${sql}`);
        }
        return { toArray: () => rows };
      },
    },
    transactionSync<Result>(callback: () => Result): Result {
      const snapshot = {
        owners: new Map(owners),
        states: new Map(states),
        heads: new Map(heads),
        chunks: chunks.map((chunk) => ({ ...chunk })),
      };
      try {
        return callback();
      } catch (error) {
        restoreMap(owners, snapshot.owners);
        restoreMap(states, snapshot.states);
        restoreMap(heads, snapshot.heads);
        chunks.splice(0, chunks.length, ...snapshot.chunks);
        throw error;
      }
    },
  };
  return storage as unknown as CloudflareDurableObjectStorage;
}

function pragmaRows(sql: string): object[] {
  if (sql.includes("nanocodex_durable_owners")) {
    return columns([["state_id", "TEXT", 0, 1], ["owner_id", "TEXT", 1, 0], ["fence", "TEXT", 1, 0]]);
  }
  if (sql.includes("nanocodex_durable_states")) {
    return columns([["state_id", "TEXT", 0, 1], ["revision", "TEXT", 1, 0], ["payload", "TEXT", 1, 0]]);
  }
  if (sql.includes("nanocodex_durable_chunk_heads")) {
    return columns([["state_id", "TEXT", 0, 1], ["revision", "TEXT", 1, 0], ["chunk_count", "INTEGER", 1, 0]]);
  }
  return columns([
    ["state_id", "TEXT", 1, 1],
    ["revision", "TEXT", 1, 2],
    ["chunk_index", "INTEGER", 1, 3],
    ["payload", "TEXT", 1, 0],
  ]);
}

function columns(shapes: Array<[string, string, number, number]>): object[] {
  return shapes.map(([name, type, notnull, pk], cid) => ({ cid, name, type, notnull, pk }));
}

function removeChunks(chunks: ChunkRow[], stateId: string): void {
  for (let index = chunks.length - 1; index >= 0; index -= 1) {
    if (chunks[index]!.stateId === stateId) chunks.splice(index, 1);
  }
}

function restoreMap<Key, Value>(target: Map<Key, Value>, source: Map<Key, Value>): void {
  target.clear();
  for (const [key, value] of source) target.set(key, value);
}
