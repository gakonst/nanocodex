import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { projectFollowupTurnId } from "../src/project-threads";

const sessions = () => (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
const authorization = JSON.stringify({ capabilities: ["agents:read", "agents:write", "tools:use"] });

it("sends and reads an unrelated conversation through the actual managed tool handlers", async () => {
  const senderId = crypto.randomUUID(), targetId = crypto.randomUUID();
  const sender = sessions().getByName(senderId), target = sessions().getByName(targetId);
  const turnId = projectFollowupTurnId(senderId, "reference");
  const taskInput = "Review the reference supplied by the other conversation.";
  const seed = (storage: DurableObjectStorage, id: string) => storage.sql.exec(`INSERT INTO session_state
    (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active)
    VALUES (1,?,'11111111-1111-4111-8111-111111111111','22222222-2222-4222-8222-222222222222',
    '33333333-3333-4333-8333-333333333333',1,'https://nanocodex.example','managed',?)`, id, Date.now());
  await runInDurableObject(target, async (session, state) => {
    seed(state.storage, targetId);
    const runtime = (session as unknown as { env: Record<string, unknown> }).env;
    Object.defineProperty(session, "env", { value: { ...runtime, NANOCODEX_ACCOUNT_TOOLS: { getByName: () => {
      throw Object.assign(new Error("retain target at durable retry boundary"), { code: "retryable" });
    } } } });
  });
  try {
    await runInDurableObject(sender, async (session, state) => {
      seed(state.storage, senderId);
      // An allowlist disables discovery without selecting the restricted environment,
      // which intentionally excludes project tools.
      state.storage.sql.exec("INSERT INTO managed_configuration VALUES (1, ?)",
        JSON.stringify({ tools: ["send_project_thread", "read_project_thread"] }));
      state.storage.sql.exec("UPDATE managed_agent_settings SET model='gpt-5.6-sol', thinking='low'");
      const outputs = new Map<string, string>();
      let requests = 0;
      class ModelSocket extends EventTarget {
        readyState = 1;
        accept() {}
        close() { this.readyState = 3; }
        send(data: string) {
          const request = JSON.parse(data) as { input?: Array<{ type: string; call_id?: string; output?: string }> };
          for (const item of request.input ?? []) {
            if (item.type === "function_call_output" && item.call_id) outputs.set(item.call_id, item.output ?? "");
          }
          if (++requests > 8) throw new Error("unexpected model continuation");
          const output = !outputs.has("send-reference") ? [{ type: "function_call", call_id: "send-reference",
            name: "send_project_thread", arguments: JSON.stringify({ agent_id: targetId, id: "reference", input: taskInput }) }]
            : !outputs.has("read-reference") ? [{ type: "function_call", call_id: "read-reference",
              name: "read_project_thread", arguments: JSON.stringify({ agent_id: targetId, turn_id: turnId }) }]
            : [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "Sent and read the reference." }] }];
          queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
            type: "response.completed", response: { id: `followup-response-${requests}`, status: "completed",
              ...(outputs.has("read-reference") ? { end_turn: true } : {}), output,
              usage: { input_tokens: 10, output_tokens: 10, total_tokens: 20 } },
          }) })));
        }
      }
      const runtime = (session as unknown as { env: Record<string, unknown> }).env;
      Object.defineProperty(session, "env", { value: { ...runtime,
        LOADER: { get: () => { throw new Error("browser execution is outside this test"); } },
        BROWSER: { fetch: () => { throw new Error("browser execution is outside this test"); } },
        NANOCODEX_MEMORY: { getByName: () => ({ fetch: async () => Response.json({}) }) },
        NANOCODEX: { async fetch(input: RequestInfo | URL, init?: RequestInit) {
          const request = new Request(input, init);
          if (request.headers.get("upgrade") === "websocket") return { status: 101, headers: new Headers(), webSocket: new ModelSocket() };
          return Response.json({ tools: [], machines: [], connections: [] });
        } },
      } });
      const now = Date.now();
      state.storage.sql.exec(`INSERT INTO managed_turns
        (id,request_hash,input_json,authorization_json,state,accepted_cursor,dispatch_input_chunks,may_have_inner_operation,attempt_count,created_at,accepted_at,updated_at)
        VALUES ('source','hash','"Send and read the reference"',?,'accepted',0,1,0,0,?,?,?)`, authorization, now, now, now);
      state.storage.sql.exec("INSERT INTO managed_turn_dispatch_chunks VALUES ('source',0,'\"Send and read the reference\"')");
      try {
        await session.alarm();
        const sourceTurn = () => state.storage.sql.exec<{ state: string; error: string | null }>(
          "SELECT state,error FROM managed_turns WHERE id='source'").one();
        await expect.poll(() => sourceTurn().state, { timeout: 15_000 }).not.toBe("accepted");
        expect(sourceTurn(), JSON.stringify(sourceTurn())).toMatchObject({ state: "completed" });
        expect(outputs.get("send-reference")).toContain(turnId);
        expect(outputs.get("send-reference")).toContain("accepted");
        expect(outputs.get("read-reference")).toContain(taskInput);
        expect(outputs.get("read-reference")).toContain(turnId);
        expect(state.storage.sql.exec<{ agent_id: string; turn_id: string; state: string }>(
          "SELECT agent_id,turn_id,state FROM project_thread_runs").one()).toMatchObject({ agent_id: targetId, turn_id: turnId, state: "watching" });
      } finally {
        state.storage.sql.exec("UPDATE project_thread_runs SET state='retired'");
        await state.storage.deleteAlarm();
      }
    });
    await runInDurableObject(target, async (_session, state) => {
      const rows = state.storage.sql.exec<{ id: string; input_json: string }>("SELECT id,input_json FROM managed_turns").toArray();
      expect(rows).toHaveLength(1);
      expect(rows[0]?.id).toBe(turnId);
      expect(JSON.parse(rows[0]!.input_json)).toBe(taskInput);
    });
  } finally {
    await runInDurableObject(target, async (_session, state) => {
      state.storage.sql.exec("UPDATE managed_turns SET state='cancelled',retry_at=NULL");
      await state.storage.deleteAlarm();
    });
  }
}, 30_000);
