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

describe("live gateway telemetry integration", () => {
  it("records status/body completion against the pinned catalog route", async () => {
    const { createGatewayResponses } = await import("nanocodex/cloudflare/gateway-responses");
    const observations: unknown[] = [];
    const runtime = gatewayRuntime({ OPENROUTER_API_KEY: "private-key" }, route("openrouter"), () => {},
      (async () => Response.json({ choices: [{ message: { content: "private-response" }, finish_reason: "stop" }] })) as typeof fetch,
      { workerColo: null, clientIngressColo: "LHR", store: { append: sample => { observations.push(sample); } } })!;
    const transport = createGatewayResponses(runtime);
    await transport.createResponse(`${transport.apiBaseUrl}/responses`, "private-session", {
      authorization: "host_managed", signal: new AbortController().signal, body: JSON.stringify({ input: "private-prompt" }),
    });
    expect(observations).toHaveLength(1);
    expect(observations[0]).toMatchObject({ source: "live", backend: "openrouter", model: "gpt-6-astra", effort: "medium",
      workerColo: null, clientIngressColo: "LHR", outcome: "success", status: 200, generationTtftMs: null, clientDeliveryMs: null,
      headersMs: expect.any(Number), fullResponseMs: expect.any(Number) });
    expect(JSON.stringify(observations)).not.toContain("private");
  });
  it("retains protocol failures as censored samples", async () => {
    const { createGatewayResponses } = await import("nanocodex/cloudflare/gateway-responses");
    const observations: unknown[] = [];
    const runtime = gatewayRuntime({ AI_GATEWAY_API_KEY: "private-key" }, route("vercel"), () => {},
      (async () => Response.json({ error: { message: "private-error" } })) as typeof fetch,
      { workerColo: null, clientIngressColo: null, store: { append: sample => { observations.push(sample); } } })!;
    const transport = createGatewayResponses(runtime);
    await expect(transport.createResponse(`${transport.apiBaseUrl}/responses`, "s", { authorization: "host_managed", signal: new AbortController().signal, body: "{}" })).rejects.toThrow("Gateway Responses");
    expect(observations[0]).toMatchObject({ outcome: "protocol_error", status: 200, fullResponseMs: null });
    expect(JSON.stringify(observations)).not.toContain("private");
  });
});
