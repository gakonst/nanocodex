import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { Agent as CloudflareAgent } from "nanocodex/cloudflare";
import type { DurableAgentSession } from "../src/index";

describe("durable identified steering receipts", () => {
  it("reads and replays completed acceptance before runtime recovery, rejects conflicts and tombstones", async () => {
    const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
    await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
      const now = Date.now();
      const input = "already accepted";
      state.storage.sql.exec(`INSERT INTO session_state (singleton, session_id, owner_id, organization_id, team_id, authorization_epoch, public_origin, runtime_profile, last_active)
        VALUES (1, 'fixture-session', 'fixture-owner', 'fixture-org', 'fixture-team', 1, 'https://nanocodex.example/', 'managed', ?)`, now);
      state.storage.sql.exec(`INSERT INTO managed_turns (id, request_hash, input_json, authorization_json, state, accepted_cursor, may_have_inner_operation, attempt_count, created_at, accepted_at, updated_at)
        VALUES ('finished', 'hash', '"fixture"', '{"capabilities":[]}', 'completed', 0, 1, 1, ?, ?, ?)`, now, now, now);
      state.storage.sql.exec("CREATE TABLE nanocodex_cloudflare_durability (singleton INTEGER PRIMARY KEY, state_id TEXT)");
      state.storage.sql.exec("INSERT INTO nanocodex_cloudflare_durability VALUES (1, 'fixture-session')");
      state.storage.sql.exec("CREATE TABLE nanocodex_durable_states (state_id TEXT PRIMARY KEY, revision TEXT, payload TEXT)");
      const inputKey = await CloudflareAgent.steerInputKey(input);
      const payload = JSON.stringify({ nanocodex_durable_state: { format: 4, operations: { finished: { steer_receipts: {
        accepted: { input_key: inputKey, index: 1, withdrawn: false },
        withdrawn: { input_key: inputKey, index: 2, withdrawn: true },
      } } } } });
      state.storage.sql.exec("INSERT INTO nanocodex_durable_states VALUES ('fixture-session', '9', ?)", payload);
      const receipt = (id: string) => session.fetch(new Request(`https://session.internal/turns/finished/steer-receipt?message_id=${id}`));
      expect(await (await receipt("accepted")).json()).toEqual({ protocol: 1, turn_id: "finished", message_id: "accepted", state: "accepted", input_key: inputKey, terminal: true });
      expect(await (await receipt("legacy-missing")).json()).toMatchObject({ state: "unknown", terminal: true });
      expect(await (await receipt("withdrawn")).json()).toMatchObject({ state: "withdrawn" });
      const post = (message_id: string, prompt = input) => session.fetch(new Request("https://session.internal/turns/finished/steer", { method: "POST", body: JSON.stringify({ input: prompt, message_id }) }));
      for (let replay = 0; replay < 3; replay++) expect((await post("accepted")).status).toBe(202);
      const conflict = await post("accepted", "different input");
      expect(conflict.status).toBe(409);
      expect(await conflict.json()).toMatchObject({ error: "message_id_conflict" });
      const withdrawn = await post("withdrawn");
      expect(withdrawn.status).toBe(409);
      expect(await withdrawn.json()).toMatchObject({ error: "steer_withdrawn" });
      expect(state.storage.sql.exec<{ payload: string }>("SELECT payload FROM nanocodex_durable_states").one().payload).toBe(payload);
      expect((await (await session.fetch(new Request("https://session.internal/state"))).json<{ agent_loaded: boolean }>()).agent_loaded).toBe(false);
      await state.storage.deleteAlarm();
    });
  });
});
