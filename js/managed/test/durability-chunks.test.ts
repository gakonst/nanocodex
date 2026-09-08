import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { createCloudflareDurabilityStore } from "nanocodex/durability/cloudflare";
import { durabilityRevision } from "nanocodex/durability";

import type { DurableAgentSession } from "../src/index";

describe("Cloudflare durability chunk atomicity", () => {
  it("rolls back a partial replacement and retains its prior complete revision", async () => {
    const sessions = (env as unknown as {
      NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
    }).NANOCODEX_SESSIONS;
    await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (_session, state) => {
      let failAtChunk: number | undefined;
      const storage = {
        sql: { exec<Row extends Record<string, string | number | null>>(sql: string, ...args: Array<string | number | null>) {
          if (sql.startsWith("INSERT INTO nanocodex_durable_state_chunks") && args[2] === failAtChunk) {
            throw new Error("fixture interrupted chunk write");
          }
          return state.storage.sql.exec<Row>(sql, ...args);
        } },
        transactionSync<T>(callback: () => T) { return state.storage.transactionSync(callback); },
      };
      const store = createCloudflareDurabilityStore(storage);
      const stateId = "fixture-chunked-state";
      const owner = await store.acquire(stateId, { ownerId: "fixture-owner" });
      // The retained device failure has 13,056,000 bytes, exactly 51 full
      // chunks. Exercise larger states and interrupt at that same boundary.
      const original = "o".repeat(13_600_000);
      const replacement = "r".repeat(14_600_000);
      const write = (expectedRevision: string, payload: string) => store.replace(stateId, {
        ownerId: owner.ownerId, fence: owner.fence, expectedRevision: durabilityRevision(expectedRevision), payload,
      });
      expect(await write("0", original)).toEqual({ status: "replaced", revision: "1" });
      expect(await store.load(stateId)).toEqual({ revision: "1", payload: original });

      failAtChunk = 51;
      expect(() => write("1", replacement)).toThrow("fixture interrupted chunk write");
      const reopened = createCloudflareDurabilityStore(state.storage);
      expect(await reopened.load(stateId)).toEqual({ revision: "1", payload: original });
      expect(state.storage.sql.exec<{ revision: string }>(
        "SELECT DISTINCT revision FROM nanocodex_durable_state_chunks WHERE state_id = ?", stateId,
      ).toArray()).toEqual([{ revision: "1" }]);

      failAtChunk = undefined;
      expect(await write("1", replacement)).toEqual({ status: "replaced", revision: "2" });
      expect(await reopened.load(stateId)).toEqual({ revision: "2", payload: replacement });
      const nextOwner = await reopened.acquire(stateId, { ownerId: "fixture-replacement-owner" });
      expect(nextOwner.fence).not.toBe(owner.fence);
      expect(await write("2", original)).toEqual({ status: "fenced" });
      expect(await reopened.load(stateId)).toEqual({ revision: "2", payload: replacement });
    });
  });
});
