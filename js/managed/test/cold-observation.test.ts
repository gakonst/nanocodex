import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";

describe("cold managed observations", () => {
  it("reads history and opens observers without constructing an execution runtime", async () => {
    const sessions = (env as unknown as {
      NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
    }).NANOCODEX_SESSIONS;
    await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
      const owner = crypto.randomUUID(), organization = crypto.randomUUID(), team = crypto.randomUUID();
      const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
      let constructions = 0;
      Object.defineProperty(session, "env", { value: {
        ...runtimeEnv,
        NANOCODEX_ACCOUNT_TOOLS: { getByName: () => {
          constructions++;
          throw new Error("observations must not construct the agent");
        } },
      } });
      state.storage.sql.exec(`INSERT INTO session_state
        (singleton, session_id, owner_id, organization_id, team_id, authorization_epoch,
         public_origin, runtime_profile, last_active)
        VALUES (1, ?, ?, ?, ?, 1, 'https://nanocodex.example/', 'managed', ?)`,
      crypto.randomUUID(), owner, organization, team, Date.now() - 60_000);
      const headers = {
        "x-nanocodex-owner-id": owner,
        "x-nanocodex-session-organization-id": organization,
        "x-nanocodex-session-team-id": team,
        "x-nanocodex-authorization-epoch": "1",
        "x-nanocodex-capabilities": '["agents:write","tools:use"]',
      };
      const history = await session.fetch(new Request("https://session.internal/events/history", { headers }));
      expect(history.status).toBe(200);
      expect(await history.json()).toMatchObject({ data: [] });
      const stream = await session.fetch(new Request("https://session.internal/events?cursor=latest", { headers }));
      expect(stream.status).toBe(200);
      await stream.body?.cancel();
      const socket = await session.fetch(new Request("https://session.internal/socket", { headers: { ...headers, upgrade: "websocket" } }));
      expect(socket.status).toBe(101);
      socket.webSocket!.accept();
      socket.webSocket!.close(1000);
      await session.alarm();
      const status = await session.fetch(new Request("https://session.internal/state", { headers }));
      expect(await status.json()).toMatchObject({ agent_loaded: false, active_turns: [] });
      expect(constructions).toBe(0);
    });
  });
});
