import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { DEFAULT_AGENT_SETTINGS } from "../src/agent-settings";
import { forwardPrincipalAssertions, type Principal } from "../src/account-auth";
import type { DurableAgentSession } from "../src/index";

it("returns a background job ID before web completes and wakes a finished managed turn", async () => {
  const principal: Principal = {
    kind: "api_key", userId: crypto.randomUUID(), organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(),
    role: "owner", subjectId: "user:async-web-fixture", credentialId: "async-web-fixture", authorizationEpoch: 1,
    capabilities: ["agents:read", "agents:write", "tools:use"],
  };
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
    const id = crypto.randomUUID();
    const transcript: unknown[] = [];
    let modelCalls = 0;
    let searchRequests = 0;
    let releaseWeb!: (response: Response) => void;
    const web = new Promise<Response>(resolve => { releaseWeb = resolve; });
    class ModelSocket extends EventTarget {
      readyState = 1; bufferedAmount = 0;
      accept() {}
      close() { this.readyState = 3; }
      send(value: string) {
        transcript.push(JSON.parse(value));
        const step = ++modelCalls;
        const output = step === 1
          ? [{ type: "custom_tool_call", name: "exec", call_id: "background-fixture", input: 'text(await tools.start_background_web_search({search_query:[{q:"durable objects"}]}));' }]
          : [{ type: "message", role: "assistant", content: [{ type: "output_text", text: step === 2 ? "Working while search runs." : "Saw the late result." }] }];
        queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
          type: "response.completed", response: { id: `async-web-${step}`, status: "completed", output,
            usage: { input_tokens: 100, output_tokens: 10, total_tokens: 110 } },
        }) })));
      }
    }
    const original = (session as unknown as { env: Record<string, unknown> }).env;
    Object.defineProperty(session, "env", { configurable: true, value: { ...original,
      NANOCODEX: { fetch: async (input: RequestInfo | URL) => {
        const url = new URL(input instanceof Request ? input.url : String(input));
        if (url.pathname === "/v1/search") { searchRequests++; return web; }
        if (url.pathname.includes("/responses")) return { status: 101, headers: new Headers(), webSocket: new ModelSocket() };
        if (url.pathname.startsWith("/subjects/")) return new Response(null, { status: 204 });
        return Response.json({ connectors: {}, mcp_connections: [], vault: [] });
      } },
      NANOCODEX_MEMORY: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
      NANOCODEX_USERS: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
      NANOCODEX_ACCOUNT_TOOLS: { getByName: () => ({ fetch: async () => Response.json({ tools: [], machines: [] }) }) },
    } });
    const call = (path: string, body: unknown) => {
      const headers = new Headers({ "content-type": "application/json" });
      if (path !== "/create") forwardPrincipalAssertions(headers, principal);
      return session.fetch(new Request(`https://session.internal${path}`, { method: "POST", headers, body: JSON.stringify(body) }));
    };
    expect((await call("/create", { session_id: id, owner_id: principal.userId, organization_id: principal.organizationId,
      team_id: principal.teamId, authorization_epoch: 1, public_origin: "https://nanocodex.example",
      settings: DEFAULT_AGENT_SETTINGS, configuration: { async_tools: true }, })).status).toBe(200);
    try {
      expect((await call("/turns", { id: "initial", input: "Search in the background." })).status).toBe(202);
      await expect.poll(() => state.storage.sql.exec<{ state: string }>("SELECT state FROM managed_turns WHERE id = 'initial'").one().state,
        { timeout: 20_000 }).toBe("completed");
      expect(modelCalls).toBe(2); // Model advanced and finished while the web request was pending.
      const toolOutputs = (transcript[1] as { input?: Array<{ type?: string; output?: unknown }> }).input?.filter(item => item.type === "custom_tool_call_output") ?? [];
      expect(JSON.stringify(toolOutputs)).toMatch(/bg-[a-f0-9]{32}/);
      await expect.poll(() => searchRequests, { timeout: 10_000 }).toBe(1);
      releaseWeb(Response.json({ output: "Official fixture evidence" }));
      await expect.poll(() => state.storage.sql.exec<{ state: string }>("SELECT state FROM managed_background_read_jobs").one().state,
        { timeout: 10_000 }).toBe("delivered");
      await expect.poll(() => state.storage.sql.exec<{ state: string }>(
        "SELECT state FROM managed_turns WHERE id LIKE 'background:%' LIMIT 1",
      ).toArray()[0]?.state, { timeout: 20_000 }).toBe("completed");
      expect(modelCalls).toBe(3);
      expect(JSON.stringify(transcript[2])).toContain("Official fixture evidence");
      expect(state.storage.sql.exec<{ state: string }>("SELECT state FROM managed_background_read_jobs").one().state).toBe("delivered");
    } finally {
      releaseWeb(Response.json({ output: "cleanup" }));
      state.storage.sql.exec("UPDATE session_state SET last_active=0");
      await session.alarm();
      await state.storage.deleteAlarm();
    }
  });
}, 60_000);
