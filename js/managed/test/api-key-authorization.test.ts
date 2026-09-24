import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { ApiKeyRecord, authenticate, type AccountAuthEnv } from "../src/account-auth";

const token = `ncx_live_${"k".repeat(12)}_${"s".repeat(43)}`;
const request = () => new Request("https://test.example/v1/agents", {
  headers: { authorization: `Bearer ${token}` },
});
async function fixture() {
  const digest = btoa(String.fromCharCode(...new Uint8Array(
    await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token)),
  ))).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  const record = {
    id: "k".repeat(12), prefix: `ncx_live_${"k".repeat(12)}`, digest,
    label: "voice", createdAt: 1, userId: "11111111-1111-4111-8111-111111111111",
    organizationId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    teamId: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", role: "writer",
    authorizationEpoch: 1, capabilities: ["agents:read", "agents:write"],
  };
  const account = {
    id: record.userId, organizationId: record.organizationId,
    persistent: true, createdAt: 1, lastAuthenticatedAt: 1,
  };
  const grant = { ...record, role: "owner" };
  const users = vi.fn(async () => Response.json(account));
  const organizations = vi.fn(async () => Response.json(grant));
  const bindings = {
    NANOCODEX_USERS: { getByName: () => ({ fetch: users }) },
    NANOCODEX_ORGANIZATIONS: { getByName: () => ({ fetch: organizations }) },
  } as unknown as AccountAuthEnv;
  return { record, account, grant, users, organizations, bindings };
}

// Use real DO storage/request handling, with account/grant services controlled
// so revocation between two warm calls is observable without timing assumptions.
async function withKey(run: (key: ApiKeyRecord, f: Awaited<ReturnType<typeof fixture>>) => Promise<void>) {
  const f = await fixture();
  const namespace = (env as unknown as { NANOCODEX_USERS: DurableObjectNamespace }).NANOCODEX_USERS;
  await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_, state) => {
    await state.storage.put("record", f.record);
    await run(new ApiKeyRecord(state, f.bindings), f);
  });
}

describe("live API key authorization beside the key", () => {
  it("uses fresh account and organization RPC results on every key request", async () => {
    const f = await fixture();
    const userRpc = vi.fn(async () => f.account.organizationId);
    const orgRpc = vi.fn(async () => f.grant);
    const bindings = {
      ...f.bindings,
      NANOCODEX_USERS: { getByName: () => ({ authorizationOrganizationId: userRpc,
        fetch: () => { throw new Error("unexpected account HTTP read"); } }) },
      NANOCODEX_ORGANIZATIONS: { getByName: () => ({ authorizationGrant: orgRpc,
        fetch: () => { throw new Error("unexpected grant HTTP read"); } }) },
    } as unknown as AccountAuthEnv;
    const namespace = (env as unknown as { NANOCODEX_USERS: DurableObjectNamespace }).NANOCODEX_USERS;
    await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_, state) => {
      await state.storage.put("record", f.record);
      const key = new ApiKeyRecord(state, bindings);
      expect(await key.resolveAuthorizedKey()).toBeDefined();
      f.grant.authorizationEpoch++;
      expect(await key.resolveAuthorizedKey()).toBeUndefined();
      f.grant.authorizationEpoch--;
      f.account.organizationId = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
      expect(await key.resolveAuthorizedKey()).toBeUndefined();
    });
    expect(userRpc).toHaveBeenCalledTimes(3);
    expect(orgRpc).toHaveBeenCalledTimes(3);
  });
  it("uses one RPC reply and observes key deletion without a streamed response", async () => {
    await withKey(async (key, f) => {
      const rpc = vi.fn(() => key.resolveAuthorizedKey());
      const fetch = vi.fn(() => { throw new Error("unexpected HTTP fallback"); });
      const edge = { ...f.bindings, NANOCODEX_API_KEYS: { getByName: () => ({ resolveAuthorizedKey: rpc, fetch }) } } as unknown as AccountAuthEnv;
      expect(await authenticate(request(), edge)).toMatchObject({ kind: "api_key", capabilities: f.record.capabilities });
      await key.fetch(new Request("https://key/record", { method: "DELETE" }));
      expect(await authenticate(request(), edge)).toBeUndefined();
      expect(rpc).toHaveBeenCalledTimes(2);
      expect(fetch).not.toHaveBeenCalled();
    });
  });
  it("validates once remotely and projects only the stored key's scope", async () => {
    await withKey(async (key, f) => {
      const keys = vi.fn((input: RequestInfo | URL) => key.fetch(new Request(input)));
      const edge = {
        ...f.bindings, NANOCODEX_API_KEYS: { getByName: () => ({ fetch: keys }) },
      } as unknown as AccountAuthEnv;
      expect(await authenticate(request(), edge)).toMatchObject({
        kind: "api_key", role: "writer", capabilities: f.record.capabilities,
        authorizationEpoch: 1,
      });
      expect(keys).toHaveBeenCalledTimes(1);
      expect(f.users).toHaveBeenCalledTimes(1);
      expect(f.organizations).toHaveBeenCalledTimes(1);
      await key.fetch(new Request("https://key/record", { method: "DELETE" }));
      expect(await authenticate(request(), edge)).toBeUndefined();
    });
  });
  it.each(["organization", "membership", "team", "epoch", "role", "capabilities"])(
    "rejects a live %s change on the next call", async (change) => {
      await withKey(async (key, f) => {
        const resolve = () => key.fetch(new Request("https://key/resolve?authorize=1"));
        expect((await resolve()).status).toBe(200);
        expect(await key.resolveAuthorizedKey()).toBeDefined();
        if (change === "organization") f.account.organizationId = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        if (change === "membership") f.organizations.mockImplementation(async () => new Response(null, { status: 404 }));
        if (change === "team") f.grant.teamId = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        if (change === "epoch") f.grant.authorizationEpoch++;
        if (change === "role") f.grant.role = "reader";
        if (change === "capabilities") f.grant.capabilities = ["agents:read"];
        const denied = await resolve();
        expect(denied.status).toBe(401);
        expect(await key.resolveAuthorizedKey()).toBeUndefined();
        expect(denied.headers.has("x-nanocodex-api-key-authorized")).toBe(false);
      });
    },
  );
  it("keeps live validation when an older key object omits the marker", async () => {
    const f = await fixture();
    const edge = {
      ...f.bindings,
      NANOCODEX_API_KEYS: { getByName: () => ({ fetch: async () => Response.json(f.record) }) },
    } as unknown as AccountAuthEnv;
    expect(await authenticate(request(), edge)).toBeDefined();
    f.grant.authorizationEpoch++;
    expect(await authenticate(request(), edge)).toBeUndefined();
    expect(f.users).toHaveBeenCalledTimes(2);
    expect(f.organizations).toHaveBeenCalledTimes(2);
  });
  it("does not trust an authorization marker supplied by the client", async () => {
    const f = await fixture();
    f.grant.authorizationEpoch++;
    const edge = {
      ...f.bindings,
      NANOCODEX_API_KEYS: { getByName: () => ({ fetch: async () => Response.json(f.record) }) },
    } as unknown as AccountAuthEnv;
    const forged = request();
    forged.headers.set("x-nanocodex-api-key-authorized", "1");
    expect(await authenticate(forged, edge)).toBeUndefined();
  });
});
