import { afterEach, describe, expect, it, vi } from "vitest";
import { handleEgress, type EgressEnv } from "../src/egress";

afterEach(() => vi.restoreAllMocks());

function environment(userSource = false): EgressEnv {
  return {
    AGENT_SUBJECTS: { getByName: () => ({ fetch: async () => Response.json({ user_id: "fixture-user" }) }) },
    USER_CREDENTIALS: { getByName: () => ({ resolveModelCredential: async () => ({ status: 200, credential: {
      kind: "chatgpt", revision: 1, secret: "fixture-provider-secret", accountId: "fixture-account",
      ...(userSource ? { source: "user" } : {}),
    } }) }) },
  } as unknown as EgressEnv;
}
function request(method = "GET"): Request {
  return new Request("https://nanocodex.internal/v1/responses", { method, ...(method === "POST" ? { body: JSON.stringify({ input: [], stream: true }) } : {}), headers: {
    ...(method === "POST" ? { "content-type": "application/json" } : {}),
    authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "openai-beta": "responses_websockets=2026-02-06",
    ...(method === "GET" ? { upgrade: "websocket" } : {}), "x-nanocodex-subject": "a".repeat(64),
  } });
}

describe("model upstream rejection diagnostics", () => {
  for (const method of ["GET", "POST"]) for (const status of [400, 401, 403, 413, 429, 500, 503]) {
    it(`${method} preserves HTTP ${status} and exposes only a recognized error code`, async () => {
      const logs = [vi.spyOn(console, "warn").mockImplementation(() => {}), vi.spyOn(console, "error").mockImplementation(() => {})];
      const response = await handleEgress(request(method), environment(), undefined, (async () => Response.json({
        error: { code: "invalid_request_error", message: "fixture-private-provider-response", secret: "fixture-provider-secret" },
      }, { status, headers: { "retry-after": "15", authorization: "fixture-provider-secret" } })) as typeof fetch);
      expect(response.status).toBe(status);
      expect(await response.json()).toEqual({ error: {
        code: "invalid_request_error", message: `Upstream model request rejected (HTTP ${status}; invalid_request_error).`,
      }, upstream_status: status });
      expect(response.headers.get("retry-after")).toBe("15");
      expect(response.headers.has("authorization")).toBe(false);
      const recorded = JSON.stringify(logs.flatMap(log => log.mock.calls));
      expect(recorded).not.toContain("fixture-private");
      expect(recorded).not.toContain("fixture-provider-secret");
      expect(recorded).toContain('"upstream_status":' + status);
    });
  }
  for (const body of ["<html>private edge response</html>", "x".repeat(65537), JSON.stringify({ error: { code: "secret_value", message: "private" } })]) {
    it(`retains status for unrecognized or oversized bodies (${body.length} bytes)`, async () => {
      vi.spyOn(console, "warn").mockImplementation(() => {});
      const response = await handleEgress(request(), environment(), undefined,
        (async () => new Response(body, { status: 400, headers: { "retry-after": "private" } })) as typeof fetch);
      expect(response.status).toBe(400);
      expect(await response.json()).toMatchObject({ error: { code: "upstream_rejected" }, upstream_status: 400 });
      expect(response.headers.has("retry-after")).toBe(false);
    });
  }
  it("preserves structured image recovery selectors without reflecting the message", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const response = await handleEgress(request("POST"), environment(), undefined, (async () => Response.json({
      error: { type: "invalid_request_error", code: "invalid_value", param: "input[188].output[2].image_url", message: "private input" },
    }, { status: 400 })) as typeof fetch);
    expect(await response.json()).toEqual({ error: {
      type: "invalid_request_error", code: "invalid_value", param: "input[188].output[2].image_url",
      message: "Upstream model request rejected (HTTP 400; invalid_value).",
    }, upstream_status: 400 });
  });
  it("preserves the fixed legacy image diagnostic without reflecting provider text", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const response = await handleEgress(request("POST"), environment(), undefined, (async () => Response.json({
      error: { type: "invalid_request_error", message: "The image data you provided does not represent a valid image: private input" },
    }, { status: 400 })) as typeof fetch);
    expect(await response.json()).toEqual({ error: {
      code: "invalid_image", type: "invalid_request_error",
      message: "The image data you provided does not represent a valid image",
    }, upstream_status: 400 });
  });
  it("preserves policy stops and strips unknown provider fields", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const response = await handleEgress(request("POST"), environment(), undefined, (async () => Response.json({
      error: { code: "misalignment_policy_violation", type: "private", param: "private", message: "The image data you provided does not represent a valid image: private input" },
    }, { status: 400 })) as typeof fetch);
    expect(await response.json()).toEqual({ error: {
      code: "misalignment_policy_violation", message: "Upstream model request rejected (HTTP 400; misalignment_policy_violation).",
    }, upstream_status: 400 });
  });
  it("retains the discovered schema selector without echoing schema property names", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const response = await handleEgress(request("POST"), environment(), undefined, (async () => Response.json({
      error: { code: "invalid_function_parameters", param: "input[2].tools[0].tools[1].parameters.properties.private", message: "private" },
    }, { status: 400 })) as typeof fetch);
    expect(await response.json()).toMatchObject({ error: {
      code: "invalid_function_parameters", param: "input[2].tools[0].tools[1].parameters",
    } });
  });
  it("retains a rate limit code after the failover check consumes its body", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    const upstream = vi.fn(async () => Response.json({ error: { code: "rate_limit_exceeded" } }, { status: 429 }));
    const response = await handleEgress(request(), environment(true), undefined, upstream as typeof fetch);
    expect(response.status).toBe(429);
    expect(await response.json()).toMatchObject({ error: { code: "rate_limit_exceeded" } });
    expect(upstream).toHaveBeenCalledTimes(1);
  });
});
