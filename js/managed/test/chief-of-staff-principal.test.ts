import { describe, expect, it } from "vitest";

import type { AccountAuthEnv } from "../src/account-auth";
import {
  chiefOfStaffIdentity,
  resolveChiefOfStaffIdentity,
} from "../src/chief-of-staff-principal";

describe("Chief of Staff provider identities", () => {
  it.each([
    { provider: "slack", tenant: "T123ABC", subject: "U123ABC" },
    { provider: "viber", tenant: "nanocodex-chief", subject: "01234567890A=" },
    { provider: "whatsapp", tenant: "123456789012345", subject: "15551234567" },
  ])("accepts an exact $provider identity", (identity) => {
    expect(chiefOfStaffIdentity(identity)).toEqual(identity);
  });

  it.each([
    null,
    { provider: "slack", tenant: "T123ABC", subject: "U123ABC", accountId: crypto.randomUUID() },
    { provider: "slack", tenant: "not-a-team", subject: "U123ABC" },
    { provider: "slack", tenant: "T123ABC", subject: "not-a-user" },
    { provider: "whatsapp", tenant: "T123ABC", subject: "15551234567" },
    { provider: "viber", tenant: "nanocodex chief", subject: "01234567890A=" },
  ])("rejects malformed or authority-widening identity %#", (identity) => {
    expect(chiefOfStaffIdentity(identity)).toBeUndefined();
  });

  it("maps equal identities stably and isolates provider, tenant, and subject", async () => {
    const env = identityEnv();
    const slack = { provider: "slack", tenant: "T123ABC", subject: "U123ABC" } as const;
    const first = await resolveChiefOfStaffIdentity(env, slack);
    const replay = await resolveChiefOfStaffIdentity(env, slack);
    const otherSubject = await resolveChiefOfStaffIdentity(env, { ...slack, subject: "U999XYZ" });
    const otherTenant = await resolveChiefOfStaffIdentity(env, { ...slack, tenant: "T999XYZ" });
    const otherProvider = await resolveChiefOfStaffIdentity(env, {
      provider: "viber",
      tenant: "T123ABC",
      subject: "U123ABC",
    });

    expect(replay.userId).toBe(first.userId);
    expect(new Set([
      first.userId,
      otherSubject.userId,
      otherTenant.userId,
      otherProvider.userId,
    ])).toHaveLength(4);
    for (const principal of [first, replay, otherSubject, otherTenant, otherProvider]) {
      expect(principal.capabilities).toEqual(["agents:read", "agents:write", "tools:use"]);
    }
  });
});

function identityEnv(): AccountAuthEnv {
  const authStores = new Map<string, Map<string, unknown>>();
  const accounts = new Map<string, { id: string; organizationId: string; persistent: boolean }>();
  const organizationId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
  const teamId = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
  const auth = {
    idFromName(name: string) { return name; },
    get(name: string) {
      let store = authStores.get(name);
      if (!store) {
        store = new Map();
        authStores.set(name, store);
      }
      return {
        async fetch(input: RequestInfo | URL, init?: RequestInit) {
          const request = new Request(input, init);
          const url = new URL(request.url);
          const key = url.searchParams.get("key")!;
          if (url.pathname === "/get") return Response.json({ value: store!.get(key) });
          if (url.pathname === "/create") {
            const { value } = await request.json<{ value: unknown }>();
            const created = !store!.has(key);
            if (created) store!.set(key, value);
            return Response.json({ created });
          }
          return new Response(null, { status: 404 });
        },
      };
    },
  } as unknown as DurableObjectNamespace;
  const users = {
    getByName(userId: string) {
      return {
        async fetch(input: RequestInfo | URL, init?: RequestInit) {
          const request = new Request(input, init);
          if (request.method === "PUT") {
            accounts.set(userId, { id: userId, organizationId, persistent: true });
          }
          const account = accounts.get(userId);
          return account
            ? Response.json({ ...account, createdAt: 1, lastAuthenticatedAt: 1 })
            : new Response(null, { status: 404 });
        },
      };
    },
  } as unknown as DurableObjectNamespace;
  const organizations = {
    getByName() {
      return { fetch: async () => Response.json({
        authorizationEpoch: 1,
        capabilities: ["agents:read", "agents:write", "tools:use", "organization:write"],
        organizationId,
        role: "owner",
        teamId,
      }) };
    },
  } as unknown as DurableObjectNamespace;
  return {
    NANOCODEX_AUTH: auth,
    NANOCODEX_API_KEYS: {} as DurableObjectNamespace,
    NANOCODEX_ORGANIZATIONS: organizations,
    NANOCODEX_USERS: users,
  };
}
