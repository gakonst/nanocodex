import { routeManaged, type ManagedProxyEnv } from "../../account/worker/managedProxy";
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
    authorizationEpoch: 1, capabilities: ["agents:read", "agents:write", "tools:use"],
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
  it.each(["key", "organization", "membership", "team", "epoch", "role", "capabilities"])(
    "direct live creation observes %s revocation without cached access or a second create", async change => {
      await withKey(async (key, f) => {
        let creates = 0, fallback = 0;
        const runtime: ManagedProxyEnv = {
          NANOCODEX_BACKEND: { fetch: async () => { fallback++; throw Error("unexpected managed fallback"); } } as unknown as Fetcher,
          NANOCODEX_LIVE_API_KEYS: { getByName: name => {
            expect(name).toBe(f.record.digest);
            return { resolveAuthorizedKey: () => key.resolveAuthorizedKey() };
          } },
          NANOCODEX_LIVE_SESSIONS: { getByName: () => ({ fetch: async internal => {
            creates++;
            expect(new URL(internal.url).pathname).toBe("/create-live");
            expect(internal.headers.get("x-nanocodex-owner-id")).toBe(f.record.userId);
            return new Response(null, { status: 200 });
          } }) },
        };
        const create = () => {
          const req = new Request("https://test.example/v1/agents/live", { headers: {
            authorization: `Bearer ${token}`, upgrade: "websocket", "x-nanocodex-access": "not-authority",
          } });
          return routeManaged(req, runtime, new URL(req.url));
        };
        expect((await create())?.status).toBe(200);
        if (change === "key") await key.fetch(new Request("https://key/record", { method: "DELETE" }));
        if (change === "organization") f.account.organizationId = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        if (change === "membership") f.organizations.mockImplementation(async () => new Response(null, { status: 404 }));
        if (change === "team") f.grant.teamId = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        if (change === "epoch") f.grant.authorizationEpoch++;
        if (change === "role") f.grant.role = "reader";
        if (change === "capabilities") f.grant.capabilities = ["agents:read"];
        expect((await create())?.status).toBe(401);
        expect(creates).toBe(1); expect(fallback).toBe(0);
      });
    },
  );
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
