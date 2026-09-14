import { createExecutionContext, env, evictDurableObject, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { ManagedAgentOwnership, type DurableAgentSession, type Env } from "../src/index";
import { DEFAULT_AGENT_SETTINGS } from "../src/agent-settings";
import {
  managedCredentialSubject,
  readSessionCredentialSubject,
  scopedManagedModelEgress,
  sessionCredentialOwner,
} from "../src/session-credential-ownership";

const storageId = "a".repeat(64);
const ownerId = "11111111-1111-4111-8111-111111111111";
const sessionId = "018f25e8-7b51-7a32-8c4d-0123456789ab";
const coordinates = { owner_id: ownerId, session_id: sessionId, runtime_profile: "managed" };
const active = {
  subject: managedCredentialSubject(storageId), storageId,
  binding: { ...coordinates, subject: storageId, state: "active", strategy: "session_v1" },
  session: coordinates, initialization: { ...coordinates, state: "active" },
  deleting: false, deleted: false, exported: false, importPending: false,
};

describe("Session-owned credential authority", () => {
  it("reads the persisted voice strategy and rejects mismatched identities", async () => {
    for (const direct of [false, true]) {
      const subject = direct ? active.subject : storageId;
      expect(await readSessionCredentialSubject(Response.json({
        subject, strategy: direct ? "session_v1" : "directory_v1",
      }), storageId)).toEqual({ subject, direct });
    }
    for (const body of [{}, { subject: storageId, strategy: "session_v1" },
      { subject: active.subject, strategy: "directory_v1" },
      { subject: managedCredentialSubject("b".repeat(64)), strategy: "session_v1" }]) {
      expect(await readSessionCredentialSubject(Response.json(body), storageId)).toBeUndefined();
    }
    expect(await readSessionCredentialSubject(new Response(null, { status: 404 }), storageId)).toBeUndefined();
  });

  it("limits the private entrypoint to resolving one validated Session identity", async () => {
    const response = Response.json({ user_id: ownerId });
    const id = {} as DurableObjectId;
    const fetch = vi.fn(async () => response);
    const idFromString = vi.fn((value: string) => {
      if (value !== storageId) throw new Error("foreign namespace");
      return id;
    });
    const get = vi.fn(() => ({ fetch }));
    const entrypoint = new ManagedAgentOwnership(createExecutionContext(), {
      NANOCODEX_SESSIONS: { idFromString, get },
    } as unknown as Env);
    const url = `https://managed-ownership.internal/v1/resolve?subject=${active.subject}`;
    expect(await entrypoint.fetch(new Request(url))).toBe(response);
    expect(idFromString).toHaveBeenCalledWith(storageId);
    expect(get).toHaveBeenCalledWith(id);
    expect(fetch).toHaveBeenCalledWith(`https://session.internal/credential-owner?subject=${active.subject}`);
    for (const request of [new Request(url, { method: "POST" }),
      new Request(url.replace("managed-ownership.internal", "example.com")),
      new Request(`${url}&subject=${active.subject}`), new Request(`${url}&path=/delete`),
      new Request(url.replace(active.subject, storageId)),
      new Request(url.replace(storageId, "b".repeat(64)))]) {
      expect((await entrypoint.fetch(request)).status).toBe(400);
    }
    expect(get).toHaveBeenCalledTimes(1);
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("requires matching committed ownership at all three durable coordinates", () => {
    expect(sessionCredentialOwner(active)).toBe(ownerId);
    for (const field of ["deleting", "deleted", "exported", "importPending"] as const) {
      expect(sessionCredentialOwner({ ...active, [field]: true })).toBeUndefined();
    }
    for (const binding of [undefined, { ...active.binding, strategy: undefined },
      { ...active.binding, state: "preparing" }, { ...active.binding, owner_id: "other" },
      { ...active.binding, session_id: "other" }, { ...active.binding, subject: "b".repeat(64) }]) {
      expect(sessionCredentialOwner({ ...active, binding })).toBeUndefined();
    }
    for (const initialization of [undefined, { ...active.initialization, state: "deleted" },
      { ...active.initialization, owner_id: "other" },
      { ...active.initialization, session_id: "other" },
      { ...active.initialization, runtime_profile: "multiplayer" }]) {
      expect(sessionCredentialOwner({ ...active, initialization })).toBeUndefined();
    }
    expect(sessionCredentialOwner({ ...active, subject: storageId })).toBeUndefined();
    expect(sessionCredentialOwner({ ...active, session: undefined })).toBeUndefined();
    expect(sessionCredentialOwner({ ...active, session: { ...coordinates, runtime_profile: "multiplayer" } }))
      .toBeUndefined();
  });

  it("scopes only the SDK's exact model identity while retaining its request", async () => {
    let received: Request | undefined;
    const binding = { fetch: vi.fn(async (request: Request) => {
      received = request;
      return new Response(null, { status: 204 });
    }) } as unknown as Fetcher;
    const scoped = scopedManagedModelEgress(binding, storageId, active.subject);
    const request = new Request("https://nanocodex.internal/v1/responses", { headers: {
      "x-nanocodex-subject": storageId, upgrade: "websocket",
      authorization: "Bearer NANOCODEX_PROVIDER_CREDENTIAL", "thread-id": sessionId,
    } });
    expect((await scoped.fetch(request)).status).toBe(204);
    expect(received?.headers.get("x-nanocodex-subject")).toBe(active.subject);
    expect(received?.headers.get("thread-id")).toBe(sessionId);
    expect(received?.headers.get("upgrade")).toBe("websocket");
    expect(received?.headers.get("authorization")).toBe("Bearer NANOCODEX_PROVIDER_CREDENTIAL");
    expect(request.headers.get("x-nanocodex-subject")).toBe(storageId);
    expect(() => scoped.fetch("https://nanocodex.internal/v1/responses", { headers: {
      "x-nanocodex-subject": "b".repeat(64),
    } })).toThrow(/subject mismatch/);
    expect(binding.fetch).toHaveBeenCalledTimes(1);
  });

  for (const direct of [false, true]) {
    for (const lifecycle of ["active", "exported", "import-pending", "deleted"] as const) {
      it(`restores ${lifecycle} credential authority after eviction (direct=${direct})`, async () => {
        const sessions = (env as unknown as {
          NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
        }).NANOCODEX_SESSIONS;
        const stub = sessions.getByName(crypto.randomUUID());
        await runInDurableObject(stub, async (_session, state) => {
          state.storage.sql.exec(`INSERT INTO session_state (
            singleton, session_id, owner_id, organization_id, team_id,
            authorization_epoch, public_origin, runtime_profile, last_active
          ) VALUES (1, ?, ?, ?, ?, 1, 'https://nanocodex.example', 'managed', ?)`,
          sessionId, ownerId, "22222222-2222-4222-8222-222222222222",
          "33333333-3333-4333-8333-333333333333", Date.now());
          state.storage.sql.exec(`INSERT INTO session_initialization_ownership (
            singleton, session_id, owner_id, runtime_profile, state
          ) VALUES (1, ?, ?, 'managed', ?)`, sessionId, ownerId,
          lifecycle === "deleted" ? "deleted" : "active");
          await state.storage.put("nanocodex:credential-binding", {
            owner_id: ownerId, session_id: sessionId, subject: state.id.toString(),
            cleanup_at: Date.now(), state: "active", ...(direct ? { strategy: "session_v1" } : {}),
          });
          if (lifecycle === "exported") await state.storage.put("nanocodex:durability-exported", true);
          if (lifecycle === "import-pending") await state.storage.put("nanocodex:durability-import-state", "pending");
        });
        await evictDurableObject(stub);
        const subject = managedCredentialSubject(stub.id.toString());
        const resolve = await stub.fetch(`https://session.internal/credential-owner?subject=${subject}`);
        expect(resolve.status).toBe(direct && lifecycle === "active" ? 200 : 404);
        const voice = await stub.fetch("https://session.internal/credential-subject", { headers: {
          "x-nanocodex-owner-id": ownerId,
          "x-nanocodex-session-organization-id": "22222222-2222-4222-8222-222222222222",
          "x-nanocodex-session-team-id": "33333333-3333-4333-8333-333333333333",
          "x-nanocodex-authorization-epoch": "1", "x-nanocodex-capabilities": "[]",
        } });
        expect(voice.status).toBe(lifecycle === "active" ? 200 : 404);
        if (voice.ok) expect(await voice.json()).toEqual({
          subject: direct ? subject : stub.id.toString(), strategy: direct ? "session_v1" : "directory_v1",
        });
      });
    }

    it(`preserves creation strategy through prepare, commit and tombstone (direct=${direct})`, async () => {
      const sessions = (env as unknown as {
        NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
      }).NANOCODEX_SESSIONS;
      await runInDurableObject(sessions.getByName(crypto.randomUUID()), async (session, state) => {
        let binds = 0;
        const originalEnv = (session as unknown as { env: Record<string, unknown> }).env;
        const runtimeEnv = {
          ...originalEnv,
          MANAGED_AGENT_DIRECT_CREDENTIALS: String(direct),
          NANOCODEX: { fetch: async () => { binds += 1; return new Response(null, { status: 204 }); } },
          NANOCODEX_USERS: { getByName: () => ({ fetch: async () => new Response(null, { status: 204 }) }) },
        };
        Object.defineProperty(session, "env", { value: runtimeEnv });
        const id = state.id.toString();
        const subject = managedCredentialSubject(id);
        const resolve = () => session.fetch(new Request(`https://session.internal/credential-owner?subject=${subject}`));
        const prepare = () => session.fetch(new Request("https://session.internal/credential-binding", {
          method: "PUT", body: JSON.stringify({ owner_id: ownerId, session_id: sessionId,
            subject: id, durability_import: null }),
        }));
        expect((await resolve()).status).toBe(404);
        expect((await prepare()).status).toBe(204);
        // A deployment flag change cannot reclassify retained legacy ownership.
        runtimeEnv.MANAGED_AGENT_DIRECT_CREDENTIALS = String(!direct);
        expect((await prepare()).status).toBe(204);
        expect(await state.storage.get("nanocodex:credential-binding")).toMatchObject({
          state: "preparing", ...(direct ? { strategy: "session_v1" } : {}),
        });
        expect((await resolve()).status).toBe(404);
        expect((await session.fetch(new Request("https://session.internal/credential-binding/bind", {
          method: "POST",
        }))).status).toBe(204);
        expect(binds).toBe(direct ? 0 : 1);
        expect((await session.fetch(new Request("https://session.internal/initialize", {
          method: "PUT", body: JSON.stringify({
            session_id: sessionId, owner_id: ownerId,
            organization_id: "22222222-2222-4222-8222-222222222222",
            team_id: "33333333-3333-4333-8333-333333333333", authorization_epoch: 1,
            public_origin: "https://nanocodex.example", settings: DEFAULT_AGENT_SETTINGS,
          }),
        }))).status).toBe(204);
        expect((await resolve()).status).toBe(404);
        expect((await session.fetch(new Request("https://session.internal/credential-binding/commit", {
          method: "POST",
        }))).status).toBe(204);
        const resolved = await resolve();
        expect(resolved.status).toBe(direct ? 200 : 404);
        if (direct) expect(await resolved.json()).toEqual({ user_id: ownerId });
        const voiceSubject = (owner = ownerId) => session.fetch(new Request(
          "https://session.internal/credential-subject", { headers: {
            "x-nanocodex-owner-id": owner,
            "x-nanocodex-session-organization-id": "22222222-2222-4222-8222-222222222222",
            "x-nanocodex-session-team-id": "33333333-3333-4333-8333-333333333333",
            "x-nanocodex-authorization-epoch": "1", "x-nanocodex-capabilities": "[]",
          } },
        ));
        expect(await (await voiceSubject()).json()).toEqual({
          subject: direct ? subject : id, strategy: direct ? "session_v1" : "directory_v1",
        });
        expect((await voiceSubject("44444444-4444-4444-8444-444444444444")).status).toBe(404);
        expect((await session.fetch(new Request("https://session.internal/credential-subject"))).status).toBe(404);
        state.storage.sql.exec("UPDATE session_initialization_ownership SET state = 'deleted'");
        expect((await resolve()).status).toBe(404);
        if (direct) expect((await voiceSubject()).status).toBe(404);
        expect((await session.fetch(new Request("https://session.internal/credential-owner?subject=wrong"))).status)
          .toBe(404);
        expect(binds).toBe(direct ? 0 : 1);
        await state.storage.deleteAlarm();
      });
    });
  }
});
