import { describe, expect, it, vi } from "vitest";
import { CHILD_ROUTE_TICKET_TTL_MS, createSubagentRouteController, type RetainedChildRoute } from "../src/subagent-model-routing";
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
  it("pins siblings independently, restores without classification, and never inherits a missing route", async () => {
    const { controller, options, ai } = fixture();
    const first = await controller.resolve({ ...request, model: "glm-5.3", thinking: "low" });
    const second = await controller.resolve({ ...request, model: "astra", thinking: "high" });
    controller.bind({ ...requestBinding(first.routeId), sessionId: "child-1" });
    controller.bind({ ...requestBinding(second.routeId), sessionId: "child-2" });
    const restored = createSubagentRouteController(options);
    expect(restored.routeForSession("child-1")).toMatchObject({ backend: "workers_ai", model: "@cf/zai-org/glm-5.3", thinking: "low" });
    expect(restored.routeForSession("child-2")).toMatchObject({ backend: "chatgpt", model: "gpt-6-astra", thinking: "high" });
    expect(() => restored.routeForSession("unknown")).toThrow("missing");
    expect(ai.run).toHaveBeenCalledTimes(2);
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
