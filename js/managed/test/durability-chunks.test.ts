import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { createCloudflareDurabilityStore } from "nanocodex/durability/cloudflare";
import { durabilityRevision } from "nanocodex/durability";
import type { DurableAgentSession } from "../src/index";

describe("Cloudflare execution records", () => {
  it("publishes records and their execution head in one transaction", async () => {
    const sessions = (env as unknown as {
      NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
    }).NANOCODEX_SESSIONS;
    await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_session, state) => {
      let failAtRecord: string | undefined;
      let recordReads = 0;
      const storage = {
        sql: { exec<Row extends Record<string, string | number | null>>(sql: string, ...args: Array<string | number | null>) {
          if (sql.startsWith("INSERT INTO nanocodex_durable_records") && args[1] === failAtRecord) {
            throw new Error("fixture interrupted record write");
          }
          if (sql.startsWith("SELECT value FROM nanocodex_durable_records") || sql.startsWith("SELECT key, value FROM nanocodex_durable_records")) recordReads++;
          return state.storage.sql.exec<Row>(sql, ...args);
        } },
        transactionSync<T>(callback: () => T) { return state.storage.transactionSync(callback); },
      };
      const store = createCloudflareDurabilityStore(storage);
      const stateId = "fixture-record-state";
      const owner = await store.acquire(stateId, { ownerId: "fixture-owner" });
      const original = { key: "original", value: "retained exact result 🧪" };
      const write = (expectedRevision: string, payload: string, records: readonly { key: string; value: string }[]) => store.replace(stateId, {
        ownerId: owner.ownerId, fence: owner.fence, expectedRevision: durabilityRevision(expectedRevision), payload, records,
      });
      expect(await write("0", "head-1", [original])).toEqual({ status: "replaced", revision: "1" });
      // Exercise the incident's 51 * 256,000-byte interruption boundary. Bodies
      // are independent records, and the execution head is never a partial body.
      const records = Array.from({ length: 64 }, (_, index) => ({ key: `record-${index}`, value: "r".repeat(256_000) }));
      failAtRecord = "record-51";
      expect(() => write("1", "head-2", records)).toThrow("fixture interrupted record write");
      expect(await store.load(stateId)).toEqual({ revision: "1", payload: "head-1" });
      expect(await store.readRecord(stateId, original.key)).toBe(original.value);
      expect(await store.readRecord(stateId, "record-0")).toBeNull();
      expect(await store.readRecord(stateId, "record-50")).toBeNull();

      failAtRecord = undefined;
      expect(await write("1", "head-2", records)).toEqual({ status: "replaced", revision: "2" });
      recordReads = 0;
      const next = await store.acquire(stateId, { ownerId: "replacement-owner" });
      expect(next.payload).toBe("head-2");
      expect(recordReads).toBe(0); // Cold acquisition loads control state only.
      expect(await store.readRecords!(stateId, ["record-63", "missing", "original", "record-63"])).toEqual([records[63]!.value, null, original.value, records[63]!.value]);
      expect(recordReads).toBe(1); // One SQLite query preserves requested order and missing values.
      expect(await write("2", "stale", [{ key: "stale", value: "must not commit" }])).toEqual({ status: "fenced" });
      expect(await store.readRecord(stateId, "stale")).toBeNull();
    });
  });
});
