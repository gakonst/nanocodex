import { describe, expect, it, vi } from "vitest";
import { runJev, JEV_ROUTING_BUDGET_MS, type JevDiagnostics } from "../src/jev-reliability";
import { resolveThreadRoute, routingPolicySchema, ThreadRoutePin } from "../src/thread-model-routing";
const diagnostics = (): JevDiagnostics => ({ outcome: "unsupported_input", attempts: [] });
describe("single-attempt foreground Jev", () => {
  it("fails immediately on a transient error without retrying or exposing error text", async () => {
    const run = vi.fn().mockRejectedValueOnce(Object.assign(new Error("secret body"), { status: 503 })).mockResolvedValueOnce({ answers: {} });
    const trace = diagnostics();
    await expect(runJev({run}, {}, trace)).rejects.toThrow();
    expect(trace.outcome).toBe("unavailable");
    expect(trace.attempts.map(a => a.outcome)).toEqual(["unavailable"]);
    expect(run).toHaveBeenCalledOnce();
    expect(JSON.stringify(trace)).not.toContain("secret");
  });
  it.each([401,403,429,400])("does not retry HTTP %s", async status => {
    const run = vi.fn().mockRejectedValue(Object.assign(new Error("private"),{status}));
    await expect(runJev({run},{},diagnostics())).rejects.toThrow(); expect(run).toHaveBeenCalledTimes(1);
  });
  it("does not retry after its request was cancelled", async () => {
    const controller=new AbortController();
    const run=vi.fn(async()=>{controller.abort();throw Object.assign(new Error("unavailable"),{status:503});});
    await expect(runJev({run},{},diagnostics(),1000,controller.signal)).rejects.toThrow();
    expect(run).toHaveBeenCalledOnce();
  });
  it("does not retry repeated transient failures", async () => {
    const run = vi.fn().mockRejectedValue(Object.assign(new Error("private"),{status:502}));
    const trace=diagnostics(); await expect(runJev({run},{},trace)).rejects.toThrow();
    expect(run).toHaveBeenCalledTimes(1); expect(trace.attempts).toHaveLength(1);
  });
  it("does not duplicate a hanging binding and uses a shared deadline", async () => {
    vi.useFakeTimers();
    try {
      const run = vi.fn(() => new Promise(() => {})); const trace=diagnostics();
      const pending=runJev({run},{},trace); const result=expect(pending).rejects.toThrow("deadline");
      await vi.advanceTimersByTimeAsync(JEV_ROUTING_BUDGET_MS); await result;
      expect(run).toHaveBeenCalledTimes(1); expect(trace.outcome).toBe("timeout");
    } finally { vi.useRealTimers(); }
  });
  it("skips fixed-provider classification without inventing probabilities", async () => {
    const run=vi.fn(); const observed=vi.fn();
    const route=await resolveThreadRoute({run},"hello",routingPolicySchema.parse({candidates:["openrouter:openai/gpt-6-luna:low"]}),
      {openrouter:true,vercel:false,bypassSingleCandidate:true,observeRoute:observed});
    expect(run).not.toHaveBeenCalled(); expect(route.classifier).toEqual({outcome:"not_requested",attempts:[]});
    expect(route.audit).toBeUndefined(); expect(observed).toHaveBeenCalledOnce();
  });
  it("records invalid output separately without retrying it", async () => {
    const run=vi.fn().mockResolvedValue({state:"Completed",result:{secret:"untrusted"}});
    const route=await resolveThreadRoute({run},"hello",routingPolicySchema.parse({}),{openrouter:false,vercel:false});
    expect(route.classifier?.outcome).toBe("invalid_result"); expect(run).toHaveBeenCalledOnce();
    expect(JSON.stringify(route)).not.toContain("untrusted");
  });
});


describe("routing cancellation", () => {
  it("does not invoke Jev or record an attempt for a pre-aborted signal", async () => {
    const controller = new AbortController();
    const reason = new Error("cancelled request");
    controller.abort(reason);
    const run = vi.fn(), trace = diagnostics();
    await expect(runJev({ run }, {}, trace, JEV_ROUTING_BUDGET_MS, controller.signal)).rejects.toBe(reason);
    expect(run).not.toHaveBeenCalled();
    expect(trace).toEqual(diagnostics());
  });
  it.each(["direct", "legacy"] as const)("refuses pre-aborted %s routing before classification or observation", async strategy => {
    const controller = new AbortController(); controller.abort();
    const run = vi.fn(), observeRoute = vi.fn();
    await expect(resolveThreadRoute({ run }, "task", routingPolicySchema.parse({ strategy }),
      { openrouter: false, vercel: false, signal: controller.signal, observeRoute })).rejects.toBe(controller.signal.reason);
    expect(run).not.toHaveBeenCalled(); expect(observeRoute).not.toHaveBeenCalled();
  });
  it.each(["direct", "legacy"] as const)("cancels %s immediately without observing or pinning a late answer", async strategy => {
    vi.useFakeTimers();
    try {
      const controller = new AbortController(), reason = new Error("cancelled request");
      const removeListener = vi.spyOn(controller.signal, "removeEventListener");
      let release!: (value: unknown) => void;
      const run = vi.fn(() => new Promise(resolve => { release = resolve; }));
      const observeRoute = vi.fn(), commit = vi.fn();
      const pin = new ThreadRoutePin({ read: () => undefined, commit });
      const pending = pin.resolve(() => resolveThreadRoute({ run }, "task", routingPolicySchema.parse({ strategy }),
        { openrouter: false, vercel: false, signal: controller.signal, observeRoute }));
      const rejected = expect(pending).rejects.toBe(reason);
      await Promise.resolve();
      expect(run).toHaveBeenCalledOnce();
      controller.abort(reason);
      // No timer advancement: cancellation must settle without waiting for the deadline.
      await rejected;
      expect(vi.getTimerCount()).toBe(0);
      expect(removeListener).toHaveBeenCalledWith("abort", expect.any(Function));
      release({ answers: { candidate: { choice: "gpt-6-luna:low", confidence: .99 },
        family: { choice: "terminal", confidence: .99 } } });
      await Promise.resolve();
      expect(observeRoute).not.toHaveBeenCalled(); expect(commit).not.toHaveBeenCalled();
    } finally { vi.useRealTimers(); }
  });
  it("does not pin a route if cancelled during observation", async () => {
    const controller = new AbortController(), commit = vi.fn();
    const pin = new ThreadRoutePin({ read: () => undefined, commit });
    const pending = pin.resolve(() => resolveThreadRoute({ run: vi.fn() }, "task",
      routingPolicySchema.parse({ candidates: ["gpt-6-luna:low"] }),
      { openrouter: false, vercel: false, signal: controller.signal, bypassSingleCandidate: true,
        observeRoute: () => { controller.abort(); } }));
    await expect(pending).rejects.toMatchObject({ name: "AbortError" });
    expect(commit).not.toHaveBeenCalled();
  });
  it.each(["direct", "legacy"] as const)("uses the same two-second single-attempt deadline for %s", async strategy => {
    vi.useFakeTimers();
    try {
      expect(JEV_ROUTING_BUDGET_MS).toBe(2000);
      const run = vi.fn(() => new Promise(() => {}));
      const finished = vi.fn();
      const pending = resolveThreadRoute({ run }, "task", routingPolicySchema.parse({ strategy })).then(route => {
        finished(); return route;
      });
      await vi.advanceTimersByTimeAsync(1999);
      expect(finished).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(1);
      expect((await pending).selection).toBe("fallback");
      expect(run).toHaveBeenCalledOnce();
      expect(vi.getTimerCount()).toBe(0);
    } finally { vi.useRealTimers(); }
  });
});
