import { env, runInDurableObject } from "cloudflare:test";
import { expect, it, vi } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { DEFAULT_AGENT_SETTINGS } from "../src/agent-settings";
import { forwardPrincipalAssertions, type Principal } from "../src/account-auth";
import { ROUTING_CANDIDATES, OSS_MODEL } from "../src/thread-model-routing";

// Keyless integration: only identity/ancillary services and model responses are
// synthetic. Admission, Jev policy, provider adapters, Rust/WASM subagent tools,
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

// Normalize only the synthetic provider fixture; production transports still
// receive native Responses versus Chat Completions and run the real WASM loop.
function fromNative(input: any) {
  expect(input).toMatchObject({ stream: true, store: false });
  expect(input).not.toHaveProperty("messages");
  return { tools: input.tools.map((tool: any) => ({ type: "function", function: tool })),
    messages: input.input.map((item: any) => item.type === "function_call_output"
      ? { role: "tool", content: item.output } : item) };
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
  ["openrouter", "workers_ai", "binding"], ["vercel", "workers_ai", "binding"],
  ["cloudflare", "workers_ai", "binding"], ["openrouter", "cloudflare", "binding"], ["cloudflare", "cloudflare", "binding"],
  ["cloudflare", "workers_ai", "rest"], ["openrouter", "cloudflare", "rest"], ["cloudflare", "cloudflare", "rest"],
] as const)("opt-in %s root and %s child (%s) pin independent live transports and never resurrect children after unload", async (provider, childProvider, transport) => {
  const childModel = childProvider === "cloudflare" ? "gpt-6-astra" : OSS_MODEL;
  const childCandidate = ROUTING_CANDIDATES.find(c => c.backend === childProvider && c.model === childModel && c.thinking === "high")!;
  const rootCandidate = ROUTING_CANDIDATES.find(c => c.backend === provider && c.model === "gpt-5.6-sol" && c.thinking === "low")!;
  const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
    let choices = 0, rootCalls = 0, childCalls = 0, childToolResults = 0, submissions = 0;
    let phase = 1, step = 0, childId: number | undefined;
    const childInference = Promise.withResolvers<void>();
    const observedTools: string[] = [];
    const sql = state.storage.sql;
    const table = (name: string) => sql.exec(`SELECT * FROM ${name}`).toArray();
    const expectNoDurableChildren = () => {
      expect(sql.exec("SELECT name FROM sqlite_master WHERE name IN ('managed_subagent_routes', 'managed_subagent_authorizations', 'nanocodex_cloudflare_subagents', 'nanocodex_cloudflare_subagent_checkpoints')").toArray()).toEqual([]);
    };
    const request = (path: string, method: string, body?: unknown) => {
      const headers = new Headers({ "content-type": "application/json" });
      if (path !== "/create") forwardPrincipalAssertions(headers, principal);
      return session.fetch(new Request(`https://session.internal${path}`, { method, headers,
        ...(body === undefined ? {} : { body: JSON.stringify(body) }) }));
    };
    const call = (input: any, name: string, args: unknown) => {
      observedTools.push(name);
      return toolCall(input, name, args, `root-${rootCalls}`);
    };
    const modelResponse = async (model: string, input: any) => {
        if (model === "typesafe/jev") {
          choices++;
          expect(choices).toBeLessThanOrEqual(2);
          const chooserState = JSON.parse(input.state);
          const candidate = choices === 1 ? rootCandidate : childCandidate;
          expect(Object.keys(input.questions.candidate.criteria)).toContain(candidate.id);
          if (choices === 2) expect(chooserState.opening_prompt).toContain("child-fixture-task");
          return { answers: { candidate: { choice: candidate.id, confidence: .99 }, family: { choice: "terminal", confidence: .99 } } };
        }
        if (provider === "cloudflare" && model === "openai/gpt-5.6-sol") {
          expect(input.reasoning).toEqual({ effort: "low" });
          return toNative(await (await rootResponse(fromNative(input))).json());
        }
        const nativeInput = input;
        if (childProvider === "cloudflare") {
          expect(model).toBe("openai/gpt-6-astra");
          expect(input.reasoning).toEqual({ effort: "high" });
          input = fromNative(input);
        }
        const handleChild = async () => {
          childCalls++;
          if (childCalls === 1) await childInference.promise;
          expect(childCalls).toBeLessThanOrEqual(6);
          if (childProvider === "workers_ai") {
            expect(model).toBe(OSS_MODEL);
            expect(nativeInput.reasoning_effort).toBe("high");
          }
          expectNoDurableChildren();
          // Result revisions remain runtime-owned after child durability is removed.
          expect(JSON.stringify(input.messages)).not.toMatch(/turn_token: \d+/);
          const childTurn = phase;
          const last = input.messages.at(-1);
          if (last?.role === "tool" && last.content.includes('"accepted":true')) {
            expect(JSON.parse(last.content)).toMatchObject({ accepted: true, status: "accepted", decoded_json_text: true });
            submissions++;
            return completion({ content: `CHILD_DONE_${childTurn}` });
          }
          if (childTurn === 1 && childCalls === 1) {
            observedTools.push("child:exec_command");
            return toolCall(input, "exec_command", { cmd: "cat /brain/child-input.txt", workdir: "/brain" }, "child-read");
          }
          if (childTurn === 1) {
            expect(last.role).toBe("tool");
            expect(last.content).toContain(marker);
            childToolResults++;
          } else {
            expect(JSON.stringify(input.messages)).toContain(marker);
            expect(JSON.stringify(input.messages)).toContain("CHILD_DONE_1");
          }
          observedTools.push("child:submit_result");
          return toolCall(input, "submit_result", { output: JSON.stringify({ value: marker, turn: childTurn }) }, `submit-${childTurn}`);
        };
        const result = await handleChild();
        return childProvider === "cloudflare" ? toNative(result) : result;
    };
    const original = (session as unknown as { env: Record<string, unknown> }).env;
    Object.defineProperty(session, "env", { configurable: true, value: { ...original,
      NANOCODEX_THREAD_ROUTING: "true", NANOCODEX_CLOUDFLARE_FRONTIER_ENABLED: "true", OPENROUTER_API_KEY: "synthetic-availability-only", AI_GATEWAY_API_KEY: "synthetic-availability-only", AGENT_IDLE_TIMEOUT_MS: "1000",
      NANOCODEX_MEMORY: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
      NANOCODEX_USERS: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
      NANOCODEX_ACCOUNT_TOOLS: { getByName: () => ({ fetch: async () => Response.json({ tools: [], machines: [], connections: [] }) }) },
      NANOCODEX: { fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
        const req = new Request(input, init), url = new URL(req.url);
        if (url.hostname === "broker.internal" && ["PUT", "DELETE"].includes(req.method)) return new Response(null, { status: 204 });
        return Response.json({ tools: [], machines: [], connections: [], accounts: {} });
      } },
      ...(transport === "rest" ? { CLOUDFLARE_AI_API_TOKEN: "synthetic-cloudflare-token", NANOCODEX_CLOUDFLARE_ACCOUNT_ID: "a".repeat(32) } : {}),
      AI: { run: async (model: string, input: any) => {
        if (transport === "rest") expect(model.startsWith("openai/")).toBe(false);
        return modelResponse(model, input);
      } },
    } });
    const rootResponse = async (body: any) => {
      rootCalls++;
      expect(rootCalls).toBeLessThanOrEqual(20);
      if (provider !== "cloudflare") {
        expect(body.model).toBe("openai/gpt-5.6-sol");
        expect(provider === "openrouter" ? body.reasoning.effort : body.reasoning_effort).toBe("low");
      }
      expect(JSON.parse(String(table("managed_thread_route")[0].route_json))).toMatchObject({ backend: provider, model: "gpt-5.6-sol", thinking: "low" });
      const last = body.messages.at(-1);
      if (phase === 1 && step++ === 0) return Response.json(call(body, "spawn_agent", {
        role: "fixture specialist", task: "child-fixture-task: read /brain/child-input.txt and submit the value.", output_schema: schema,
      }));
      if (phase === 1 && childId === undefined) {
        childId = JSON.parse(last.content).agent_id;
        expect(childId).toBeTypeOf("number");
        return Response.json(completion({ content: "ROOT_DONE_1" }));
      }
      if (phase === 2 && step++ === 0) return Response.json(call(body, "send_agent_message", {
        agent_id: childId, message: "Recall the prior value from history and submit it for turn 2.", purpose: "delegate",
      }));
      if (phase < 3) {
        const receipt = last?.role === "tool" ? JSON.parse(last.content) : undefined;
        const child = receipt?.agents?.find((agent: any) => agent.agent_id === childId);
        if (child?.status.state === "completed") {
          expect(child.status.output).toEqual({ value: marker, turn: phase });
          return Response.json(completion({ content: `ROOT_DONE_${phase}` }));
        }
        return Response.json(call(body, "wait_agent", { agent_ids: [childId], timeout_ms: 5_000 }));
      }
      if (step++ === 0) return Response.json(call(body, "list_agents", { include_completed: true }));
      if (step === 2) {
        expect(JSON.parse(last.content).agents).toEqual([]);
        return Response.json(call(body, "close_agent", { agent_id: childId }));
      }
      return Response.json(completion({ content: "ROOT_DONE_3" }));
    };
    const fetchSpy = vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const req = new Request(input, init);
      if (new URL(req.url).hostname === "api.cloudflare.com") {
        expect(transport).toBe("rest");
        expect(req.url).toBe(`https://api.cloudflare.com/client/v4/accounts/${"a".repeat(32)}/ai/v1/responses`);
        expect(req.headers.get("authorization")).toBe("Bearer synthetic-cloudflare-token");
        expect(req.redirect).toBe("manual");
        const body = await req.json() as any;
        expect(body.stream).toBe(true);
        return providerSse(await modelResponse(body.model, body), true);
      }
      expect(provider).not.toBe("cloudflare");
      expect(req.url).toBe(provider === "openrouter" ? "https://openrouter.ai/api/v1/chat/completions" : "https://ai-gateway.vercel.sh/v1/chat/completions");
      const body = await req.json() as any;
      expect(body.stream).toBe(true);
      return providerSse(await (await rootResponse(body)).json(), false);
    });
    try {
      expect((await request("/create", "POST", {
        session_id: crypto.randomUUID(), owner_id: principal.userId, organization_id: principal.organizationId,
        team_id: principal.teamId, authorization_epoch: 1, public_origin: "https://nanocodex.example", settings: DEFAULT_AGENT_SETTINGS,
        configuration: { multi_agent: { enabled: true },
          tools: ["exec_command"], environment: { network: { access: "disabled" }, files: [{ path: "/brain/child-input.txt", content: marker + "\n" }] } },
      })).status).toBe(200);
      expect(await (await request("/state", "GET")).json()).toMatchObject({ model_routing_enabled: false, model_route: null });
      const beforeEnable: any = await (await request("/configuration", "GET")).json();
      // This is the production /autoroute admission sequence: create normally,
      // explicitly enable the empty thread, then send its first prompt.
      const enabled = await request("/routing", "POST");
      expect(enabled.status).toBe(200);
      expect(await enabled.json()).toMatchObject({ enabled: true });
      expect(await (await request("/configuration", "GET")).json()).toMatchObject({
        ...beforeEnable, model_routing: { strategy: "direct" },
      });
      expect(await (await request("/state", "GET")).json()).toMatchObject({ model_routing_enabled: true, model_route: null });
      expect(choices).toBe(0);
      expect(table("managed_thread_route")).toHaveLength(0);
      let rootPin: unknown;
      for (phase = 1; phase <= 3; phase++) {
        step = 0;
        const id = `fixture-turn-${phase}`;
        expect((await request("/turns", "POST", { id, input: `Run fixture phase ${phase}.` })).status).toBe(202);
        await expect.poll(() => sql.exec("SELECT state,error FROM managed_turns WHERE id=?", id).one(), { timeout: 15_000 })
          .toEqual({ state: "completed", error: null });
        const receipt: any = await (await request(`/turns/${id}`, "GET")).json();
        expect(receipt.terminal.final_message).toBe(`ROOT_DONE_${phase}`);
        if (phase === 1) {
          rootPin = table("managed_thread_route");
          // The root is finished, but a child still owns an in-flight model call.
          // Aging the root must not unload that ephemeral child.
          sql.exec("UPDATE session_state SET last_active=0");
          await session.alarm();
          expect(await (await request("/state", "GET")).json()).toMatchObject({ agent_loaded: true });
          childInference.resolve();
          await expect.poll(() => submissions, { timeout: 15_000 }).toBe(1);
        }
        expect(table("managed_thread_route")).toEqual(rootPin);
        // Route selection must reach live/reconnecting clients through durable
        // history, exactly once and before the first turn settles.
        const routeEvents = sql.exec<{ cursor: number; message_json: string }>(
          "SELECT cursor,message_json FROM managed_events WHERE turn_id = 'fixture-turn-1' AND json_extract(message_json, '$.model_route.model') IS NOT NULL",
        ).toArray();
        expect(routeEvents).toHaveLength(1);
        expect(JSON.parse(routeEvents[0].message_json)).toMatchObject({ type: "event", event: { type: "run.started" },
          model_route: { backend: provider, model: "gpt-5.6-sol", thinking: "low" }, model_routing_automatic: true });
        const opening = sql.exec<{ accepted_cursor: number; terminal_cursor: number }>(
          "SELECT accepted_cursor,terminal_cursor FROM managed_turns WHERE id = 'fixture-turn-1'",
        ).one();
        expect(routeEvents[0].cursor).toBeGreaterThan(opening.accepted_cursor);
        expect(routeEvents[0].cursor).toBeLessThan(opening.terminal_cursor);
        // Footer/status must report the retained root route even while a child
        // has used another provider/model and after the child is closed.
        const status: any = await (await request("/state", "GET")).json();
        expect(status.model_routing_enabled).toBe(true);
        expect(status.model_route).toEqual(JSON.parse(String(table("managed_thread_route")[0].route_json)));
        expect(status.model_route).toMatchObject({ backend: provider, model: "gpt-5.6-sol", thinking: "low" });
        expect(choices).toBe(2);
        expectNoDurableChildren();
        if (phase === 2) {
          if (provider !== "cloudflare" && childProvider !== "cloudflare") {
            (session as unknown as { env: Record<string, unknown> }).env.NANOCODEX_CLOUDFLARE_FRONTIER_ENABLED = "false";
          }
          // Drive the production idle alarm after aging only its activity clock.
          sql.exec("UPDATE session_state SET last_active=0");
          await session.alarm();
          expect(await (await request("/state", "GET")).json()).toMatchObject({ agent_loaded: false });
          expectNoDurableChildren();
          // A fresh client connection reads the retained root settings without
          // selecting a route or reconstructing a child on the read itself.
          const headers = new Headers({ upgrade: "websocket" });
          forwardPrincipalAssertions(headers, principal);
          const connection = await session.fetch(new Request("https://session.internal/socket", { headers }));
          expect(connection.status).toBe(101);
          const socket = connection.webSocket!;
          const ready = new Promise<any>(resolve => socket.addEventListener("message", event => resolve(JSON.parse(String(event.data))), { once: true }));
          socket.accept();
          expect(await ready).toMatchObject({ type: "ready", restored: true, active_turns: [], settings: {
            model: "gpt-5.6-sol", thinking: "low", reasoning_mode: "standard", fast_mode: false,
          } });
          socket.close(1000, "fixture reconnect complete");
          expect(choices).toBe(2);
          expect(table("managed_thread_route")).toEqual(rootPin);
          expectNoDurableChildren();
        }
      }
      expectNoDurableChildren();
      expect(childToolResults).toBe(1);
      expect(submissions).toBe(2);
      expect(childCalls).toBe(5);
      expect(observedTools).toEqual(expect.arrayContaining(["spawn_agent", "wait_agent", "send_agent_message", "list_agents", "close_agent", "child:exec_command", "child:submit_result"]));
    } finally {
      childInference.resolve();
      fetchSpy.mockRestore();
      await state.storage.deleteAlarm();
    }
  });
}, 60_000);
