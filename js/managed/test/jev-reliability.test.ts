import { describe, expect, it, vi } from "vitest";
import { runJev, type JevDiagnostics } from "../src/jev-reliability";
import { resolveThreadRoute, routingPolicySchema } from "../src/thread-model-routing";
const diagnostics = (): JevDiagnostics => ({ outcome: "unsupported_input", attempts: [] });
describe("bounded Jev recovery", () => {
  it("recovers once from a 503 and preserves the failed attempt", async () => {
    const run = vi.fn().mockRejectedValueOnce(Object.assign(new Error("secret body"), { status: 503 })).mockResolvedValueOnce({ answers: {} });
    const trace = diagnostics();
    expect(await runJev({run}, {}, trace)).toEqual({ answers: {} });
    expect(trace.outcome).toBe("success");
    expect(trace.attempts.map(a => a.outcome)).toEqual(["unavailable", "success"]);
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
  it("never exceeds two transient attempts", async () => {
    const run = vi.fn().mockRejectedValue(Object.assign(new Error("private"),{status:502}));
    const trace=diagnostics(); await expect(runJev({run},{},trace)).rejects.toThrow();
    expect(run).toHaveBeenCalledTimes(2); expect(trace.attempts).toHaveLength(2);
  });
  it("does not duplicate a hanging binding and uses a shared deadline", async () => {
    vi.useFakeTimers();
    try {
      const run = vi.fn(() => new Promise(() => {})); const trace=diagnostics();
      const pending=runJev({run},{},trace,1000); const result=expect(pending).rejects.toThrow("deadline");
      await vi.advanceTimersByTimeAsync(1000); await result;
      expect(run).toHaveBeenCalledTimes(1); expect(trace.outcome).toBe("timeout");
    } finally { vi.useRealTimers(); }
  });
  it("skips fixed-provider classification without inventing probabilities", async () => {
    const run=vi.fn(); const observed=vi.fn();
    const route=await resolveThreadRoute({run},"hello",routingPolicySchema.parse({candidates:["openrouter:openai/gpt-5.6-luna:low"]}),
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
