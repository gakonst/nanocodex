import { createExecutionContext } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { handleEgress, SessionModelEgress, type EgressEnv } from "../src/egress";
import { TRUSTED_INGRESS_HEADER } from "nanocodex/cloudflare/durable-placement";
const owner = "11111111-1111-4111-8111-111111111111";
describe("broker first-use placement", () => {
  it.each([undefined, "SJC", "LHR", "ZZZ"])("uses private control ingress with unchanged owner keys (%s)", async colo => {
    const credentials = vi.fn(() => ({ fetch: async () => new Response(null, { status: 204 }) }));
    const connectors = vi.fn(() => ({ fetch: async () => new Response(null, { status: 204 }) }));
    const env = { USER_CREDENTIALS: { getByName: credentials }, USER_CONNECTORS: { getByName: connectors } } as unknown as EgressEnv;
    for (const path of ["wallet", "credentials", "catalog", "connectors"]) {
      const request = new Request(`https://broker.internal/users/${owner}/${path}`, { headers: colo ? { [TRUSTED_INGRESS_HEADER]: colo } : {} });
      expect((await handleEgress(request, env)).status).toBe(204);
    }
    const options = colo === "SJC" ? { locationHint: "wnam" } : colo === "LHR" ? { locationHint: "weur" } : undefined;
    expect(credentials).toHaveBeenCalledWith(owner, options);
    expect(connectors).toHaveBeenCalledWith(owner, options);
    expect(env).not.toHaveProperty("trustedClientIngressColo");
  });
  it("uses only the private Session model region for credential first touch", async () => {
    const credentials = vi.fn(() => ({ resolveModelCredential: async () => ({ status: 401, error: "credential_not_found" }) }));
    const env = { USER_CREDENTIALS: { getByName: credentials },
      MANAGED_AGENT_OWNERSHIP: { fetch: async () => Response.json({ user_id: owner }) } } as unknown as EgressEnv;
    const headers = { "x-nanocodex-session-model-owner": owner, "x-nanocodex-subject": `managed-session-v1_${"a".repeat(64)}`,
      "x-nanocodex-model-region": "wnam", [TRUSTED_INGRESS_HEADER]: "NRT",
      authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", upgrade: "websocket", "openai-beta": "responses_websockets=2026-02-06" };
    await new SessionModelEgress(createExecutionContext(), env).fetch(new Request("https://nanocodex.internal/v1/responses", { headers }));
    expect(credentials).toHaveBeenCalledWith(owner, { locationHint: "wnam" });
    expect(env).not.toHaveProperty("trustedPlacementRegion");
    credentials.mockClear();
    const generic = new Headers(headers); generic.delete("x-nanocodex-session-model-owner");
    // Generic egress cannot treat either placement header as Session authority.
    await handleEgress(new Request("https://nanocodex.internal/v1/responses", { headers: generic }), env);
    expect(credentials).toHaveBeenCalledWith(owner, undefined);
  });
});
