import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { createCloudflareDurabilityStore } from "nanocodex/durability/cloudflare";
import { Agent as CloudflareAgent } from "nanocodex/cloudflare";
import { durabilityRevision } from "nanocodex/durability";
import type { DurableAgentSession } from "../src/index";

describe("Cloudflare execution records", () => {
  it("hides staged imported receipts until identity and head commit atomically", async () => {
    const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
    await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_session, state) => {
      let fail = true;
      const storage = {
        sql: { exec<Row extends Record<string, string | number | null>>(sql: string, ...args: Array<string | number | null>) {
          if (fail && sql.startsWith("INSERT INTO nanocodex_cloudflare_durability")) throw new Error("fixture identity interruption");
          return state.storage.sql.exec<Row>(sql, ...args);
        } },
        transactionSync<T>(callback: () => T) { return state.storage.transactionSync(callback); },
      };
      const store = createCloudflareDurabilityStore(storage);
      await store.stageImportRecords("import-fixture", "manifest-a", [{ key: "staged", value: "exact retained content" }]);
      expect(await store.readRecord("import-fixture", "staged")).toBeNull();
      const archive = { format: "nanocodex-durability-state-v2" as const, stateId: "import-fixture", revision: durabilityRevision("1"), payload: JSON.stringify({ nanocodex_durable_state: { format: 4, operations: {}, latest_checkpoint: null } }), records: [] };
      const owner = { ctx: { storage, acceptWebSocket() {}, getWebSockets() { return []; } } };
      await expect(CloudflareAgent.importDurabilityState(owner, archive, { stagedImportId: "manifest-a" })).rejects.toThrow("fixture identity interruption");
      expect(await store.load(archive.stateId)).toEqual({ revision: "0", payload: null });
      expect(await store.readRecord(archive.stateId, "staged")).toBeNull();
      // A failed import cannot impersonate a prior completed call even if an
      // unrelated session advances to the archived revision later.
      fail = false;
      await CloudflareAgent.importDurabilityState(owner, archive, { stagedImportId: "manifest-a" });
      expect(await store.load(archive.stateId)).toEqual({ revision: "1", payload: archive.payload });
      expect(await store.readRecord(archive.stateId, "staged")).toBe("exact retained content");
      expect(state.storage.sql.exec("SELECT * FROM nanocodex_durable_staged_records").toArray()).toEqual([]);
    });
  });
  it("an interrupted managed import cannot surface an orphan receipt after unrelated head advancement", async () => {
    const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
    await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_session, state) => {
      const store = createCloudflareDurabilityStore(state.storage);
      const id = "interrupted-late-import";
      const key = `late-receipt:${"a".repeat(64)}`;
      const receipt = JSON.stringify({
        format: 1, archived_revision: 1, operation_id: "late-output:old-call-id",
        input: { inline: "old-input" }, status: { Cancelled: { checkpoint: null } },
      });
      await store.stageImportRecords(id, "manifest-interrupted", [{ key, value: receipt }]);
      expect(await store.readRecord(id, key)).toBeNull();
      const owner = await store.acquire(id, { ownerId: "unrelated-live-agent" });
      expect(await store.replace(id, {
        ...owner, expectedRevision: durabilityRevision("0"), payload: "unrelated-live-head", records: [],
      })).toEqual({ status: "replaced", revision: "1" });
      expect(await store.readRecord(id, key)).toBeNull();
      expect(store.scanRecords(id, "", 16)).toEqual([]);
      expect(state.storage.sql.exec("SELECT import_id FROM nanocodex_durable_staged_records WHERE state_id = ?", id).toArray()).toEqual([
        { import_id: "manifest-interrupted" },
      ]);
    });
  });
  it("staged receipt collisions roll back import without publishing a head", async () => {
    const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
    await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_session, state) => {
      const store = createCloudflareDurabilityStore(state.storage);
      const id = "staged-conflict";
      await store.importRecords(id, [{ key: "late-receipt:original", value: "original" }]);
      await store.stageImportRecords(id, "manifest-a", [{ key: "late-receipt:original", value: "altered" }]);
      const head = { revision: durabilityRevision("1"), payload: "new-head" };
      expect(() => store.importState(id, head, { stagedImportId: "manifest-a" })).toThrow(/immutable record conflict/);
      expect(await store.load(id)).toEqual({ revision: "0", payload: null });
      expect(await store.readRecord(id, "late-receipt:original")).toBe("original");
      expect(state.storage.sql.exec("SELECT value FROM nanocodex_durable_staged_records WHERE state_id = ?", id).toArray()).toEqual([
        { value: "altered" },
      ]);
    });
  });
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
