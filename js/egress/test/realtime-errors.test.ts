import { afterEach, describe, expect, it, vi } from "vitest";

import { handleEgress, type EgressEnv } from "../src/egress";

afterEach(() => vi.restoreAllMocks());

describe("Realtime upstream failure diagnostics", () => {
  for (const status of [403, 429, 500]) {
    it(`retains upstream HTTP ${status} without logging provider response content or credentials`, async () => {
      const log = vi.spyOn(console, "error").mockImplementation(() => {});
      const env = {
        AGENT_SUBJECTS: { getByName: () => ({ fetch: async () => Response.json({ user_id: "fixture-user" }) }) },
        USER_CREDENTIALS: { getByName: () => ({ fetch: async () => Response.json({
          kind: "chatgpt", revision: 1, secret: "fixture-provider-secret", accountId: "fixture-account",
        }) }) },
      } as unknown as EgressEnv;
      const request = new Request("https://nanocodex.internal/v1/realtime/calls", {
        method: "POST",
        headers: {
          authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL",
          "x-nanocodex-subject": "a".repeat(64),
          "content-type": "application/json",
          "openai-alpha": "quicksilver=v2",
          "session-id": "fixture-session",
          "thread-id": "fixture-session",
          "x-session-id": "fixture-session",
        },
        body: '{"sdp":"fixture-private-SDP"}',
      });
      const response = await handleEgress(request, env, undefined, (async () => new Response(
        "fixture-private-provider-response", { status },
      )) as typeof fetch);
      expect(response.status).toBe(status === 429 ? 503 : 502);
      expect(await response.json()).toEqual({ error: "upstream_rejected" });
      expect(log).toHaveBeenCalledTimes(1);
      expect(log.mock.calls[0]?.[0]).toEqual({
        type: "egress.request", action: "error", rule: "realtime-call", method: "POST",
        host: "nanocodex.internal", path: "/v1/realtime/calls", duration_ms: expect.any(Number),
        code: "upstream_rejected", status: status === 429 ? 503 : 502, upstream_status: status,
      });
    });
  }
});
