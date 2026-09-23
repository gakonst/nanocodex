import { createExecutionContext } from "cloudflare:test";
import worker, { type Env } from "../src/index";
import { describe, expect, it, vi } from "vitest";
import { durablePlacementOptions, placementHeaders, placementRegion, TRUSTED_INGRESS_HEADER, withIngressPlacement } from "nanocodex/cloudflare/durable-placement";
import { ensureAccount, UserAccount, type AccountAuthEnv } from "../src/account-auth";

const userId = "11111111-1111-4111-8111-111111111111";
describe("trusted first-use Durable Object placement", () => {
  it.each([["SJC", "wnam"], ["IAD", "enam"], ["LHR", "weur"], ["ATH", "eeur"], ["SIN", "apac"], ["SYD", "oc"], ["GRU", "sam"]])("preserves regional selection for %s", (colo, region) => {
    expect(placementRegion(colo)).toBe(region);
    expect(durablePlacementOptions(colo)).toEqual({ locationHint: region });
  });
  it.each([null, undefined, "ZZZ", "sjc", "SJC,MXP", "wnam", {}, 3])("falls back without inventing placement for %s", colo => {
    expect(durablePlacementOptions(colo)).toBeUndefined();
  });
  it("replaces spoofed hints and scopes independent concurrent requests without mutating the binding", async () => {
    const received: Request[] = [];
    const binding = { fetch: vi.fn(async (request: Request) => { received.push(request); return new Response(null, { status: 204 }); }) } as unknown as Fetcher;
    const original = { NANOCODEX: binding };
    const west = withIngressPlacement(original, "SJC"), europe = withIngressPlacement(original, "LHR"), unknown = withIngressPlacement(original, null);
    const input = new Request("https://broker.internal/users/owner/wallet", { method: "PUT", headers: { [TRUSTED_INGRESS_HEADER]: "NRT", "x-ordinary": "retained" }, body: "fixture" });
    await Promise.all([west, europe, unknown].map(env => env.NANOCODEX.fetch(input.clone())));
    expect(received.map(request => request.headers.get(TRUSTED_INGRESS_HEADER))).toEqual(["SJC", "LHR", null]);
    expect(await Promise.all(received.map(request => request.text()))).toEqual(["fixture", "fixture", "fixture"]);
    expect(received.every(request => request.method === "PUT" && request.headers.get("x-ordinary") === "retained")).toBe(true);
    expect(input.headers.get(TRUSTED_INGRESS_HEADER)).toBe("NRT");
    expect(original.NANOCODEX).toBe(binding);
    expect(original).not.toHaveProperty("trustedClientIngressColo");
    expect(placementHeaders({ [TRUSTED_INGRESS_HEADER]: "NRT" }, undefined).has(TRUSTED_INGRESS_HEADER)).toBe(false);
  });
  it("keeps the account key and passes ingress across the nested organization first touch", async () => {
    const values = new Map<string, unknown>();
    const organization = vi.fn(() => ({ fetch: vi.fn(async () => new Response(null, { status: 204 })) }));
    const account = { ctx: { storage: { get: async (key: string) => values.get(key), put: async (key: string, value: unknown) => { values.set(key, value); } } },
      env: { NANOCODEX_ORGANIZATIONS: { getByName: organization } } };
    const accounts = vi.fn(() => ({ fetch: (input: RequestInfo | URL, init?: RequestInit) => UserAccount.prototype.fetch.call(account as unknown as UserAccount, new Request(input, init)) }));
    const env = { NANOCODEX_USERS: { getByName: accounts }, trustedClientIngressColo: "SJC" } as unknown as AccountAuthEnv;
    await ensureAccount(env, userId, true);
    const record = values.get("account") as { id: string; organizationId: string };
    expect(accounts).toHaveBeenCalledWith(userId, { locationHint: "wnam" });
    expect(organization).toHaveBeenCalledWith(record.organizationId, { locationHint: "wnam" });
    // Retrying from another region retains both account and organization identities.
    await ensureAccount({ ...env, trustedClientIngressColo: "LHR" }, userId, true);
    expect(accounts).toHaveBeenLastCalledWith(userId, { locationHint: "weur" });
    expect(organization).toHaveBeenLastCalledWith(record.organizationId, { locationHint: "weur" });
    expect((values.get("account") as typeof record).organizationId).toBe(record.organizationId);
  });
});


it.each([undefined, "ZZZ", "SJC"])("ignores public placement headers at managed ingress (platform=%s)", async colo => {
  const token = `ncx_live_${"a".repeat(12)}_${"b".repeat(43)}`;
  const digest = btoa(String.fromCharCode(...new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token)))))
    .replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  const record = { id: "a".repeat(12), label: "fixture", prefix: `ncx_live_${"a".repeat(12)}`, createdAt: 1, digest, userId,
    organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    role: "owner", authorizationEpoch: 1, capabilities: ["agents:read", "agents:write", "tools:use"] };
  const placements = vi.fn(() => ({ resolveAuthorizedKey: async () => record }));
  const received: Request[] = [];
  const env = { NANOCODEX_API_KEYS: { getByName: placements }, NANOCODEX: { fetch: async (request: Request) => {
    received.push(request); return new Response(null, { status: 409 });
  } } } as unknown as Env;
  const request = new Request("https://fixture.invalid/v1/credentials", { headers: {
    authorization: `Bearer ${token}`, [TRUSTED_INGRESS_HEADER]: "NRT", "x-nanocodex-client-ingress-colo": "NRT",
    "x-nanocodex-model-region": "apac",
  }, ...(colo ? { cf: { colo } } : {}) });
  expect((await worker.fetch(request, env, createExecutionContext())).status).toBe(409);
  expect(placements).toHaveBeenCalledWith(digest, colo === "SJC" ? { locationHint: "wnam" } : undefined);
  expect(received).toHaveLength(1);
  expect(received[0]!.headers.get(TRUSTED_INGRESS_HEADER)).toBe(colo ?? null);
  expect(env).not.toHaveProperty("trustedClientIngressColo");
  const sessionRequests: Request[] = [];
  const sessions = vi.fn(() => ({ fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
    sessionRequests.push(new Request(input, init)); return Response.json({});
  } }));
  const sessionEnv = { ...env, NANOCODEX_SESSIONS: { idFromName: () => ({ toString: () => "a".repeat(64) }), getByName: sessions } } as unknown as Env;
  const create = new Request("https://fixture.invalid/v1/agents", { method: "POST", headers: request.headers, body: "", ...(colo ? { cf: { colo } } : {}) });
  const created = await worker.fetch(create, sessionEnv, createExecutionContext());
  expect(created.status, await created.text()).toBe(201);
  expect(sessions).toHaveBeenCalledWith(expect.stringMatching(/^[0-9a-f-]{36}$/), colo === "SJC" ? { locationHint: "wnam" } : undefined);
  expect(sessionRequests[0]!.headers.get("x-nanocodex-client-ingress-colo")).toBe(colo ?? null);
});
