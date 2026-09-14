const MAX_REVISION = 18_446_744_073_709_551_615n;
const MAX_REVISION_TEXT = String(MAX_REVISION);
const PORTABLE_FORMAT = "nanocodex-durability-state-v2";
const PORTABLE_PAGE_FORMAT = "nanocodex-durability-state-page-v2";
const PORTABLE_EXPORT_OWNER = "nanocodex-portable-export";
const PORTABLE_IMPORT_OWNER = "nanocodex-portable-import";
const DEFAULT_EXPORT_PAGE_SIZE = 256 * 1024;
const MAX_EXPORT_PAGE_SIZE = 1024 * 1024;

export const sqliteDurabilitySchema = Object.freeze([
  `CREATE TABLE IF NOT EXISTS nanocodex_durable_records (
     state_id TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
     PRIMARY KEY (state_id, key)
   ) WITHOUT ROWID`,
  `CREATE TABLE IF NOT EXISTS nanocodex_durable_owners (
     state_id TEXT PRIMARY KEY,
     owner_id TEXT NOT NULL,
     fence TEXT NOT NULL
   )`,
  `CREATE TABLE IF NOT EXISTS nanocodex_durable_states (
     state_id TEXT PRIMARY KEY,
     revision TEXT NOT NULL,
     payload TEXT NOT NULL
   )`,
]);

export function durabilityRevision(value) {
  return durabilityUint64(value, "revision");
}

/** Returns SHA-256 over the UTF-8 JSON tuple `[revision, payload]`. */
export async function durabilityStateDigest(state) {
  const exact = copyState(state);
  const encoded = new TextEncoder().encode(JSON.stringify([exact.revision, exact.payload]));
  const digest = await globalThis.crypto?.subtle?.digest("SHA-256", encoded);
  if (digest === undefined) {
    throw new Error("durability range portability requires Web Crypto SHA-256 support");
  }
  return `sha256:${[...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("")}`;
}

export class DurabilityImportConflictError extends Error {
  constructor(stateId, expectedRevision, actualRevision) {
    const id = JSON.stringify(stateId);
    super(expectedRevision === undefined
      ? `durability state ${id} already exists at the import destination`
      : expectedRevision === actualRevision
        ? `durability import expected the exact destination state at revision ${expectedRevision} for ${id}, but found a different payload lineage`
        : `durability import expected destination revision ${expectedRevision} for state ${id}, but found ${actualRevision}`);
    this.name = "DurabilityImportConflictError";
    this.expectedRevision = expectedRevision;
    this.actualRevision = actualRevision;
  }
}

/** Exports one deterministic page of the exact total state at `to`. */
export async function exportDurabilityStatePage(store, stateId, request) {
  requireId(stateId, "state");
  if (!store || typeof store.acquire !== "function") {
    throw new TypeError("durability export requires a state store");
  }
  const from = durabilityRevision(request?.from);
  let fromDigest;
  if (request?.fromDigest === undefined) {
    if (from !== "0") {
      throw new TypeError("nonzero durability export ranges require a from-state digest");
    }
    fromDigest = await durabilityStateDigest({ revision: "0", payload: null });
  }
  else {
    fromDigest = durabilityStateDigestText(request.fromDigest);
  }
  const requestedTo = request?.to === undefined ? undefined : durabilityRevision(request.to);
  const recordCursor = typeof request?.cursor === "string" && request.cursor.startsWith("v2:r:");
  const offset = recordCursor ? 0 : decodeExportCursor(request?.cursor);
  const limit = exportPageSize(request?.limit);
  const acquired = await store.acquire(stateId, { ownerId: PORTABLE_EXPORT_OWNER });
  exactObject(acquired, ["ownerId", "fence", "revision", "payload"], "durability export acquisition");
  if (acquired.ownerId !== PORTABLE_EXPORT_OWNER) {
    throw new TypeError("durability export acquisition returned a different owner ID");
  }
  durabilityFence(acquired.fence);
  const state = copyState({ revision: acquired.revision, payload: acquired.payload });
  const to = state.revision;
  if (requestedTo !== undefined && requestedTo !== to) {
    throw new DurabilityImportConflictError(stateId, requestedTo, to);
  }
  if (BigInt(from) >= BigInt(to)) {
    throw new RangeError("durability export from revision must be less than to revision");
  }
  const payload = state.payload ?? "";
  if (recordCursor) {
    const after = decodeURIComponent(request.cursor.slice(5));
    const records = checkedRecords(await store.scanRecords(stateId, after, 16));
    return Object.freeze({ format: PORTABLE_PAGE_FORMAT, stateId, from, fromDigest, to,
      cursor: request.cursor, nextCursor: records.length === 16 ? recordExportCursor(records.at(-1).key) : null,
      payloadLength: payload.length, payload: "", records,
    });
  }
  if (offset > payload.length) throw new TypeError("durability export cursor is out of range");
  let end = Math.min(offset + limit, payload.length);
  if (end < payload.length && end > offset && isHighSurrogate(payload.charCodeAt(end - 1))) end -= 1;
  const nextCursor = end < payload.length ? encodeExportCursor(end) : recordExportCursor("");
  return Object.freeze({
    format: PORTABLE_PAGE_FORMAT,
    stateId,
    from,
    fromDigest,
    to,
    cursor: encodeExportCursor(offset),
    nextCursor,
    payloadLength: payload.length,
    payload: payload.slice(offset, end),
    records: [],
  });
}

/** Imports contiguous cursor pages with a CAS guard on the destination's `from` revision. */
export async function importDurabilityStatePages(store, pages) {
  if (!store || typeof store.importState !== "function") {
    throw new TypeError("durability import requires a portable state store");
  }
  if (!pages || (typeof pages[Symbol.iterator] !== "function" && typeof pages[Symbol.asyncIterator] !== "function")) {
    throw new TypeError("durability import pages must be iterable");
  }
  let first;
  let cursor = encodeExportCursor(0);
  let payload = "";
  let complete = false;
  for await (const page of pages) {
    exactObject(
      page,
      [
        "format", "stateId", "from", "fromDigest", "to", "cursor", "nextCursor",
        "payloadLength", "payload", "records",
      ],
      "durability export page",
    );
    if (page.format !== PORTABLE_PAGE_FORMAT || typeof page.payload !== "string") {
      throw new TypeError("unsupported durability export page");
    }
    if (!Number.isSafeInteger(page.payloadLength) || page.payloadLength < 0) {
      throw new TypeError("durability export page payload length is invalid");
    }
    const identity = {
      stateId: requireId(page.stateId, "exported state"),
      from: durabilityRevision(page.from),
      fromDigest: durabilityStateDigestText(page.fromDigest),
      to: durabilityRevision(page.to),
      payloadLength: page.payloadLength,
    };
    first ??= identity;
    if (identity.stateId !== first.stateId || identity.from !== first.from
      || identity.fromDigest !== first.fromDigest
      || identity.to !== first.to || identity.payloadLength !== first.payloadLength) {
      throw new TypeError("durability export pages describe different revision ranges");
    }
    if (complete || page.cursor !== cursor) throw new TypeError("durability export pages are missing, duplicated, or out of order");
    const records = checkedRecords(page.records);
    if (page.cursor.startsWith("v2:r:")) {
      if (payload.length !== first.payloadLength || page.payload !== "") throw new TypeError("durability record pages preceded the complete head");
      let after = decodeURIComponent(page.cursor.slice(5));
      if (records.length > 16) throw new TypeError("durability record page exceeds its bound");
      for (const record of records) {
        if (record.key <= after) throw new TypeError("durability record pages are out of order");
        after = record.key;
      }
      if (page.nextCursor !== null && (records.length === 0 || page.nextCursor !== recordExportCursor(after))) throw new TypeError("invalid durability record cursor");
      await store.importRecords(first.stateId, records);
      complete = page.nextCursor === null;
    } else {
      if (records.length !== 0 || decodeExportCursor(page.cursor) !== payload.length) throw new TypeError("durability export pages are missing, duplicated, or out of order");
      payload += page.payload;
      if (payload.length > first.payloadLength) throw new TypeError("durability export pages exceed their declared payload length");
      const expected = payload.length === first.payloadLength ? recordExportCursor("") : encodeExportCursor(payload.length);
      if (page.nextCursor !== expected) throw new TypeError("durability export page cursor does not match its payload");
    }
    cursor = page.nextCursor;

  }
  if (!first || !complete) throw new TypeError("durability export pages are incomplete");
  if (BigInt(first.from) >= BigInt(first.to)) {
    throw new RangeError("durability import from revision must be less than to revision");
  }
  const state = copyState({ revision: first.to, payload: first.to === "0" ? null : payload });
  const current = copyState(await store.load(first.stateId));
  const currentDigest = await durabilityStateDigest(current);
  if (current.revision !== first.from || currentDigest !== first.fromDigest) {
    throw new DurabilityImportConflictError(first.stateId, first.from, current.revision);
  }
  const imported = await store.importState(first.stateId, state, {
    expectedRevision: first.from,
    expectedPayload: current.payload,
  });
  return copyState(imported);
}

/** Fences a source store and exports one coherent provider-neutral state archive. */
export async function exportDurabilityState(store, stateId, { headOnly = false } = {}) {
  requireId(stateId, "state");
  if (!store || typeof store.acquire !== "function") {
    throw new TypeError("durability export requires a state store");
  }
  const ownerId = PORTABLE_EXPORT_OWNER;
  const acquired = await store.acquire(stateId, { ownerId });
  exactObject(
    acquired,
    ["ownerId", "fence", "revision", "payload"],
    "durability export acquisition",
  );
  if (acquired.ownerId !== ownerId) {
    throw new TypeError("durability export acquisition returned a different owner ID");
  }
  durabilityFence(acquired.fence);
  const state = copyState({ revision: acquired.revision, payload: acquired.payload });
  const records = [];
  if (!headOnly) {
    let after = "";
    while (true) {
      const page = await store.scanRecords(stateId, after, 16);
      records.push(...page);
      if (page.length < 16) break;
      after = page.at(-1).key;
    }
  }
  return Object.freeze({ format: PORTABLE_FORMAT, stateId, ...state, records });
}

/** Restores an exact provider-neutral archive into an empty portable store. */
export async function importDurabilityState(store, archive) {
  if (!store || typeof store.importState !== "function") {
    throw new TypeError("durability import requires a portable state store");
  }
  exactObject(
    archive,
    ["format", "stateId", "revision", "payload", "records"],
    "durability export archive",
  );
  if (archive.format !== PORTABLE_FORMAT) {
    throw new TypeError(`unsupported durability export format: ${String(archive.format)}`);
  }
  requireId(archive.stateId, "exported state");
  const state = copyState({ revision: archive.revision, payload: archive.payload });
  const records = checkedRecords(archive.records);
  const imported = await store.importState(archive.stateId, state, { records });
  return copyState(imported);
}

function durabilityFence(value) {
  return durabilityUint64(value, "fence");
}

function durabilityUint64(value, field) {
  if (typeof value === "number" && (!Number.isSafeInteger(value) || value < 0)) {
    throw new TypeError(
      `durability ${field} numbers must be nonnegative safe integers; `
      + "use exact unsigned decimal text for larger values",
    );
  }
  if (typeof value !== "string" && typeof value !== "bigint" && typeof value !== "number") {
    throw new TypeError(`durability ${field} must be an unsigned 64-bit decimal string`);
  }
  const encoded = String(value);
  if (!/^(0|[1-9][0-9]*)$/.test(encoded) || BigInt(encoded) > MAX_REVISION) {
    throw new TypeError(`durability ${field} must be an unsigned 64-bit decimal string`);
  }
  return encoded;
}

export function createMemoryDurabilityStore(stateId, initial) {
  requireId(stateId, "state");
  const records = new Map();
  const states = new Map([[stateId, {
    state: copyState(initial ?? { revision: "0", payload: null }),
    owner: undefined,
  }]]);
  const select = (selected) => {
    requireId(selected, "state");
    const entry = states.get(selected);
    if (entry === undefined) throw new Error(`unknown durability state: ${selected}`);
    return entry;
  };
  return Object.freeze({
    stateId,
    scanRecords(selected, after = "", limit = 16) {
      return [...records].flatMap(([address, value]) => {
        const [state, key] = JSON.parse(address);
        return state === selected && key > after ? [{ key, value }] : [];
      }).sort((a, b) => a.key < b.key ? -1 : a.key > b.key ? 1 : 0).slice(0, limit);
    },
    importRecords(selected, incoming) {
      const checked = checkedRecords(incoming);
      for (const record of checked) {
        const previous = records.get(JSON.stringify([selected, record.key]));
        if (previous !== undefined && previous !== record.value) throw new Error("immutable record conflict");
      }
      for (const record of checked) records.set(JSON.stringify([selected, record.key]), record.value);
    },
    readRecord(selected, key) {
      return records.get(JSON.stringify([selected, key])) ?? null;
    },

    load(selected) {
      return select(selected).state;
    },
    acquire(selected, request) {
      requireId(selected, "state");
      const ownerId = requireId(request?.ownerId, "owner");
      let entry = states.get(selected);
      if (entry === undefined) {
        entry = {
          state: copyState({ revision: "0", payload: null }),
          owner: Object.freeze({ ownerId, fence: "1" }),
        };
        states.set(selected, entry);
        return acquiredState(entry.owner, entry.state);
      }
      if (entry.owner?.ownerId === ownerId) return acquiredState(entry.owner, entry.state);
      const previousFence = entry.owner?.fence ?? "0";
      if (previousFence === MAX_REVISION_TEXT) {
        throw new RangeError("in-memory durability fence overflow");
      }
      entry.owner = Object.freeze({
        ownerId,
        fence: durabilityFence(BigInt(previousFence) + 1n),
      });
      return acquiredState(entry.owner, entry.state);
    },
    replace(selected, request) {
      const entry = select(selected);
      const ownerId = requireId(request?.ownerId, "owner");
      const fence = durabilityFence(request?.fence);
      if (ownerId !== entry.owner?.ownerId || fence !== entry.owner.fence) {
        return { status: "fenced" };
      }
      const expectedRevision = durabilityRevision(request?.expectedRevision);
      if (expectedRevision !== entry.state.revision) {
        if (entry.state.revision === nextRevision(expectedRevision)
          && typeof request?.payload === "string"
          && entry.state.payload === request.payload) {
          return { status: "replaced", revision: entry.state.revision };
        }
        return { status: "conflict", actualRevision: entry.state.revision };
      }
      if (expectedRevision === MAX_REVISION_TEXT) {
        return { status: "not_committed", message: "in-memory durability revision overflow" };
      }
      const payload = requirePayload(request?.payload);
      const revision = durabilityRevision(BigInt(expectedRevision) + 1n);
      for (const record of request.records) {
        const address = JSON.stringify([selected, record.key]);
        const previous = records.get(address);
        if (previous !== undefined && previous !== record.value) throw new Error("immutable record conflict");
      }
      for (const record of request.records) records.set(JSON.stringify([selected, record.key]), record.value);
      entry.state = Object.freeze({ revision, payload });
      return { status: "replaced", revision };
    },
    importState(selected, imported, options) {
      requireId(selected, "state");
      const next = copyState(imported);
      const expectedRevision = options?.expectedRevision === undefined
        ? undefined
        : durabilityRevision(options.expectedRevision);
      const expectedPayload = expectedImportPayload(options, expectedRevision);
      const existing = states.get(selected);
      const entry = existing ?? {
        state: copyState({ revision: "0", payload: null }),
        owner: undefined,
      };
      if (expectedRevision === undefined
        ? entry.owner !== undefined || entry.state.revision !== "0"
        : entry.state.revision !== expectedRevision
          || expectedPayload.present && entry.state.payload !== expectedPayload.value) {
        throw new DurabilityImportConflictError(selected, expectedRevision, entry.state.revision);
      }
      const incoming = checkedRecords(options?.records ?? []);
      for (const record of incoming) {
        const previous = records.get(JSON.stringify([selected, record.key]));
        if (previous !== undefined && previous !== record.value) throw new Error("immutable record conflict");
      }
      const owner = Object.freeze({
        ownerId: PORTABLE_IMPORT_OWNER,
        fence: durabilityFence(BigInt(entry.owner?.fence ?? "0") + 1n),
      });
      if (existing === undefined) states.set(selected, entry);
      for (const record of incoming) records.set(JSON.stringify([selected, record.key]), record.value);
      entry.owner = owner;
      entry.state = next;
      return entry.state;
    },
    snapshot() {
      return states.get(stateId).state;
    },
  });
}

export function createSqliteDurabilityStore(options) {
  if (!options || typeof options.transaction !== "function") {
    throw new TypeError("SQLite durability requires a transaction function");
  }
  return Object.freeze({
    scanRecords(stateId, after = "", limit = 16) {
      if (!Number.isInteger(limit) || limit < 1 || limit > 16) throw new TypeError("invalid durability record page size");
      return options.transaction((query) => query(
        "SELECT key, value FROM nanocodex_durable_records WHERE state_id = ? AND key > ? ORDER BY key LIMIT ?", [stateId, after, limit],
      ));
    },
    importRecords(stateId, incoming) {
      const records = checkedRecords(incoming);
      return options.transaction((query) => {
        let pending;
        for (const record of records) pending = mapMaybePromise(pending, () => mapMaybePromise(
          query("SELECT value FROM nanocodex_durable_records WHERE state_id = ? AND key = ?", [stateId, record.key]),
          (rows) => {
            if (rows.length && rows[0].value !== record.value) throw new Error("immutable record conflict");
            return query("INSERT INTO nanocodex_durable_records (state_id, key, value) VALUES (?, ?, ?) ON CONFLICT (state_id, key) DO NOTHING", [stateId, record.key, record.value]);
          },
        ));
        return pending;
      });
    },
    readRecords(stateId, keys) {
      if (!Array.isArray(keys) || keys.length > 16) throw new TypeError("durability record batch exceeds 16 records");
      if (keys.length === 0) return [];
      return options.transaction((query) => mapMaybePromise(
        query(`SELECT key, value FROM nanocodex_durable_records WHERE state_id = ? AND key IN (${keys.map(() => "?").join(",")})`, [stateId, ...keys]),
        (rows) => { const records = new Map(rows.map(({ key, value }) => [key, value])); return keys.map((key) => records.get(key) ?? null); },
      ));
    },

    readRecord(stateId, key) {
      return options.transaction((query) => mapMaybePromise(
        query("SELECT value FROM nanocodex_durable_records WHERE state_id = ? AND key = ?", [stateId, key]),
        (rows) => rows[0]?.value ?? null,
      ));
    },

    load(stateId) {
      requireId(stateId, "state");
      return options.transaction((query) => loadSqliteState(query, stateId));
    },
    acquire(stateId, request) {
      requireId(stateId, "state");
      const ownerId = requireId(request?.ownerId, "owner");
      return options.transaction((query) => mapMaybePromise(
        query(
          "SELECT owner_id, fence FROM nanocodex_durable_owners WHERE state_id = ?",
          [stateId],
        ),
        (owners) => {
          exactRows(owners, 1, "SQLite durability owner");
          if (owners.length === 1) exactObject(owners[0], ["owner_id", "fence"], "SQLite durability owner");
          if (owners[0]?.owner_id === ownerId) {
            return mapMaybePromise(
              loadSqliteState(query, stateId),
              (state) => acquiredState({ ownerId, fence: durabilityFence(owners[0].fence) }, state),
            );
          }
          const previousFence = durabilityFence(owners[0]?.fence ?? "0");
          if (previousFence === MAX_REVISION_TEXT) {
            throw new RangeError("SQLite durability fence overflow");
          }
          const fence = durabilityFence(BigInt(previousFence) + 1n);
          return mapMaybePromise(
            query(
              `INSERT INTO nanocodex_durable_owners (state_id, owner_id, fence) VALUES (?, ?, ?)
               ON CONFLICT (state_id) DO UPDATE
               SET owner_id = excluded.owner_id, fence = excluded.fence`,
              [stateId, ownerId, fence],
            ),
            () => mapMaybePromise(
              loadSqliteState(query, stateId),
              (state) => acquiredState({ ownerId, fence }, state),
            ),
          );
        },
      ));
    },
    replace(stateId, request) {
      requireId(stateId, "state");
      const ownerId = requireId(request?.ownerId, "owner");
      const fence = durabilityFence(request?.fence);
      return options.transaction((query) => mapMaybePromise(
        query(
          "SELECT owner_id, fence FROM nanocodex_durable_owners WHERE state_id = ?",
          [stateId],
        ),
        (owners) => {
          exactRows(owners, 1, "SQLite durability owner");
          if (owners.length === 1) exactObject(owners[0], ["owner_id", "fence"], "SQLite durability owner");
          const storedOwner = owners[0];
          if (
            storedOwner?.owner_id !== ownerId
            || durabilityFence(storedOwner?.fence ?? "0") !== fence
          ) {
            return { status: "fenced" };
          }
          const expectedRevision = durabilityRevision(request?.expectedRevision);
          // The normal write needs only the revision. Loading the previous
          // payload also hydrates every Cloudflare chunk, doubling live state
          // memory while the replacement is already held by Rust and JS.
          return mapMaybePromise(query(
            "SELECT revision FROM nanocodex_durable_states WHERE state_id = ?",
            [stateId],
          ), (rows) => {
            exactRows(rows, 1, "SQLite durability revision");
            if (rows.length === 1) exactObject(rows[0], ["revision"], "SQLite durability revision");
            const actualRevision = durabilityRevision(rows[0]?.revision ?? "0");
            if (actualRevision !== expectedRevision) {
              if (actualRevision === nextRevision(expectedRevision)
                && typeof request?.payload === "string") {
                // A lost acknowledgement must still replay only the exact
                // committed payload, under the same transaction and fence.
                return mapMaybePromise(loadSqliteState(query, stateId), (state) =>
                  state.payload === request.payload
                    ? { status: "replaced", revision: actualRevision }
                    : { status: "conflict", actualRevision });
              }
              return { status: "conflict", actualRevision };
            }
            if (expectedRevision === MAX_REVISION_TEXT) {
              return { status: "not_committed", message: "SQLite durability revision overflow" };
            }
            const payload = requirePayload(request?.payload);
            const revision = durabilityRevision(BigInt(expectedRevision) + 1n);
            let pending;
            for (const record of request.records) {
              pending = mapMaybePromise(pending, () => query(
                `INSERT INTO nanocodex_durable_records (state_id, key, value) VALUES (?, ?, ?)
                 ON CONFLICT (state_id, key) DO NOTHING`, [stateId, record.key, record.value],
              ));
            }
            return mapMaybePromise(pending, () => mapMaybePromise(
              query(
                `INSERT INTO nanocodex_durable_states (state_id, revision, payload) VALUES (?, ?, ?)
                 ON CONFLICT (state_id) DO UPDATE
                 SET revision = excluded.revision, payload = excluded.payload`,
                [stateId, revision, payload],
              ),
              () => ({ status: "replaced", revision }),
            ));
          });
        },
      ));
    },
    importState(stateId, imported, importOptions) {
      requireId(stateId, "state");
      const state = copyState(imported);
      const expectedRevision = importOptions?.expectedRevision === undefined
        ? undefined
        : durabilityRevision(importOptions.expectedRevision);
      const expectedPayload = expectedImportPayload(importOptions, expectedRevision);
      return options.transaction((query) => mapMaybePromise(
        query(
          "SELECT owner_id, fence FROM nanocodex_durable_owners WHERE state_id = ?",
          [stateId],
        ),
        (owners) => {
          exactRows(owners, 1, "SQLite durability owner");
          if (owners.length === 1) {
            exactObject(owners[0], ["owner_id", "fence"], "SQLite durability owner");
          }
          return mapMaybePromise(
            query(
              "SELECT revision, payload FROM nanocodex_durable_states WHERE state_id = ?",
              [stateId],
            ),
            (states) => {
              exactRows(states, 1, "SQLite durability state");
              const current = states.length === 0
                ? Object.freeze({ revision: "0", payload: null })
                : copyState({ revision: states[0].revision, payload: states[0].payload });
              if (expectedRevision === undefined
                ? owners.length !== 0 || states.length !== 0
                : current.revision !== expectedRevision
                  || expectedPayload.present && current.payload !== expectedPayload.value) {
                throw new DurabilityImportConflictError(stateId, expectedRevision, current.revision);
              }
              const previousFence = durabilityFence(owners[0]?.fence ?? "0");
              if (previousFence === MAX_REVISION_TEXT) throw new RangeError("SQLite durability fence overflow");
              const fence = durabilityFence(BigInt(previousFence) + 1n);
              let staged;
              for (const record of checkedRecords(importOptions?.records ?? [])) staged = mapMaybePromise(staged, () => query(
                "INSERT INTO nanocodex_durable_records (state_id, key, value) VALUES (?, ?, ?) ON CONFLICT (state_id, key) DO NOTHING",
                [stateId, record.key, record.value],
              ));
              return mapMaybePromise(staged, () => mapMaybePromise(
                query(
                  `INSERT INTO nanocodex_durable_owners (state_id, owner_id, fence) VALUES (?, ?, ?)
                   ON CONFLICT (state_id) DO UPDATE
                   SET owner_id = excluded.owner_id, fence = excluded.fence`,
                  [stateId, PORTABLE_IMPORT_OWNER, fence],
                ),
                () => state.revision === "0"
                  ? state
                  : mapMaybePromise(
                    query(
                      `INSERT INTO nanocodex_durable_states (state_id, revision, payload) VALUES (?, ?, ?)
                       ON CONFLICT (state_id) DO UPDATE
                       SET revision = excluded.revision, payload = excluded.payload`,
                      [stateId, state.revision, state.payload],
                    ),
                    () => state,
                  ),
              ));
            },
          );
        },
      ));
    },
  });
}

function nextRevision(revision) {
  return revision === MAX_REVISION_TEXT ? undefined : durabilityRevision(BigInt(revision) + 1n);
}

function exportPageSize(value) {
  if (value === undefined) return DEFAULT_EXPORT_PAGE_SIZE;
  if (!Number.isSafeInteger(value) || value < 1 || value > MAX_EXPORT_PAGE_SIZE) {
    throw new TypeError(`durability export page limit must be an integer from 1 to ${MAX_EXPORT_PAGE_SIZE}`);
  }
  return value;
}

function recordExportCursor(key) { return `v2:r:${encodeURIComponent(key)}`; }

function encodeExportCursor(offset) {
  return `v2:${offset}`;
}

function decodeExportCursor(value) {
  if (value === undefined) return 0;
  if (typeof value !== "string" || !/^v2:(0|[1-9][0-9]*)$/.test(value)) {
    throw new TypeError("durability export cursor is invalid");
  }
  const offset = Number(value.slice(3));
  if (!Number.isSafeInteger(offset)) throw new TypeError("durability export cursor is invalid");
  return offset;
}

function isHighSurrogate(value) {
  return value >= 0xd800 && value <= 0xdbff;
}

function durabilityStateDigestText(value) {
  if (typeof value !== "string" || !/^sha256:[0-9a-f]{64}$/.test(value)) {
    throw new TypeError("durability export page from-state digest is invalid");
  }
  return value;
}

function expectedImportPayload(options, expectedRevision) {
  const present = options?.expectedPayload !== undefined;
  if (!present) return { present: false, value: undefined };
  if (expectedRevision === undefined) {
    throw new TypeError("durability import expected payload requires an expected revision");
  }
  const expected = copyState({ revision: expectedRevision, payload: options.expectedPayload });
  return { present: true, value: expected.payload };
}

function loadSqliteState(query, stateId) {
  return mapMaybePromise(
    query(
      "SELECT revision, payload FROM nanocodex_durable_states WHERE state_id = ?",
      [stateId],
    ),
    (rows) => {
      exactRows(rows, 1, "SQLite durability state");
      if (rows.length === 0) return Object.freeze({ revision: "0", payload: null });
      exactObject(rows[0], ["revision", "payload"], "SQLite durability state");
      return copyState({ revision: rows[0].revision, payload: rows[0].payload });
    },
  );
}

function mapMaybePromise(value, mapper) {
  return value && typeof value.then === "function" ? value.then(mapper) : mapper(value);
}

function copyState(state) {
  if (!state || typeof state !== "object" || Array.isArray(state)) {
    throw new TypeError("durability state must be an object");
  }
  const keys = Object.keys(state).sort();
  if (keys.length !== 2 || keys[0] !== "payload" || keys[1] !== "revision") {
    throw new TypeError("durability state must contain exactly payload and revision");
  }
  const revision = durabilityRevision(state.revision);
  const payload = state.payload === null ? null : requirePayload(state.payload);
  if ((revision === "0") !== (payload === null)) {
    throw new TypeError("durability state payload must be null exactly at revision zero");
  }
  return Object.freeze({ revision, payload });
}

function acquiredState(owner, state) {
  return Object.freeze({
    ownerId: owner.ownerId,
    fence: owner.fence,
    revision: state.revision,
    payload: state.payload,
  });
}

function requireId(value, kind) {
  if (typeof value !== "string" || !value.trim()) {
    throw new TypeError(`durability ${kind} ID must be a non-empty string`);
  }
  return value;
}

function checkedRecords(records) {
  if (!Array.isArray(records)) throw new TypeError("durability records must be an array");
  const seen = new Map();
  for (const record of records) {
    exactObject(record, ["key", "value"], "durability record");
    requireId(record.key, "record");
    requirePayload(record.value);
    if (seen.has(record.key) && seen.get(record.key) !== record.value) throw new Error("immutable record conflict");
    seen.set(record.key, record.value);
  }
  return [...seen].map(([key, value]) => ({ key, value }));
}

function requirePayload(value) {
  if (typeof value !== "string") {
    throw new TypeError("durability payload must be a string");
  }
  return value;
}

function exactRows(rows, maximum, label) {
  if (!Array.isArray(rows) || rows.length > maximum) {
    throw new TypeError(`${label} query returned an invalid row set`);
  }
}

function exactObject(value, expected, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError(`${label} row must be an object`);
  }
  const actual = Object.keys(value).sort();
  const required = [...expected].sort();
  if (actual.length !== required.length || actual.some((key, index) => key !== required[index])) {
    throw new TypeError(`${label} row has an invalid shape`);
  }
}
