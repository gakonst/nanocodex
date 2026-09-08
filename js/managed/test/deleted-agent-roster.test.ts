import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { attachAgent, listAgents } from "../src/account-auth";
import type worker from "../src/index";

const runtime = env as Parameters<typeof worker.fetch>[1];

describe("deleted agent account discovery", () => {
  it.each([false, true])("removes a tombstoned agent before external cleanup, retaining durable retry ownership (account retry=%s)", async (accountInitiallyUnavailable) => {
    const owner = crypto.randomUUID(), id = crypto.randomUUID();
    await attachAgent(runtime, owner, id);
    const stub = runtime.NANOCODEX_SESSIONS.getByName(id);
    await runInDurableObject(stub, async (session, state) => {
      state.storage.sql.exec(`INSERT INTO session_state (
        singleton, session_id, owner_id, organization_id, team_id,
        authorization_epoch, public_origin, runtime_profile, last_active
      ) VALUES (1, ?, ?, ?, ?, 1, 'https://nanocodex.example', 'managed', ?)`,
      id, owner, crypto.randomUUID(), crypto.randomUUID(), Date.now());
      const ownership = { owner_id: owner, session_id: id, subject: state.id.toString(),
        cleanup_at: Date.now(), state: "active", strategy: "session_v1" };
      await state.storage.put("nanocodex:credential-binding", ownership);
      // A failed external cleanup must not leave an unreadable conversation in
      // the account's conversation/schedule roster for every subsequent launch.
      let accountUnavailable = accountInitiallyUnavailable;
      Object.defineProperty(session, "env", { value: { ...runtime,
        NANOCODEX_USERS: { getByName: (userId: string) => ({ fetch: async (...args: Parameters<Fetcher["fetch"]>) =>
          accountUnavailable ? new Response(null, { status: 503 }) : runtime.NANOCODEX_USERS.getByName(userId).fetch(...args),
        }) },
        NANOCODEX_MEMORY: { getByName: () => ({ fetch: async () => new Response(null, { status: 503 }) }) },
      } });
      const response = await session.fetch(new Request("https://session.internal/session", { method: "DELETE" }));
      expect(response.status).toBe(503);
      expect(await response.json()).toEqual({ error: "session_cleanup_pending" });
      expect((await session.fetch(new Request("https://session.internal/triggers"))).status).toBe(404);
      expect(await listAgents(runtime, owner)).toHaveLength(accountUnavailable ? 1 : 0);
      expect(await state.storage.get("nanocodex:session-deleting")).toBe(true);
      expect(await state.storage.get("nanocodex:credential-binding")).toEqual(ownership);
      expect(await state.storage.getAlarm()).not.toBeNull();
      expect(state.storage.sql.exec("SELECT session_id FROM session_state").toArray()).toEqual([{ session_id: id }]);
      // The alarm resumes the same cleanup ownership even though discovery no
      // longer advertises the permanently deleted agent.
      accountUnavailable = false;
      await session.alarm();
      expect(await listAgents(runtime, owner)).toEqual([]);
      expect(await state.storage.get("nanocodex:session-deleting")).toBe(true);
      expect(await state.storage.getAlarm()).not.toBeNull();
      await state.storage.deleteAlarm();
    });
  });
});
