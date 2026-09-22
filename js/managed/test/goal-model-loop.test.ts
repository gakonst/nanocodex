import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { Goals } from "../src/goals";

it("continues a real managed model turn and stops at the goal token budget", async () => {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
    let requests = 0;
    const prompts: string[] = [];
    class ModelSocket extends EventTarget {
      readyState = 1;
      accept() {}
      close() { this.readyState = 3; }
      send(data: string) {
        prompts.push(data);
        const sequence = ++requests;
        if (sequence > 4) throw new Error("goal continued beyond its budget");
        queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
          type: "response.completed", response: {
            id: `goal-response-${sequence}`, status: "completed", end_turn: true,
            output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "Made progress; work remains." }] }],
            usage: { input_tokens: 100, input_tokens_details: { cached_tokens: 80 }, output_tokens: 10, total_tokens: 110 },
          },
        }) })));
      }
    }
    const runtimeEnv = (session as unknown as { env: Record<string, unknown> }).env;
    Object.defineProperty(session, "env", { value: { ...runtimeEnv,
      NANOCODEX_MEMORY: { getByName: () => ({ fetch: async () => Response.json({}) }) },
      NANOCODEX: { async fetch(input: RequestInfo | URL, init?: RequestInit) {
        const request = new Request(input, init);
        if (request.headers.get("upgrade") === "websocket") return { status: 101, headers: new Headers(), webSocket: new ModelSocket() };
        return Response.json({ tools: [], machines: [], connections: [] });
      } },
    } });
    const now = Date.now();
    const threadId = crypto.randomUUID();
    state.storage.sql.exec(`INSERT INTO session_state (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active)
      VALUES (1,?,'fixture-owner','fixture-org','fixture-team',1,'https://nanocodex.example/','managed',?)`, threadId, now);
    state.storage.sql.exec("INSERT INTO managed_configuration VALUES (1, ?)", JSON.stringify({ tools: [], environment: { files: [], skills: [], setup_commands: [], network: { access: "disabled" } } }));
    state.storage.sql.exec("UPDATE managed_agent_settings SET model='gpt-6-sol', thinking='low'");
    const goals = new Goals(state.storage, () => threadId);
    goals.create({ objective: "Complete every acceptance criterion", token_budget: 65 });
    state.storage.sql.exec(`INSERT INTO managed_turns (id,request_hash,input_json,authorization_json,state,accepted_cursor,dispatch_input_chunks,may_have_inner_operation,attempt_count,created_at,accepted_at,updated_at)
      VALUES ('first','hash','"fixture"','{"capabilities":[]}','accepted',0,1,0,0,?,?,?)`, now, now, now);
    state.storage.sql.exec("INSERT INTO managed_turn_dispatch_chunks VALUES ('first',0,'\"fixture\"')");
    try {
      await session.alarm();
      await expect.poll(() => goals.get()?.status, { timeout: 15_000 }).toBe("budgetLimited");
      await expect.poll(() => state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM managed_turns WHERE state IN ('accepted','cancelling')").one().n).toBe(0);
      expect(requests).toBe(3);
      expect(goals.get()?.tokensUsed).toBe(90);
      expect(prompts.slice(1).join("\n")).toContain("Completion audit");
      expect(prompts.slice(1).join("\n")).toContain("Complete every acceptance criterion");
      expect(state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM managed_goal_turns WHERE pending=1").one().n).toBe(0);
    } finally {
      goals.clear();
      await state.storage.deleteAlarm();
    }
  });
}, 30_000);
