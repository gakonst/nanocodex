import { describe, expect, it, vi } from "vitest";
import { CHILD_ROUTE_TICKET_TTL_MS, createSubagentRouteController, subagentRoutingPolicy, type RetainedChildRoute } from "../src/subagent-model-routing";
import { ROUTING_CANDIDATES, routingPolicySchema } from "../src/thread-model-routing";

const request = { parentSessionId: "root", hostContextRef: "account-turn", role: "worker", task: "Inspect fixtures" };
function fixture(extra: Partial<Parameters<typeof createSubagentRouteController>[0]> = {}) {
  const rows = new Map<string, RetainedChildRoute>();
  const ai = { run: vi.fn(async () => { throw new Error("classifier unavailable"); }) };
  const authorize = vi.fn();
  const options = { ai, authorize, policy: routingPolicySchema.parse({}), availability: () => ({ openrouter: false, vercel: false }),
    store: { read: (id: string) => rows.get(id), commit: (id: string, value: RetainedChildRoute) => { rows.set(id, value); } }, ...extra };
  return { rows, options, controller: createSubagentRouteController(options), ai, authorize };
}

describe("hosted child routing", () => {
  it("manual root selection leaves child models independent while explicit policy constraints remain", async () => {
    const policy = routingPolicySchema.parse({ candidates: ["openrouter:moonshotai/kimi-k3:low"] });
    expect(subagentRoutingPolicy(policy, false)).toBe(policy);
    const children = subagentRoutingPolicy(policy, true);
    expect(children.candidates).toBeUndefined();
    expect(policy.candidates).toEqual(["openrouter:moonshotai/kimi-k3:low"]);
    const { controller } = fixture({ policy: children, availability: () => ({ openrouter: false, vercel: true }) });
    const selected = await controller.resolve({ ...request, model: "mimo", thinking: "medium" });
    controller.bind({ ...requestBinding(selected.routeId), sessionId: "independent-child" });
    expect(controller.routeForSession("independent-child")).toMatchObject({ backend: "vercel", model: "mimo-v2.6-pro", thinking: "medium" });
  });

  it.each([["kimi", "kimi-k3"], ["mimo", "mimo-v2.6-pro"]])("routes %s children through an authorized gateway without account-model fallback", async (alias, model) => {
    const { controller } = fixture({ availability: () => ({ openrouter: false, vercel: true }) });
    const choice = await controller.resolve({ ...request, model: alias, thinking: "high" });
    controller.bind({ ...requestBinding(choice.routeId), sessionId: "gateway-child" });
    expect(controller.routeForSession("gateway-child")).toMatchObject({ backend: "vercel", model, thinking: "high" });
    const unavailable = fixture();
    await expect(unavailable.controller.resolve({ ...request, model: alias, thinking: "high" })).rejects.toThrow();
  });

  it("pins live siblings independently and drops routes when their runtime is replaced", async () => {
    const { controller, ai } = fixture();
    const first = await controller.resolve({ ...request, model: "glm-5.3", thinking: "low" });
    const second = await controller.resolve({ ...request, model: "astra", thinking: "high" });
    controller.bind({ ...requestBinding(first.routeId), sessionId: "child-1" });
    controller.bind({ ...requestBinding(second.routeId), sessionId: "child-2" });
    expect(controller.routeForSession("child-1")).toMatchObject({ backend: "workers_ai", model: "@cf/zai-org/glm-5.3", thinking: "low" });
    expect(controller.routeForSession("child-2")).toMatchObject({ backend: "chatgpt", model: "gpt-6-astra", thinking: "high" });
    expect(() => controller.routeForSession("unknown")).toThrow("missing");
    expect(ai.run).not.toHaveBeenCalled();
    expect(controller.routeForSession("child-1").classifier).toEqual({ outcome: "not_requested", attempts: [] });
    expect(controller.routeForSession("child-1").audit).toBeUndefined();
    const replacement = fixture().controller;
    expect(() => replacement.routeForSession("child-1")).toThrow("missing");
    expect(() => replacement.routeForSession("child-2")).toThrow("missing");
  });

  it("retains distinct provider choices for the same canonical model and routes nested tasks anew", async () => {
    const candidates = ["openrouter", "vercel"].map(backend => ROUTING_CANDIDATES.find(
      candidate => candidate.backend === backend && candidate.model === "gpt-6-astra" && candidate.thinking === "high",
    )!);
    let decisions = 0;
    const ai = { run: vi.fn(async () => ({ answers: {
      candidate: { choice: candidates[decisions++ % 2]!.id, confidence: 0.99 },
      family: { choice: "repository_repair", confidence: 0.99 },
    } })) };
    const { controller, authorize } = fixture({ ai, availability: () => ({ openrouter: true, vercel: true }) });
    const first = await controller.resolve({ ...request, model: "astra", thinking: "high" });
    controller.bind({ ...requestBinding(first.routeId), sessionId: "child" });
    const nested = await controller.resolve({ ...request, parentSessionId: "child", task: "Review child changes",
      model: "astra", thinking: "high" });
    controller.bind({ ...requestBinding(nested.routeId), parentSessionId: "child", sessionId: "nested" });
    expect(controller.routeForSession("child").backend).toBe("openrouter");
    expect(controller.routeForSession("nested").backend).toBe("vercel");
    expect(authorize).toHaveBeenCalledWith("child", "account-turn");
    expect(ai.run).toHaveBeenCalledTimes(2);
  });

  it("selects Cloudflare for a new child only with availability and keeps its live pin", async () => {
    const id = "cloudflare:openai/gpt-6-astra:high";
    const ai = {run:vi.fn(async()=>({answers:{candidate:{choice:id,confidence:.99},family:{choice:"terminal",confidence:.99}}}))};
    const policy = routingPolicySchema.parse({candidates:[id]});
    const disabled = fixture({ai,policy});
    await expect(disabled.controller.resolve({...request,model:"astra",thinking:"high"})).rejects.toThrow("No eligible");
    expect(ai.run).not.toHaveBeenCalled();
    let cloudflare = true;
    const enabled = fixture({ai,policy,availability:()=>({openrouter:false,vercel:false,cloudflare})});
    const resolved = await enabled.controller.resolve({...request,model:"astra",thinking:"high"});
    enabled.controller.bind({...requestBinding(resolved.routeId),sessionId:"cloudflare-child"});
    cloudflare = false;
    expect(enabled.controller.routeForSession("cloudflare-child")).toMatchObject({backend:"cloudflare",model:"gpt-6-astra",provider_model:"openai/gpt-6-astra",thinking:"high"});
    expect(ai.run).not.toHaveBeenCalled();
  });

  it("rejects policy-ineligible explicit model or effort without classification", async () => {
    const { controller, ai } = fixture({ policy: routingPolicySchema.parse({ candidates: ["@cf/zai-org/glm-5.3:low"] }) });
    await expect(controller.resolve({ ...request, model: "astra" })).rejects.toThrow("outside eligible");
    await expect(controller.resolve({ ...request, thinking: "high" })).rejects.toThrow("outside eligible");
    expect(ai.run).not.toHaveBeenCalled();
  });

  it("fails closed on authority loss during resolution and again during bind", async () => {
    let admitted = true;
    const { controller, rows } = fixture({ authorize: () => { if (!admitted) throw new Error("authority lost"); },
      ai: { run: async () => { admitted = false; throw new Error("timeout"); } } });
    await expect(controller.resolve(request)).rejects.toThrow("authority lost");
    expect(rows.size).toBe(0);
    const other = fixture({ authorize: () => { if (!admitted) throw new Error("authority lost"); } });
    admitted = true;
    const resolved = await other.controller.resolve(request);
    admitted = false;
    expect(() => other.controller.bind({ ...requestBinding(resolved.routeId), sessionId: "child" })).toThrow("authority lost");
    expect(other.rows.size).toBe(0);
  });

  it("binds only to the authorized parent and context, exactly once, with durable retry", async () => {
    const { controller, options, rows } = fixture();
    const resolved = await controller.resolve(request);
    const binding = { ...requestBinding(resolved.routeId), sessionId: "child" };
    expect(() => controller.bind({ ...binding, hostContextRef: "other-turn" })).toThrow("unauthorized");
    expect(() => controller.bind({ ...binding, parentSessionId: "other-parent" })).toThrow("unauthorized");
    expect(() => controller.bind({ ...binding, sessionId: "root" })).toThrow("replace parent");
    const commit = options.store.commit;
    options.store.commit = () => { throw new Error("storage failed"); };
    expect(() => controller.bind(binding)).toThrow("storage failed");
    expect(rows.size).toBe(0);
    options.store.commit = commit;
    controller.bind(binding);
    controller.bind(binding);
    expect(() => controller.bind({ ...binding, sessionId: "another-child" })).toThrow("unauthorized");
    expect(() => controller.bind({ ...binding, routeId: "other" })).toThrow("conflicts");
    expect(rows.size).toBe(1);
  });

  it("reclaims abandoned tickets at expiry without rerouting or expiring retained children", async () => {
    let time = 0;
    const { controller, rows, ai } = fixture({ now: () => time });
    const retained = await controller.resolve(request);
    controller.bind({ ...requestBinding(retained.routeId), sessionId: "retained-child" });
    const pinned = rows.get("retained-child");
    const abandoned = await Promise.all(Array.from({ length: 64 }, () => controller.resolve(request)));
    await expect(controller.resolve(request)).rejects.toThrow("Too many pending");
    time = CHILD_ROUTE_TICKET_TTL_MS - 1;
    await expect(controller.resolve(request)).rejects.toThrow("Too many pending");
    time++;
    const replacement = await controller.resolve(request);
    controller.bind({ ...requestBinding(replacement.routeId), sessionId: "replacement-child" });
    expect(() => controller.bind({ ...requestBinding(abandoned[0]!.routeId), sessionId: "expired-child" }))
      .toThrow("Unknown or unauthorized");
    controller.bind({ ...requestBinding(retained.routeId), sessionId: "retained-child" });
    expect(rows.get("retained-child")).toBe(pinned);
    expect(controller.routeForSession("retained-child")).toBe(pinned!.route);
    expect(rows.size).toBe(2);
    expect(ai.run).toHaveBeenCalledTimes(66);
  });

  it("rejects an expired ticket at bind even without another resolve", async () => {
    let time = 0;
    const { controller, rows } = fixture({ now: () => time });
    const expired = await controller.resolve(request);
    time = CHILD_ROUTE_TICKET_TTL_MS;
    expect(() => controller.bind({ ...requestBinding(expired.routeId), sessionId: "expired-child" }))
      .toThrow("Unknown or unauthorized");
    expect(rows.size).toBe(0);
  });

  it("starts ticket age after classification and permits a retry before expiry", async () => {
    let time = 0;
    const { controller, options, rows } = fixture({ now: () => time,
      ai: { run: async () => { time += CHILD_ROUTE_TICKET_TTL_MS; throw new Error("classifier unavailable"); } } });
    const resolved = await controller.resolve(request);
    const binding = { ...requestBinding(resolved.routeId), sessionId: "child" };
    const commit = options.store.commit;
    options.store.commit = () => { throw new Error("storage failed"); };
    expect(() => controller.bind(binding)).toThrow("storage failed");
    time += CHILD_ROUTE_TICKET_TTL_MS - 1;
    options.store.commit = commit;
    controller.bind(binding);
    expect(rows.size).toBe(1);
  });

  it("bounds concurrent classifier admissions before they complete", async () => {
    let release!: () => void;
    const held = new Promise<void>(resolve => { release = resolve; });
    const { controller } = fixture({ ai: { run: async () => { await held; throw new Error("unavailable"); } } });
    const admitted = Array.from({ length: 64 }, () => controller.resolve(request));
    await expect(controller.resolve(request)).rejects.toThrow("Too many pending");
    release();
    await Promise.all(admitted);
  });
});
function requestBinding(routeId: string) {
  return { parentSessionId: request.parentSessionId, hostContextRef: request.hostContextRef, routeId };
}
