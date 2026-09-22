import { env, runInDurableObject } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import type { DurableAgentSession } from "../src/index";

it("preparation requires authority, acknowledges before discovery, and coalesces callers", async () => {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
    const owner = crypto.randomUUID(), organization = crypto.randomUUID(), team = crypto.randomUUID();
    state.storage.sql.exec(`INSERT INTO session_state
      (singleton, session_id, owner_id, organization_id, team_id, authorization_epoch, public_origin, runtime_profile, last_active)
      VALUES (1, ?, ?, ?, ?, 1, 'https://nanocodex.example/', 'managed', ?)`, crypto.randomUUID(), owner, organization, team, Date.now());
    const headers = { "x-nanocodex-owner-id": owner, "x-nanocodex-session-organization-id": organization,
      "x-nanocodex-session-team-id": team, "x-nanocodex-authorization-epoch": "1",
      "x-nanocodex-capabilities": JSON.stringify(["agents:write", "tools:use"]) };
    let release!: () => void;
    const blocked = new Promise<void>(resolve => { release = resolve; });
    const snapshot = vi.fn(async () => { await blocked; return new Response(null, { status: 503 }); });
    const broker = vi.fn(async () => Response.json({ connectors: {}, mcp_connections: [] }));
    const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
    Object.defineProperty(session, "env", { value: { ...runtimeEnv, NANOCODEX: { fetch: broker },
      NANOCODEX_ACCOUNT_TOOLS: { getByName: () => ({ fetch: snapshot }) } } });
    const request = (h?: Record<string, string>) => session.fetch(new Request("https://session.internal/prepare", { method: "POST", headers: h }));
    try {
      expect((await request()).status).toBe(403);
      expect((await request({ ...headers, "x-nanocodex-capabilities": '["agents:read"]' })).status).toBe(403);
      expect((await request({ ...headers, "x-nanocodex-authorization-epoch": "2" })).status).toBe(404);
      expect(snapshot).not.toHaveBeenCalled();
      expect((await session.fetch(new Request("https://session.internal/prepare", {
        method: "POST", headers, body: "{}",
      }))).status).toBe(400);
      const first = await request(headers);
      expect(first.status).toBe(202);
      expect(await first.json()).toEqual({ state: "preparing" });
      expect((await request(headers)).status).toBe(202);
      expect((await session.fetch(new Request("https://session.internal/prepare", {
        method: "POST", headers, body: new ReadableStream({ start(controller) { controller.close(); } }),
      }))).status).toBe(202);
      await vi.waitFor(() => expect(snapshot).toHaveBeenCalledTimes(1));
      expect(broker.mock.calls.length).toBeGreaterThan(0);
      expect(state.storage.sql.exec<{ count: number }>("SELECT COUNT(*) AS count FROM managed_turns").one().count).toBe(0);
    } finally {
      release();
      // Let the owned background failure settle before the Worker test tears down.
      await new Promise(resolve => setTimeout(resolve, 30));
      await state.storage.deleteAlarm();
    }
  });
});
