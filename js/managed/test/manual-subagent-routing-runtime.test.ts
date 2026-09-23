import { env, runInDurableObject } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { DEFAULT_AGENT_SETTINGS } from "../src/agent-settings";
import { forwardPrincipalAssertions, type Principal } from "../src/account-auth";

// Keyless integration: only identity/ancillary services and model responses are
// synthetic. Admission, provider adapters, Rust/WASM subagent tools,
// Just Bash, live child bindings and durable root reconstruction are production.
const principal: Principal = {
  kind: "api_key", userId: "11111111-1111-4111-8111-111111111111",
  organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
  role: "owner", subjectId: "user:child-routing-fixture", credentialId: "child-routing-fixture",
  authorizationEpoch: 1, capabilities: ["agents:read", "agents:write", "tools:use"],
};
const marker = "synthetic-child-tool-proof";
const schema = { type: "object", properties: { value: { type: "string" }, turn: { type: "integer" } },
  required: ["value", "turn"], additionalProperties: false };
const completion = (message: unknown, tool = false) => ({ choices: [{ finish_reason: tool ? "tool_calls" : "stop", message }] });
function toolCall(input: any, name: string, args: unknown, id: string) {
  const declaration = input.tools.find((tool: any) => tool.function.description.startsWith(`${name}\n`));
  expect(declaration, `${name} must come from the actual Rust/WASM tool catalog`).toBeDefined();
  return completion({ content: null, tool_calls: [{ id, type: "function", function: {
    name: declaration.function.name, arguments: JSON.stringify(args),
  } }] }, true);
}

function providerSse(value: any, native: boolean) {
  const events = native
    ? [{ type: "response.completed", response: value }]
    : [{ choices: value.choices.map((choice: any) => ({ index: 0, finish_reason: choice.finish_reason,
      delta: { ...choice.message, ...(choice.message.tool_calls ? { tool_calls: choice.message.tool_calls.map((call: any, index: number) => ({ ...call, index })) } : {}) },
    })) }];
  return new Response(events.map(event => `data: ${JSON.stringify(event)}\n\n`).join("")
    + (native ? "" : "data: [DONE]\n\n"), { headers: { "content-type": "text/event-stream" } });
}
function toNative(chat: any) {
  const message = chat.choices[0].message;
  return { object: "response", status: "completed", output: message.tool_calls?.map((call: any) => ({
    type: "function_call", call_id: call.id, name: call.function.name, arguments: call.function.arguments,
  })) ?? [{ type: "message", role: "assistant", content: [{ type: "output_text", text: message.content }] }] };
}

it.each([
  [false, undefined, undefined], [false, "luna", "max"],
  [false, "kimi", "low"], [true, "kimi", "low"],
] as const)("manual GPT root with routing=%s and child=%s/%s preserves native spawning or enforces gateway admission", async (routingEnabled, model, thinking) => {
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
    const denied = model === "kimi" && !routingEnabled;
    const childIsGateway = model === "kimi";
    let rootStep = 0, childCalls = 0, gatewayCalls = 0, childId: number | undefined;
    const sockets: WebSocket[] = [];
    const tool = (body: any, name: string, args: unknown) => toolCall(body, name, args, crypto.randomUUID());
    const childResponse = (body: any) => {
      childCalls++;
      expect(childCalls).toBeLessThanOrEqual(2);
      if (body.messages.at(-1)?.role === "tool") return completion({ content: "MANUAL_CHILD_DONE" });
      return tool(body, "submit_result", { output: JSON.stringify({ value: marker, turn: 1 }) });
    };
    const rootResponse = (body: any) => {
      expect(rootStep).toBeLessThan(10);
      if (rootStep++ === 0) return tool(body, "spawn_agent", {
        role: "manual fixture", task: "Return the requested schema.", output_schema: schema,
        ...(model === undefined ? {} : { model }), ...(thinking === undefined ? {} : { thinking }),
      });
      const last = body.messages.at(-1);
      if (denied) {
        expect(last.role).toBe("tool");
        expect(last.content).toContain("subagent routing failed or was not authorized");
        return completion({ content: "GATEWAY_DENIED" });
      }
      const result = last?.role === "tool" ? JSON.parse(last.content) : undefined;
      childId ??= result?.agent_id;
      expect(childId).toBeTypeOf("number");
      const child = result?.agents?.find((agent: any) => agent.agent_id === childId);
      if (child?.status.state === "completed") {
        expect(child.status.output).toEqual({ value: marker, turn: 1 });
        return completion({ content: "MANUAL_ROOT_DONE" });
      }
      return tool(body, "wait_agent", { agent_ids: [childId], timeout_ms: 5_000 });
    };
    const original = (session as unknown as { env: Record<string, unknown> }).env;
    Object.defineProperty(session, "env", { configurable: true, value: { ...original,
      NANOCODEX_THREAD_ROUTING: String(routingEnabled), OPENROUTER_API_KEY: "synthetic-availability-only",
      AI_GATEWAY_API_KEY: undefined, AI: { run: () => { throw new Error("Manual native spawning must not classify"); } },
      NANOCODEX_MEMORY: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
      NANOCODEX_USERS: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
      NANOCODEX_ACCOUNT_TOOLS: { getByName: () => ({ fetch: async () => Response.json({ tools: [], machines: [], connections: [] }) }) },
      NANOCODEX: { fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
        const req = new Request(input, init), url = new URL(req.url);
        if (url.hostname === "broker.internal" && ["PUT", "DELETE"].includes(req.method)) return new Response(null, { status: 204 });
        if (url.hostname !== "nanocodex.internal") return Response.json({ tools: [], machines: [], connections: [], accounts: {} });
        expect(req.method).toBe("GET");
        const rootSessionId = state.storage.sql.exec<{ session_id: string }>("SELECT session_id FROM nanocodex_cloudflare_agent").one().session_id;
        const isRoot = req.headers.get("thread-id") === rootSessionId;
        if (!isRoot) expect(childIsGateway).toBe(false);
        const pair = new WebSocketPair(), server = pair[1];
        server.accept();
        sockets.push(server);
        let declarations: any[] = [];
        server.addEventListener("message", event => {
          const request = JSON.parse(String(event.data));
          expect(request.model).toBe(isRoot || model === undefined ? "gpt-6-astra" : "gpt-6-luna");
          expect(request.reasoning.effort).toBe("max");
          declarations = request.input.findLast((item: any) => item.type === "additional_tools")?.tools ?? request.tools ?? declarations;
          const normalized = { tools: declarations.map((declaration: any) => ({ function: {
            ...declaration, description: `${declaration.name}\n${declaration.description ?? ""}`,
          } })),
            messages: request.input.map((item: any) => item.type === "function_call_output" ? { role: "tool", content: item.output } : item) };
          const response = request.generate === false ? { output: [] }
            : toNative(isRoot ? rootResponse(normalized) : childResponse(normalized));
          server.send(JSON.stringify({ type: "response.completed", response: { ...response, id: crypto.randomUUID(),
            status: "completed", usage: { input_tokens: 10, output_tokens: 1, total_tokens: 11 } } }));
        });
        return new Response(null, { status: 101, webSocket: pair[0] });
      } },
    } });
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const req = new Request(input, init);
      expect(childIsGateway && routingEnabled).toBe(true);
      expect(req.url).toBe("https://openrouter.ai/api/v1/chat/completions");
      const body = await req.json() as any;
      expect(body.model).toBe("moonshotai/kimi-k3");
      expect(body.reasoning.effort).toBe("low");
      gatewayCalls++;
      return providerSse(childResponse(body), false);
    });
    const request = (path: string, body?: unknown) => {
      const headers = new Headers({ "content-type": "application/json" });
      if (path !== "/create") forwardPrincipalAssertions(headers, principal);
      return session.fetch(new Request(`https://session.internal${path}`, { method: body ? "POST" : "GET", headers,
        ...(body ? { body: JSON.stringify(body) } : {}) }));
    };
    try {
      expect((await request("/create", { session_id: crypto.randomUUID(), owner_id: principal.userId,
        organization_id: principal.organizationId, team_id: principal.teamId, authorization_epoch: 1,
        public_origin: "https://nanocodex.example", settings: { ...DEFAULT_AGENT_SETTINGS, model: "gpt-6-astra", thinking: "max" },
        configuration: { multi_agent: { enabled: true }, tools: ["exec_command"], environment: { network: { access: "disabled" } } },
      })).status).toBe(200);
      expect((await request("/turns", { id: "manual-routing-turn", input: "Run the manual child fixture." })).status).toBe(202);
      await expect.poll(() => state.storage.sql.exec("SELECT state,error FROM managed_turns WHERE id='manual-routing-turn'").one(), { timeout: 15_000 })
        .toEqual({ state: "completed", error: null });
      const receipt = await (await request("/turns/manual-routing-turn")).json() as any;
      expect(receipt.terminal.final_message).toBe(denied ? "GATEWAY_DENIED" : "MANUAL_ROOT_DONE");
      expect(childCalls).toBe(denied ? 0 : 2);
      expect(gatewayCalls).toBe(childIsGateway && !denied ? 2 : 0);
      expect(state.storage.sql.exec("SELECT * FROM managed_thread_route").toArray()).toEqual([]);
    } finally {
      fetchSpy.mockRestore();
      sockets.forEach(socket => socket.close(1000, "fixture complete"));
      await state.storage.deleteAlarm();
    }
  });
}, 30_000);
