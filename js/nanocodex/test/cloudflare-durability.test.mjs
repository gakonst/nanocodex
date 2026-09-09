import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import test from "node:test";
import { createCloudflareDurabilityStore } from "nanocodex/durability/cloudflare";

test("Cloudflare adapter reads only requested records and atomically publishes the head", () => {
  const db = new DatabaseSync(":memory:");
  let interrupt = false;
  const storage = {
    sql: { exec(sql, ...args) {
      if (interrupt && sql.startsWith("INSERT INTO nanocodex_durable_states")) throw new Error("interrupted before head");
      return db.prepare(sql).all(...args);
    } },
    transactionSync(callback) {
      db.exec("BEGIN");
      try { const result = callback(); db.exec("COMMIT"); return result; }
      catch (error) { db.exec("ROLLBACK"); throw error; }
    },
  };
  try {
    const store = createCloudflareDurabilityStore(storage);
    const owner = store.acquire("agent", { ownerId: "owner" });
    const write = (expectedRevision, payload, records) => store.replace("agent", { ...owner, expectedRevision, payload, records });
    assert.deepEqual(write("0", "head", [{ key: "message", value: "exact text 🧪" }]), { status: "replaced", revision: "1" });
    interrupt = true;
    assert.throws(() => write("1", "next", [{ key: "uncommitted", value: "not visible" }]), /interrupted before head/);
    assert.deepEqual(store.load("agent"), { revision: "1", payload: "head" });
    assert.equal(store.readRecord("agent", "message"), "exact text 🧪");
    assert.equal(store.readRecord("agent", "uncommitted"), null);
    assert.equal(store.readRecord("another-agent", "message"), null);
    interrupt = false;
    const fresh = store.acquire("agent", { ownerId: "fresh" });
    assert.equal(fresh.payload, "head");
    assert.deepEqual(write("1", "stale", []), { status: "fenced" });
  } finally { db.close(); }
});
