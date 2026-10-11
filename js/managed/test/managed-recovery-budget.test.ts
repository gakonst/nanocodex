import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";

// Public managed admission/receipts and the real Workers SQLite recovery pump.
// Only the upstream credential service is replaced; no private recovery helper
// is invoked. Backoff timestamps are advanced to avoid waiting a minute.
it.each(["direct", "reopen-wrapper", "retryable-cause"])("bounds %s host interruptions, retains safe cause diagnostics, and admits later work", async wrapping => {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, ctx) => {
    let calls = 0;
    let mode: "interrupted" | "transient" | "complete" = "interrupted";
    const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
    Object.defineProperty(session, "env", { value: { ...runtimeEnv, NANOCODEX: { fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
      const request = new Request(input, init);
      const path = new URL(request.url).pathname;
      if (mode === "complete") {
        if (request.headers.get("upgrade") === "websocket") {
          const pair = new WebSocketPair();
          pair[1].accept();
          pair[1].addEventListener("message", () => pair[1].send(JSON.stringify({ type: "response.completed", response: {
            id: "fixture-recovered-response", status: "completed", end_turn: true,
            output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "RECOVERED_OK" }] }],
            usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 },
          } })));
          return new Response(null, { status: 101, webSocket: pair[0] });
        }
        return Response.json({ tools: [], machines: [], connections: [] });
      }
      if (!path.startsWith("/subjects/")) return Response.json({ connectors: {}, mcp_connections: [] });
      calls++;
      if (mode === "interrupted") {
        const conflict = Object.assign(new Error("private tool arguments DO_NOT_PUBLISH"), { code: "code_effect_identity_conflict" });
        const cause = wrapping === "retryable-cause"
          ? Object.assign(new Error("upstream retryable secret=DO_NOT_PUBLISH", { cause: conflict }), { code: "retryable" }) : conflict;
        const interrupted = Object.assign(new Error("fixture upstream secret=DO_NOT_PUBLISH", { cause }), { code: "host_interrupted" });
        throw wrapping === "reopen-wrapper"
          ? Object.assign(new Error("rollback failed secret=DO_NOT_PUBLISH", { cause: interrupted }), { code: "reopen_required" }) : interrupted;
      }
      if (mode === "transient") throw Object.assign(new Error("fixture temporary service outage"), { code: "retryable" });
      return Response.json({ tools: [], machines: [], connections: [] });
    } } } });
    await session.fetch(new Request("https://session.internal/state")); // A fresh Session creates its schema on its first request (9d8b63102).
    ctx.storage.sql.exec(`INSERT INTO session_state (singleton, session_id, owner_id, organization_id, team_id,
      authorization_epoch, public_origin, runtime_profile, last_active) VALUES (1, ?, 'fixture-owner', 'fixture-org',
      'fixture-team', 1, 'https://nanocodex.example/', 'managed', ?)`, crypto.randomUUID(), Date.now());
    ctx.storage.sql.exec("INSERT INTO managed_configuration VALUES (1, ?)", JSON.stringify({ tools: [],
      environment: { files: [], skills: [], setup_commands: [], network: { access: "disabled" } } }));
    ctx.storage.sql.exec("UPDATE managed_agent_settings SET model='gpt-6.1-sol', thinking='low'");
    const request = (path: string, body?: unknown) => session.fetch(new Request("https://session.internal" + path,
      body === undefined ? {} : { method: "POST", body: JSON.stringify(body) }));
    const receipt = async (id: string) => (await request("/turns/" + id)).json() as Promise<{ state: string; error?: string; terminal?: { error: string; final_message?: string } }>;
    const row = (id: string) => ctx.storage.sql.exec<{ state: string; attempt_count: number; retry_at: number | null }>(
      "SELECT state,attempt_count,retry_at FROM managed_turns WHERE id=?", id).one();
    const retry = async (id: string) => {
      ctx.storage.sql.exec("UPDATE managed_turns SET retry_at=? WHERE id=?", Date.now() - 1, id);
      await session.alarm();
    };
    const trace: unknown[] = [];
    try {
      expect((await request("/turns", { id: "interrupted", input: "synthetic recovery fixture" })).status).toBe(202);
      for (let attempt = 1; attempt <= 3; attempt++) {
        await expect.poll(() => row("interrupted").attempt_count).toBe(attempt);
        const observed = await receipt("interrupted");
        expect(observed.state).toBe("accepted");
        trace.push({ attempt, observed });
        await retry("interrupted");
      }
      await expect.poll(() => row("interrupted").state).toBe("failed");
      const stopped = await receipt("interrupted");
      expect(stopped.terminal?.error).toContain("MANAGED_RECOVERY_EXHAUSTED");
      expect(JSON.stringify(trace)).toContain("host_interrupted");
      expect(JSON.stringify(trace)).toContain("code_effect_identity_conflict");
      expect(JSON.stringify(trace)).not.toContain("DO_NOT_PUBLISH");
      const stoppedCalls = calls;
      await session.alarm();
      expect(calls).toBe(stoppedCalls);
      trace.push({ stopped, stoppedCalls });

      // Explicitly transient preflight outages still retry beyond the abrupt
      // budget; a recovered service can then settle the same accepted request.
      mode = "transient";
      expect((await request("/turns", { id: "transient", input: "synthetic transient fixture" })).status).toBe(202);
      for (let attempt = 1; attempt <= 5; attempt++) {
        await expect.poll(() => row("transient").attempt_count).toBe(attempt);
        expect((await receipt("transient")).state).toBe("accepted");
        if (attempt < 5) await retry("transient");
      }
      mode = "complete";
      await retry("transient");
      await expect.poll(() => row("transient").state).toBe("completed");
      const recovered = await receipt("transient");
      expect(recovered.terminal?.final_message).toBe("RECOVERED_OK");
      trace.push({ recovered });
      console.log("MANAGED_CAUGHT_INTERRUPTION_BUDGET_JOURNEY", JSON.stringify(trace));
    } finally {
      ctx.storage.sql.exec("UPDATE managed_turns SET state='cancelled',retry_at=NULL WHERE state IN ('accepted','cancelling')");
      await ctx.storage.deleteAlarm();
    }
  });
}, 30_000);
