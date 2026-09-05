import assert from "node:assert/strict";
import test from "node:test";

import {
  DurabilityImportConflictError,
  sqliteDurabilitySchema,
} from "nanocodex/durability";
import { createCloudflareDurabilityStore } from "nanocodex/durability/cloudflare";

test("Cloudflare durability stores one state and chunks only oversized payloads", () => {
  const owners = new Map();
  const states = new Map();
  const heads = new Map();
  const chunks = [];
  const schema = [];
  let transactions = 0;
  let payloadReads = 0;
  const storage = {
    sql: {
      exec(sql, ...args) {
        if (args.some((value) => typeof value === "string" && Buffer.byteLength(value) > 2_000_000)) {
          throw new Error("string or blob too big: SQLITE_TOOBIG");
        }
        const [stateId, revision, payload] = args;
        let rows;
        if (sql.startsWith("CREATE TABLE")) {
          schema.push(sql);
          rows = [];
        } else if (sql.startsWith("PRAGMA table_info")) {
          rows = pragmaRows(sql);
        } else if (sql.startsWith("SELECT owner_id, fence FROM nanocodex_durable_owners")) {
          const stored = owners.get(stateId);
          rows = stored ? [stored] : [];
        } else if (sql.startsWith("INSERT INTO nanocodex_durable_owners")) {
          owners.set(stateId, { owner_id: args[1], fence: args[2] });
          rows = [];
        } else if (sql.startsWith("SELECT revision FROM nanocodex_durable_states")) {
          const stored = states.get(stateId);
          rows = stored ? [{ revision: stored.revision }] : [];
        } else if (sql.startsWith("SELECT revision, payload FROM nanocodex_durable_states")) {
          payloadReads += 1;
          const stored = states.get(stateId);
          rows = stored ? [stored] : [];
        } else if (sql.startsWith("INSERT INTO nanocodex_durable_states")) {
          states.set(stateId, { revision, payload });
          rows = [];
        } else if (sql.startsWith("DELETE FROM nanocodex_durable_chunk_heads")) {
          heads.delete(stateId);
          rows = [];
        } else if (sql.startsWith("INSERT INTO nanocodex_durable_chunk_heads")) {
          heads.set(stateId, { revision, chunk_count: payload });
          rows = [];
        } else if (sql.startsWith("SELECT revision, chunk_count FROM nanocodex_durable_chunk_heads")) {
          const stored = heads.get(stateId);
          rows = stored ? [stored] : [];
        } else if (sql.startsWith("DELETE FROM nanocodex_durable_state_chunks")) {
          for (let index = chunks.length - 1; index >= 0; index -= 1) {
            if (chunks[index].stateId === stateId) chunks.splice(index, 1);
          }
          rows = [];
        } else if (sql.startsWith("INSERT INTO nanocodex_durable_state_chunks")) {
          chunks.push({ stateId, revision, chunk_index: args[2], payload: args[3] });
          rows = [];
        } else if (sql.startsWith("SELECT revision, chunk_index, payload FROM nanocodex_durable_state_chunks")) {
          rows = chunks.filter((chunk) => chunk.stateId === stateId);
        } else {
          throw new Error(`unexpected SQL: ${sql}`);
        }
        return { toArray: () => rows };
      },
    },
    transactionSync(callback) {
      transactions += 1;
      return callback();
    },
  };

  const store = createCloudflareDurabilityStore(storage);
  assert.deepEqual(schema.slice(0, sqliteDurabilitySchema.length), sqliteDurabilitySchema);
  assert.equal(schema.length, sqliteDurabilitySchema.length + 2);
  assert.deepEqual(store.load("agent-1"), { revision: "0", payload: null });
  const owner = store.acquire("agent-1", { ownerId: "worker-1" });
  assert.deepEqual(owner, {
    ownerId: "worker-1",
    fence: "1",
    revision: "0",
    payload: null,
  });
  assert.deepEqual(store.replace("agent-1", {
    ownerId: owner.ownerId,
    fence: owner.fence,
    expectedRevision: "0",
    payload: "opaque-rust-state",
  }), { status: "replaced", revision: "1" });
  assert.deepEqual(store.load("agent-1"), {
    revision: "1",
    payload: "opaque-rust-state",
  });

  const largePayload = `${"x".repeat(255_999)}😀${"y".repeat(1_800_000)}`;
  const largeOwner = store.acquire("agent-large", { ownerId: "worker-large" });
  assert.deepEqual(store.replace("agent-large", {
    ownerId: largeOwner.ownerId,
    fence: largeOwner.fence,
    expectedRevision: "0",
    payload: largePayload,
  }), { status: "replaced", revision: "1" });
  assert.equal(states.get("agent-large").payload, "");
  assert.ok(chunks.filter((chunk) => chunk.stateId === "agent-large").length > 1);
  assert.ok(chunks.every((chunk) => chunk.payload.length <= 256_000));
  assert.equal(store.load("agent-large").payload, largePayload);

  // Writes must not materialize the old multi-megabyte checkpoint. Only an
  // ambiguous acknowledgement at the next revision needs an exact comparison.
  const writeOwner = store.acquire("agent-write", { ownerId: "worker-write" });
  const write = (expectedRevision, payload) => store.replace("agent-write", {
    ownerId: writeOwner.ownerId, fence: writeOwner.fence, expectedRevision, payload,
  });
  assert.deepEqual(write("0", largePayload), { status: "replaced", revision: "1" });
  const beforeWrite = payloadReads;
  assert.deepEqual(write("1", largePayload), { status: "replaced", revision: "2" });
  assert.equal(payloadReads, beforeWrite, "normal replacement must not read the old payload");
  assert.deepEqual(write("0", largePayload), { status: "conflict", actualRevision: "2" });
  assert.equal(payloadReads, beforeWrite, "stale revisions must not read the old payload");
  assert.deepEqual(write("1", largePayload), { status: "replaced", revision: "2" });
  assert.equal(payloadReads, beforeWrite + 1, "lost acknowledgement requires exact payload replay");
  assert.deepEqual(write("1", `${largePayload}different`), { status: "conflict", actualRevision: "2" });
  assert.equal(store.load("agent-write").payload, largePayload);

  const replacementOwner = store.acquire("agent-large", { ownerId: "worker-replacement" });
  assert.deepEqual(store.replace("agent-large", {
    ownerId: largeOwner.ownerId,
    fence: largeOwner.fence,
    expectedRevision: "not-a-revision",
    payload: new Uint8Array(),
  }), { status: "fenced" });
  assert.deepEqual(store.replace("agent-large", {
    ownerId: replacementOwner.ownerId,
    fence: replacementOwner.fence,
    expectedRevision: "0",
    payload: new Uint8Array(),
  }), { status: "conflict", actualRevision: "1" });

  assert.deepEqual(store.importState("agent-import", {
    revision: "47",
    payload: largePayload,
  }), { revision: "47", payload: largePayload });
  assert.equal(states.get("agent-import").payload, "");
  assert.equal(store.load("agent-import").payload, largePayload);
  assert.deepEqual(store.acquire("agent-import", { ownerId: "imported-agent" }), {
    ownerId: "imported-agent",
    fence: "2",
    revision: "47",
    payload: largePayload,
  });
  assert.throws(
    () => store.importState("agent-import", { revision: "48", payload: "overwrite" }),
    DurabilityImportConflictError,
  );

  assert.deepEqual(store.importState("agent-range", {
    revision: "4",
    payload: "expected-lineage",
  }), { revision: "4", payload: "expected-lineage" });
  assert.throws(
    () => store.importState("agent-range", { revision: "9", payload: "replacement" }, {
      expectedRevision: "4",
      expectedPayload: "divergent-lineage",
    }),
    DurabilityImportConflictError,
  );
  assert.deepEqual(store.load("agent-range"), {
    revision: "4",
    payload: "expected-lineage",
  });

  const firstChunk = chunks.find((chunk) => chunk.stateId === "agent-large");
  firstChunk.revision = "2";
  assert.throws(() => store.load("agent-large"), /invalid Cloudflare durability chunks/);
  firstChunk.revision = "1";
  firstChunk.chunk_index = 99;
  assert.throws(() => store.load("agent-large"), /invalid Cloudflare durability chunks/);
  firstChunk.chunk_index = 0;
  states.get("agent-large").payload = "unexpected-inline-state";
  assert.throws(() => store.load("agent-large"), /invalid Cloudflare durability chunk head/);
  states.get("agent-large").payload = "";
  chunks.splice(chunks.findLastIndex((chunk) => chunk.stateId === "agent-large"), 1);
  assert.throws(() => store.load("agent-large"), /missing Cloudflare durability chunks/);
  assert.ok(transactions >= 6);
  assert.throws(
    () => createCloudflareDurabilityStore({}),
    /Durable Object storage with SQLite/,
  );
  assert.throws(
    () => createCloudflareDurabilityStore({
      sql: { exec: () => ({ toArray: () => [] }) },
      transactionSync: (callback) => callback(),
    }),
    /incompatible Cloudflare durability schema/,
  );
});

function pragmaRows(sql) {
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
    ["state_id", "TEXT", 1, 1], ["revision", "TEXT", 1, 2],
    ["chunk_index", "INTEGER", 1, 3], ["payload", "TEXT", 1, 0],
  ]);
}

function columns(shapes) {
  return shapes.map(([name, type, notnull, pk], cid) => ({ cid, name, type, notnull, pk }));
}
