import { routeManaged, type ManagedProxyEnv } from "../../account/worker/managedProxy";
import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { ApiKeyRecord, authenticate, type AccountAuthEnv, type Organization, type UserAccount } from "../src/account-auth";

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

// Exercise real workerd RPC dispatch for both authority reads. The key still
// uses the existing real-storage harness because this test Worker has no key binding.
async function rpcAuthorities(f: Awaited<ReturnType<typeof fixture>>) {
  const runtime = env as unknown as AccountAuthEnv;
  const user = runtime.NANOCODEX_USERS.getByName(crypto.randomUUID());
  const organization = runtime.NANOCODEX_ORGANIZATIONS.getByName(crypto.randomUUID());
  const metadata = {
    id: f.record.organizationId, name: null, rootTeamId: f.record.teamId,
    authorizationEpoch: 1, createdAt: 1, updatedAt: 1,
  };
  const membership = {
    userId: f.record.userId, organizationId: f.record.organizationId,
    teamId: f.record.teamId, role: "writer", capabilities: f.record.capabilities, createdAt: 1,
  };
  const team = {
    id: f.record.teamId, organizationId: f.record.organizationId,
    parentTeamId: null, name: null, createdAt: 1, updatedAt: 1,
  };
  await runInDurableObject(user, async (_, state) => {
    await state.storage.put("account", f.account);
  });
  await runInDurableObject(organization, async (_, state) => {
    await state.storage.put({
      metadata,
      [`membership:user:${f.record.userId}`]: membership,
      [`team:${f.record.teamId}`]: team,
    });
  });
  const readAccount = vi.fn<UserAccount["readAccount"]>(async () => await user.readAccount());
  const resolveOrganizationGrant = vi.fn<Organization["resolveOrganizationGrant"]>(async userId => await organization.resolveOrganizationGrant(userId));
  const fetch = vi.fn(async () => { throw new Error("unexpected authority HTTP fallback"); });
  f.bindings.NANOCODEX_USERS = { getByName: (name: string) => {
    expect(name).toBe(f.record.userId);
    return { readAccount, fetch };
  } } as unknown as AccountAuthEnv["NANOCODEX_USERS"];
  f.bindings.NANOCODEX_ORGANIZATIONS = { getByName: (name: string) => {
    expect(name).toBe(f.record.organizationId);
    return { resolveOrganizationGrant, fetch };
  } } as unknown as AccountAuthEnv["NANOCODEX_ORGANIZATIONS"];
  return { user, organization, metadata, membership, team, readAccount, resolveOrganizationGrant, fetch };
}

describe("live account and organization RPC authorization", () => {
  it.each([
    "key", "account", "account subject", "account organization", "metadata",
    "membership", "membership subject", "membership organization", "team",
    "team organization", "epoch", "role", "capabilities",
  ])("observes %s revocation through real DO RPC on the next request", async change => {
    await withKey(async (key, f) => {
      const a = await rpcAuthorities(f);
      const edge = {
        ...f.bindings,
        NANOCODEX_API_KEYS: { getByName: () => ({ resolveAuthorizedKey: () => key.resolveAuthorizedKey(), fetch: a.fetch }) },
      } as unknown as AccountAuthEnv;
      expect(await authenticate(request(), edge)).toMatchObject({
        kind: "api_key", role: "writer", capabilities: f.record.capabilities, authorizationEpoch: 1,
      });
      const other = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
      if (change === "key") await key.fetch(new Request("https://key/record", { method: "DELETE" }));
      if (change.startsWith("account")) await runInDurableObject(a.user, async (_, state) => {
        if (change === "account") await state.storage.delete("account");
        else await state.storage.put("account", {
          ...f.account,
          ...(change === "account subject" ? { id: other } : { organizationId: other }),
        });
      });
      await runInDurableObject(a.organization, async (_, state) => {
        if (change === "metadata") await state.storage.delete("metadata");
        if (change === "epoch") await state.storage.put("metadata", { ...a.metadata, authorizationEpoch: 2 });
        if (change === "membership") await state.storage.delete(`membership:user:${f.record.userId}`);
        if (["membership subject", "membership organization", "role", "capabilities"].includes(change)) {
          await state.storage.put(`membership:user:${f.record.userId}`, {
            ...a.membership,
            ...(change === "membership subject" ? { userId: other } : {}),
            ...(change === "membership organization" ? { organizationId: other } : {}),
            ...(change === "role" ? { role: "reader" } : {}),
            ...(change === "capabilities" ? { capabilities: ["agents:read"] } : {}),
          });
        }
        if (change === "team") await state.storage.delete(`team:${f.record.teamId}`);
        if (change === "team organization") await state.storage.put(`team:${f.record.teamId}`, { ...a.team, organizationId: other });
      });
      expect(await authenticate(request(), edge)).toBeUndefined();
      expect(a.readAccount).toHaveBeenCalledTimes(change === "key" ? 1 : 2);
      expect(a.resolveOrganizationGrant).toHaveBeenCalledTimes(change === "key" ? 1 : 2);
      expect(a.resolveOrganizationGrant).toHaveBeenCalledWith(f.record.userId);
      expect(a.fetch).not.toHaveBeenCalled();
    });
  });

  it("shares live storage semantics with the HTTP routes and limits owner grants to the key scope", async () => {
    await withKey(async (key, f) => {
      const a = await rpcAuthorities(f);
      expect(await a.user.readAccount()).toEqual(await (await a.user.fetch("https://user/account")).json());
      await runInDurableObject(a.organization, async (_, state) => {
        await state.storage.put(`membership:user:${f.record.userId}`, { ...a.membership, role: "owner", capabilities: [] });
      });
      const grant = await a.organization.resolveOrganizationGrant(f.record.userId);
      expect(grant?.capabilities).toContain("api_keys:write");
      for (const method of ["GET", "POST"]) {
        const response = await a.organization.fetch(`https://organization/resolve?userId=${f.record.userId}`, {
          method,
          ...(method === "POST" ? { body: JSON.stringify({ userId: f.record.userId }) } : {}),
        });
        expect(response.status).toBe(200);
        expect(await response.json()).toEqual(grant);
      }
      expect(await key.resolveAuthorizedKey()).toEqual(f.record);
      expect(await a.organization.resolveOrganizationGrant("invalid-subject")).toBeUndefined();
      expect((await a.organization.fetch("https://organization/resolve?userId=invalid-subject")).status).toBe(400);
      const absent = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
      expect(await a.organization.resolveOrganizationGrant(absent)).toBeUndefined();
      expect((await a.organization.fetch(`https://organization/resolve?userId=${absent}`)).status).toBe(404);
      await runInDurableObject(a.user, async (_, state) => { await state.storage.delete("account"); });
      expect(await a.user.readAccount()).toBeUndefined();
      expect((await a.user.fetch("https://user/account")).status).toBe(404);
      expect(a.fetch).not.toHaveBeenCalled();
    });
  });

  it("starts the organization RPC while the account RPC is pending", async () => {
    await withKey(async (key, f) => {
      const a = await rpcAuthorities(f);
      let releaseAccount!: (account: typeof f.account) => void;
      const pendingAccount = new Promise<typeof f.account>(resolve => { releaseAccount = resolve; });
      let organizationStarted!: () => void;
      const started = new Promise<void>(resolve => { organizationStarted = resolve; });
      a.readAccount.mockImplementationOnce(() => pendingAccount);
      a.resolveOrganizationGrant.mockImplementationOnce(async userId => {
        organizationStarted();
        return await a.organization.resolveOrganizationGrant(userId);
      });
      const resolution = key.resolveAuthorizedKey();
      await started;
      expect(a.readAccount).toHaveBeenCalledTimes(1);
      releaseAccount(f.account);
      expect(await resolution).toEqual(f.record);
      expect(a.fetch).not.toHaveBeenCalled();
    });
  });

  it("rejects malformed or misrouted RPC replies without accepting a fetch fallback", async () => {
    await withKey(async (key, f) => {
      const a = await rpcAuthorities(f);
      a.readAccount.mockResolvedValueOnce({ ...f.account, persistent: "true" } as unknown as typeof f.account);
      expect(await key.resolveAuthorizedKey()).toBeUndefined();
      const grant = await a.organization.resolveOrganizationGrant(f.record.userId);
      a.resolveOrganizationGrant.mockResolvedValueOnce({ ...grant!, authorizationEpoch: 0 });
      expect(await key.resolveAuthorizedKey()).toBeUndefined();
      a.resolveOrganizationGrant.mockResolvedValueOnce({ ...grant!, organizationId: "cccccccc-cccc-4ccc-8ccc-cccccccccccc" });
      expect(await key.resolveAuthorizedKey()).toBeUndefined();
      a.resolveOrganizationGrant.mockRejectedValueOnce(new Error("authority unavailable"));
      await expect(key.resolveAuthorizedKey()).rejects.toThrow("authority unavailable");
      expect(a.fetch).not.toHaveBeenCalled();
    });
  });
});

describe("data-only authorization RPC ownership", () => {
  it.each([true, false])("disposes the key reply before projecting or denying it (valid=%s)", async valid => {
    const f = await fixture();
    const dispose = vi.fn();
    const raw = { ...structuredClone(f.record), digest: valid ? f.record.digest : "x".repeat(43),
      [Symbol.dispose]() { raw.capabilities.length = 0; dispose(); } };
    const fetch = vi.fn(async () => { throw new Error("unexpected HTTP fallback"); });
    const bindings = { ...f.bindings,
      NANOCODEX_API_KEYS: { getByName: () => ({ resolveAuthorizedKey: async () => raw, fetch }) },
    } as unknown as AccountAuthEnv;
    const principal = await authenticate(request(), bindings);
    expect(dispose).toHaveBeenCalledOnce();
    if (valid) expect(principal).toMatchObject({ kind: "api_key", capabilities: f.record.capabilities });
    else expect(principal).toBeUndefined();
    expect(fetch).not.toHaveBeenCalled();
  });

  it.each(["authorized", "account", "grant"])("disposes both authority replies before %s validation", async outcome => {
    await withKey(async (key, f) => {
      const accountDispose = vi.fn(), grantDispose = vi.fn();
      const account = { ...f.account, id: outcome === "account" ? "cccccccc-cccc-4ccc-8ccc-cccccccccccc" : f.account.id,
        [Symbol.dispose]() { account.id = "disposed"; accountDispose(); } };
      const grant = { ...structuredClone(f.grant), authorizationEpoch: outcome === "grant" ? 2 : 1,
        [Symbol.dispose]() { grant.capabilities.length = 0; grantDispose(); } };
      const fetch = vi.fn(async () => { throw new Error("unexpected authority HTTP fallback"); });
      f.bindings.NANOCODEX_USERS = { getByName: () => ({ readAccount: async () => account, fetch }) } as unknown as AccountAuthEnv["NANOCODEX_USERS"];
      f.bindings.NANOCODEX_ORGANIZATIONS = { getByName: () => ({ resolveOrganizationGrant: async () => grant, fetch }) } as unknown as AccountAuthEnv["NANOCODEX_ORGANIZATIONS"];
      const result = await key.resolveAuthorizedKey();
      if (outcome === "authorized") expect(result).toEqual(f.record);
      else expect(result).toBeUndefined();
      expect(accountDispose).toHaveBeenCalledOnce(); expect(grantDispose).toHaveBeenCalledOnce();
      expect(fetch).not.toHaveBeenCalled();
    });
  });

  it.each(["account", "grant"])("disposes a late %s reply after the other authority rejects", async late => {
    await withKey(async (key, f) => {
      let release!: (value: unknown) => void;
      const pending = new Promise(resolve => { release = resolve; });
      let finalized!: () => void;
      const disposed = new Promise<void>(resolve => { finalized = resolve; });
      const dispose = vi.fn(finalized);
      const failure = async () => { throw new Error("synthetic authority failure"); };
      f.bindings.NANOCODEX_USERS = { getByName: () => ({ readAccount: late === "account" ? () => pending : failure }) } as unknown as AccountAuthEnv["NANOCODEX_USERS"];
      f.bindings.NANOCODEX_ORGANIZATIONS = { getByName: () => ({ resolveOrganizationGrant: late === "grant" ? () => pending : failure }) } as unknown as AccountAuthEnv["NANOCODEX_ORGANIZATIONS"];
      await expect(key.resolveAuthorizedKey()).rejects.toThrow("synthetic authority failure");
      release({ ...(late === "account" ? f.account : f.grant), [Symbol.dispose]: dispose });
      await disposed;
      expect(dispose).toHaveBeenCalledOnce();
    });
  });
});
