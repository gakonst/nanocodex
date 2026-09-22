import { describe, expect, it, vi } from "vitest";
import { gatewayAvailability, gatewayRuntime } from "../src/gateway-runtime";
import type { ThreadRoute } from "../src/thread-model-routing";

const route = (backend: "openrouter" | "vercel") => ({ backend, model: "gpt-6-astra", thinking: "medium" }) as ThreadRoute;
describe("deployment-owned gateway credentials", () => {
  it("exposes availability booleans without exposing secrets", () => {
    expect(gatewayAvailability({OPENROUTER_API_KEY:"fixture",AI_GATEWAY_API_KEY:" "})).toEqual({openrouter:true,vercel:false});
    expect(gatewayAvailability({})).toEqual({openrouter:false,vercel:false});
  });
  it.each(["openrouter", "vercel"] as const)("does not replace a pinned %s route when credentials disappear", backend => {
    expect(() => gatewayRuntime({}, route(backend), () => {})).toThrow("configured Worker secret");
  });
  it("checks ownership before every outgoing inference request", async () => {
    const check=vi.fn(), send=vi.fn(async()=>new Response("{}"));
    const runtime=gatewayRuntime({OPENROUTER_API_KEY:"synthetic-test-key"},route("openrouter"),check,send as typeof fetch)!;
    expect(runtime).toMatchObject({provider:"openrouter",model:"gpt-6-astra",reasoningEffort:"medium"});
    await runtime.fetch("https://openrouter.ai/api/v1/chat/completions");
    check.mockImplementation(()=>{throw Error("revoked");});
    expect(()=>runtime.fetch("https://openrouter.ai/api/v1/chat/completions")).toThrow("revoked");
    expect(send).toHaveBeenCalledTimes(1);
  });
  it("does not create a gateway transport for non-gateway threads", () => {
    expect(gatewayRuntime({},undefined,()=>{})).toBeUndefined();
    expect(gatewayRuntime({}, {backend:"chatgpt"} as ThreadRoute,()=>{})).toBeUndefined();
  });
});
