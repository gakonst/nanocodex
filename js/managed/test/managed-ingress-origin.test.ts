import { createExecutionContext, env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import worker, { type DurableAgentSession } from "../src/index";
import { DEFAULT_AGENT_SETTINGS } from "../src/agent-settings";
import { forwardPrincipalAssertions, type Principal } from "../src/account-auth";
import { ROUTING_CANDIDATES } from "../src/thread-model-routing";
import { summarizeProviderObservationGroups, type ProviderObservation } from "../src/provider-telemetry";

const header = "x-nanocodex-client-ingress-colo";
const principal: Principal = {
  kind: "api_key", userId: "11111111-1111-4111-8111-111111111111",
  organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
  role: "owner", subjectId: "user:origin-fixture", credentialId: "origin-fixture",
  authorizationEpoch: 1, capabilities: ["agents:read", "agents:write", "tools:use"],
};
const publicRequest = (path: string, colo: unknown, body?: unknown) => new Request(`https://nanocodex.example${path}`, {
  method: "POST", cf: { colo }, headers: { [header]: "SFO", "x-nanocodex-worker-colo": "SFO", "idempotency-key": "origin-fixture" },
  ...(body === undefined ? {} : { body: JSON.stringify(body) }),
});

describe("trusted managed ingress", () => {
  it.each(["FRA", undefined, "not-a-colo", "fra"])("rebuilds routing headers from platform metadata (%s)", async colo => {
    const runtime = { ...env, NANOCODEX_SESSIONS: { getByName: () => ({ fetch: async (_url: string, init: RequestInit) => {
      const headers = new Headers(init.headers);
      expect(headers.get(header)).toBe(colo === "FRA" ? "FRA" : null);
      expect(headers.has("x-nanocodex-worker-colo")).toBe(false);
      return Response.json({ enabled: true });
    } }) } } as unknown as Parameters<typeof worker.fetch>[1];
    const response = await worker.fetch(publicRequest("/v1/agents/0198d3f0-8844-7000-8000-000000000092/routing", colo), runtime, createExecutionContext(), principal);
    expect(response.status).toBe(200);
  });

  it("rebuilds the live WebSocket creation assertion from trusted ingress", async () => {
    const runtime = { ...env, NANOCODEX_SESSIONS: { getByName: () => ({ fetch: async (_url: string, request: Request) => {
      expect(request.headers.get(header)).toBe("FRA");
      expect(request.headers.has("x-nanocodex-worker-colo")).toBe(false);
      return new Response(null, { status: 200 });
    } }) } } as unknown as Parameters<typeof worker.fetch>[1];
    const response = await worker.fetch(new Request("https://nanocodex.example/v1/agents/live", {
      cf: { colo: "FRA" }, headers: { upgrade: "websocket", [header]: "SFO", "x-nanocodex-worker-colo": "SFO" },
    }), runtime, createExecutionContext(), principal);
    expect(response.status).toBe(200);
  });

  it.each(["/v1/agents", "/v1/agent-runs"])("preserves trusted context through %s creation", async path => {
    const calls: string[] = [];
    const runtime = { ...env, NANOCODEX_SESSIONS: {
      idFromName: () => ({ toString: () => "fixture-session" }),
      getByName: () => ({ fetch: async (input: RequestInfo, init?: RequestInit) => {
        const request = new Request(input, init);
        calls.push(new URL(request.url).pathname);
        expect(request.headers.get(header)).toBe("FRA");
        expect(request.headers.has("x-nanocodex-worker-colo")).toBe(false);
        const body: any = await request.json();
        expect(body).not.toHaveProperty("clientIngressColo");
        return new URL(request.url).pathname === "/create" ? Response.json({})
          : Response.json({ turn_id: body.id, accepted_cursor: "1" }, { status: 202 });
      } }),
    } } as unknown as Parameters<typeof worker.fetch>[1];
    const response = await worker.fetch(publicRequest(path, "FRA", path.endsWith("agent-runs") ? { input: "Synthetic task" } : undefined), runtime, createExecutionContext(), principal);
    expect(response.status).toBe(201);
    expect(calls).toEqual(path.endsWith("agent-runs") ? ["/create", "/turns"] : ["/create"]);
  });

  it.each([false, true])("retains creation origin and uses shared live metrics with probes disabled (coordinator fails: %s)", async unavailable => {
    const sessions = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
    await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
      const candidate = ROUTING_CANDIDATES.find(c => c.backend === "cloudflare" && c.model === "gpt-6-sol" && c.thinking === "low")!;
      const childCandidate = ROUTING_CANDIDATES.find(c => c.backend === "cloudflare" && c.model === "gpt-6-astra" && c.thinking === "low")!;
      let rootCalls = 0, childCalls = 0, childId: number | undefined;
      const completed = () => ({ object: "response", status: "completed", output: [{ id: "fixture-message", type: "message", role: "assistant", content: [{ type: "output_text", text: "DONE" }] }] });
      const toolCall = (input: any, name: string, args: unknown) => {
        const tool = input.tools.find((tool: any) => tool.description.startsWith(`${name}\n`));
        expect(tool).toBeDefined();
        return { object: "response", status: "completed", output: [{ id: "fixture-call", type: "function_call", call_id: `call-${rootCalls}-${childCalls}`, name: tool.name, arguments: JSON.stringify(args) }] };
      };
      const streamed = (response: any) => new ReadableStream<Uint8Array>({ start(controller) {
        const item = response.output[0];
        const frames = [
          { type: "response.output_item.added", output_index: 0, item },
          item.type === "message" ? { type: "response.output_text.delta", output_index: 0, item_id: item.id, delta: "DONE" }
            : { type: "response.function_call_arguments.delta", output_index: 0, item_id: item.id, delta: item.arguments },
          { type: "response.completed", response },
        ];
        controller.enqueue(new TextEncoder().encode(frames.map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join("")));
        controller.close();
      } });
      const observations: ProviderObservation[] = [];
      const snapshots: unknown[] = [], choices: any[] = [];
      const sample: ProviderObservation = { timestamp: Date.now(), source: "live", workerColo: null, clientIngressColo: "FRA",
        backend: candidate.backend, model: candidate.model, effort: candidate.thinking, outcome: "success", status: 200,
        headersMs: 1, generationTtftMs: 2, fullResponseMs: 3, clientDeliveryMs: null, elapsedMs: 3 };
      const shared = summarizeProviderObservationGroups([sample, sample, sample], Date.now(), { clientIngressColo: "FRA", workerColo: null });
      const original = (session as unknown as { env: Record<string, unknown> }).env;
      Object.defineProperty(session, "env", { configurable: true, value: { ...original,
        NANOCODEX_THREAD_ROUTING: "true", NANOCODEX_CLOUDFLARE_FRONTIER_ENABLED: "true", NANOCODEX_PROVIDER_PROBES: "false",
        NANOCODEX_PROVIDER_PROBE_COORDINATOR: { getByName: () => ({
          snapshot: async (origin: unknown) => { snapshots.push(origin); if (unavailable) throw Error("offline"); return shared; },
          observe: async (observation: ProviderObservation) => { observations.push(observation); if (unavailable) throw Error("offline"); return true; },
        }) },
        NANOCODEX_MEMORY: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
        NANOCODEX_USERS: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
        NANOCODEX_ACCOUNT_TOOLS: { getByName: () => ({ fetch: async () => Response.json({ tools: [], machines: [], connections: [] }) }) },
        NANOCODEX: { fetch: async () => Response.json({ tools: [], machines: [], connections: [], accounts: {} }) },
        AI: { run: async (model: string, input: any) => {
          if (model === "typesafe/jev") {
            choices.push(JSON.parse(input.state));
            return { answers: { candidate: { choice: choices.length === 1 ? candidate.id : childCandidate.id, confidence: .99 }, family: { choice: "terminal", confidence: .99 } } };
          }
          let response: any;
          if (model === "openai/gpt-6-astra") {
            childCalls++;
            response = childCalls === 1 ? toolCall(input, "submit_result", { output: { value: "synthetic" } }) : completed();
          } else {
            rootCalls++;
            if (rootCalls === 1) response = toolCall(input, "spawn_agent", { role: "origin specialist", task: "Return synthetic result",
              output_schema: { type: "object", properties: { value: { type: "string" } }, required: ["value"] } });
            else if (rootCalls === 2) {
              childId = JSON.parse(input.input.at(-1).output).agent_id;
              response = toolCall(input, "wait_agent", { agent_ids: [childId], timeout_ms: 5_000 });
            } else response = completed();
          }
          return input.stream ? streamed(response) : response;
        } },
      } });
      const request = (path: string, method: string, colo: string, body?: unknown) => {
        const headers = new Headers({ [header]: colo, "content-type": "application/json" });
        if (path !== "/create" && path !== "/initialize") forwardPrincipalAssertions(headers, principal);
        return session.fetch(new Request(`https://session.internal${path}`, { method, headers,
          ...(body === undefined ? {} : { body: JSON.stringify(body) }) }));
      };
      const initialization = { session_id: crypto.randomUUID(), owner_id: principal.userId, organization_id: principal.organizationId,
        team_id: principal.teamId, authorization_epoch: 1, public_origin: "https://nanocodex.example", settings: DEFAULT_AGENT_SETTINGS,
        configuration: { multi_agent: { enabled: true }, tools: [], environment: { network: { access: "disabled" } }, model_routing: { candidates: [candidate.id, childCandidate.id] } },
        clientIngressColo: "SFO", workerColo: "SFO" };
      try {
        expect((await request("/create", "POST", "FRA", initialization)).status).toBe(200);
        expect((await request("/initialize", "PUT", "SFO", initialization)).status).toBe(204);
        expect(state.storage.sql.exec("SELECT * FROM managed_routing_origin").toArray()).toEqual([{ singleton: 1, client_ingress_colo: "FRA" }]);
        for (const id of ["origin-turn-1", "origin-turn-2"]) {
          expect((await request("/turns", "POST", "SFO", { id, input: "Synthetic task" })).status).toBe(202);
          await expect.poll(() => state.storage.sql.exec("SELECT state,error FROM managed_turns WHERE id=?", id).one(), { timeout: 15_000 })
            .toEqual({ state: "completed", error: null });
          if (id === "origin-turn-1") {
            // Gracefully unload the agent so its next turn reconstructs from SQLite.
            state.storage.sql.exec("UPDATE session_state SET last_active=0");
            await session.alarm();
            expect(state.storage.sql.exec("SELECT * FROM managed_routing_origin").toArray()).toEqual([{ singleton: 1, client_ingress_colo: "FRA" }]);
          }
        }
        expect(choices).toHaveLength(2);
        expect(childCalls).toBe(2);
        expect(snapshots).toEqual([{ clientIngressColo: "FRA", workerColo: null }, { clientIngressColo: "FRA", workerColo: null }]);
        for (const choice of choices) expect(choice.provider_telemetry).toMatchObject({ clientIngressColo: "FRA", workerColo: null });
        const chosen = choices[0].candidates.find((c: any) => c.id === candidate.id);
        expect(chosen.responsiveness.probe).toBeNull();
        if (unavailable) expect(chosen.responsiveness.live).toBeNull();
        else expect(chosen.responsiveness.live).toMatchObject({ clientIngressColo: "FRA", workerColo: null, regionalMatch: true, regionalMatchKind: "client_ingress" });
        await vi.waitFor(() => expect(observations.length).toBeGreaterThanOrEqual(2));
        expect(observations.some(observation => observation.model === childCandidate.model)).toBe(true);
        for (const observation of observations) expect(observation).toMatchObject({ clientIngressColo: "FRA", workerColo: null });
      } finally { await state.storage.deleteAlarm(); }
    });
  }, 30_000);
});
