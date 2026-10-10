import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";

it("stopping voice retains its transcript without admitting a backend turn, and replays exactly once", async () => {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
    const owner = crypto.randomUUID(), organization = crypto.randomUUID(), team = crypto.randomUUID();
    const id = crypto.randomUUID(), voice = crypto.randomUUID(), operation = crypto.randomUUID();
    const capabilities = ["agents:write", "tools:use"];
    await session.fetch(new Request("https://session.internal/state")); // A fresh Session creates its schema on its first request (9d8b63102).
    state.storage.sql.exec(`INSERT INTO session_state(singleton,session_id,owner_id,organization_id,team_id,
      authorization_epoch,public_origin,runtime_profile,accepted_turns,last_active)
      VALUES(1,?,?,?,?,1,'https://test.example','managed',0,?)`, id, owner, organization, team, Date.now());
    state.storage.sql.exec(`INSERT INTO managed_realtime_session(singleton,voice_session_id,authorization_json,updated_at)
      VALUES(1,?,?,?)`, voice, JSON.stringify({ capabilities }), Date.now());
    state.storage.sql.exec("INSERT INTO managed_configuration VALUES(1,?)", JSON.stringify({ tools: [] }));
    const runtime = session as unknown as { env: Record<string, unknown> };
    const originalEnv = runtime.env;
    Object.defineProperty(session, "env", { configurable: true, value: { ...originalEnv,
      NANOCODEX: { fetch: async (input: RequestInfo | URL) => {
        const url = new URL(input instanceof Request ? input.url : String(input));
        if (url.hostname === "broker.internal" && url.pathname.startsWith("/subjects/")) return new Response(null, { status: 204 });
        throw new Error(`Unexpected provider request during voice stop: ${url.pathname}`);
      } },
    } });
    const transcript = [{ role: "user", text: "Hello synthetic voice." }, { role: "assistant", text: "Hello." }];
    const stop = (entries: unknown = transcript, operationId = operation) => session.fetch(new Request("https://session.internal/realtime/stop", {
      method: "POST", headers: { "content-type": "application/json", "x-nanocodex-owner-id": owner,
        "x-nanocodex-session-organization-id": organization, "x-nanocodex-session-team-id": team,
        "x-nanocodex-authorization-epoch": "1", "x-nanocodex-capabilities": JSON.stringify(capabilities) },
      body: JSON.stringify({ voice_session_id: voice, operation_id: operationId, transcript: entries }),
    }));
    const first = await stop();
    const result = await first.json<Record<string, unknown>>();
    expect(first.status, JSON.stringify(result)).toBe(200);
    expect(result.stopped).toBe(true);
    expect(JSON.stringify(result.context)).toContain("Hello synthetic voice.");
    expect(state.storage.sql.exec<{ accepted_turns: number }>("SELECT accepted_turns FROM session_state").one().accepted_turns).toBe(0);
    expect(state.storage.sql.exec("SELECT * FROM managed_realtime_session").toArray()).toHaveLength(0);
    const replay = await stop();
    expect(replay.status).toBe(200);
    expect(await replay.json()).toEqual(result);
    const conflict = await stop([{ role: "user", text: "different content" }]);
    expect(conflict.status).toBe(409);
    const replacement = crypto.randomUUID();
    state.storage.sql.exec(`INSERT INTO managed_realtime_session(singleton,voice_session_id,authorization_json,updated_at)
      VALUES(1,?,?,?)`, replacement, JSON.stringify({ capabilities }), Date.now());
    const stale = await stop([{ role: "user", text: "stale call must not append this" }], crypto.randomUUID());
    expect(stale.status).toBe(200);
    expect(await stale.json()).toMatchObject({ stopped: false, stale: true, context: [] });
    expect(state.storage.sql.exec<{ voice_session_id: string }>("SELECT voice_session_id FROM managed_realtime_session").one().voice_session_id).toBe(replacement);
    Object.defineProperty(session, "env", { configurable: true, value: originalEnv });
  });
});
