import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { ManagedRealtimeArchive, type ManagedRealtimeReceipt } from "../src/managed-realtime-archive";

for (const failWrite of [false, true]) {
  it(`archives a voice context larger than a SQLite row with write failure=${failWrite}`, async () => {
    const bindings = env as unknown as {
      NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
      NANOCODEX_HISTORY: R2Bucket;
    };
    await runInDurableObject(bindings.NANOCODEX_SESSIONS.getByName(crypto.randomUUID()), async (_session, state) => {
      const receipt: ManagedRealtimeReceipt = {
        voice_session_id: crypto.randomUUID(), operation_id: crypto.randomUUID(),
        kind: "start", request_hash: "a".repeat(64), state: "completed",
        response_json: JSON.stringify({ context: {
          workspace: "/brain", history: [{ role: "user", content: [
            { type: "input_text", text: "😀".repeat(800_000) },
          ] }],
        } }),
        created_at: 1, updated_at: 2,
      };
      expect(new TextEncoder().encode(receipt.response_json).length).toBeGreaterThan(3 * 1024 * 1024);
      state.storage.sql.exec(
        `INSERT INTO managed_realtime_operations (
          voice_session_id, operation_id, kind, request_hash, state, created_at, updated_at
        ) VALUES (?, ?, ?, ?, 'pending', ?, ?)`,
        receipt.voice_session_id, receipt.operation_id, receipt.kind,
        receipt.request_hash, receipt.created_at, receipt.created_at,
      );
      const bucket = failWrite ? { put: async () => { throw new Error("fixture R2 unavailable"); } } as unknown as R2Bucket : bindings.NANOCODEX_HISTORY;
      const archive = new ManagedRealtimeArchive(state.storage, bucket, state.id.toString());
      const rows = () => state.storage.sql.exec<{ state: string; response_json: string | null }>(
        "SELECT state, response_json FROM managed_realtime_operations",
      ).toArray();
      if (failWrite) {
        await expect(archive.complete(receipt)).rejects.toThrow("fixture R2 unavailable");
        expect(rows()).toEqual([{ state: "pending", response_json: null }]);
        expect(archive.capacity().archived_receipts).toBe(0);
      } else {
        await archive.complete(receipt);
        expect(rows()).toEqual([]);
        expect(archive.capacity()).toMatchObject({ archived_receipts: 1, objects: 1 });
        const restored = new ManagedRealtimeArchive(state.storage, bindings.NANOCODEX_HISTORY, state.id.toString());
        expect(await restored.find(receipt.voice_session_id, receipt.operation_id)).toEqual(receipt);
      }
    });
  });
}
