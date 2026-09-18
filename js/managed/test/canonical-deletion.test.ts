import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { mainThreadRegistry } from "../src/main-thread-registry";
import type worker from "../src/index";
import type { DurableAgentSession } from "../src/index";

const runtime = env as Parameters<typeof worker.fetch>[1];
const tables = ["agent_registry", "main_threads", "canonical_projects", "conversation_projects", "project_threads"];
const snapshot = (storage: DurableObjectStorage) => tables.map(table => storage.sql.exec(`SELECT * FROM ${table}`).toArray());
const deletion = (id: string, team?: string) => new Request(`https://user.internal/agents/${id}${team === undefined ? "" : `?team_id=${encodeURIComponent(team)}`}`, { method: "DELETE" });
const registration = (id: string, kind: "main" | "project") => new Request(`https://user.internal/${kind === "main" ? "main-thread" : "projects/build"}?team_id=team-a`, {
  method: "PUT", body: JSON.stringify(kind === "main" ? { agent_id: id } : { name: "Build", coordinator_agent_id: id }),
});
type Role = "main" | "project" | "navigation";

async function fixture(team: string | null = "team-a", role?: Role) {
  const owner = crypto.randomUUID(), id = crypto.randomUUID();
  const account = runtime.NANOCODEX_USERS.getByName(owner);
  const session = runtime.NANOCODEX_SESSIONS.getByName(id);
  await runInDurableObject(account, async (_, state) => {
    await state.storage.put("account", { id: owner, organizationId: crypto.randomUUID(), persistent: true, createdAt: 1, lastAuthenticatedAt: 1 });
    state.storage.sql.exec("INSERT INTO agent_registry(id,created_at,updated_at,team_id) VALUES (?,1,1,?)", id, team);
    if (role === "main") state.storage.sql.exec("INSERT INTO main_threads VALUES ('team-a',?)", id);
    if (role === "project") state.storage.sql.exec("INSERT INTO canonical_projects VALUES ('team-a','build','Build',?)", id);
    if (role === "navigation") state.storage.sql.exec("INSERT INTO conversation_projects VALUES (?,?,?)", id, id, "Migrated");
  });
  await runInDurableObject(session, async (_, state) => {
    state.storage.sql.exec(`INSERT INTO session_state
      (singleton,session_id,owner_id,organization_id,team_id,authorization_epoch,public_origin,runtime_profile,last_active)
      VALUES (1,?,?,'organization','team-a',1,'https://nanocodex.example','managed',1)`, id, owner);
    await state.storage.put("retained-content", { text: "Keep this conversation" });
  });
  return { owner, id, account, session };
}

async function assertSessionPreserved(session: DurableObjectStub<DurableAgentSession>, action: (instance: { fetch(request: Request): Promise<Response> }) => Promise<void>) {
  await runInDurableObject(session, async (instance, state) => {
    const before = state.storage.sql.exec("SELECT * FROM session_state").toArray();
    await action(instance);
    expect(await state.storage.get("nanocodex:session-deleting")).toBeUndefined();
    expect(await state.storage.get("retained-content")).toEqual({ text: "Keep this conversation" });
    expect(state.storage.sql.exec("SELECT * FROM session_state").toArray()).toEqual(before);
    expect(await state.storage.getAlarm()).toBeNull();
    expect((await instance.fetch(new Request("https://session.internal/state"))).status).toBe(200);
  });
}

describe("canonical agent deletion reservation", () => {
  for (const role of ["main", "project", "navigation"] as const) {
    for (const team of ["team-a", null]) {
      it(`preserves ${role} roots and all registry rows (registry team=${team})`, async () => {
        const { id, account, session } = await fixture(team, role);
        const before = await runInDurableObject(account, async (_, state) => snapshot(state.storage));
        for (const scope of [undefined, "team-a"]) {
          const response = await account.fetch(deletion(id, scope));
          expect(response.status).toBe(409);
          expect(await response.json()).toEqual({ error: "canonical_agent_deletion_forbidden" });
        }
        await assertSessionPreserved(session, async instance => {
          const response = await instance.fetch(new Request("https://session.internal/session", { method: "DELETE" }));
          expect(response.status).toBe(409);
          expect(await response.json()).toEqual({ error: "canonical_agent_deletion_forbidden" });
        });
        expect(await runInDurableObject(account, async (_, state) => snapshot(state.storage))).toEqual(before);
      });
    }
  }

  it("an expired preparing-credential alarm cannot delete a protected root", async () => {
    const { owner, id, account, session } = await fixture(null, "navigation");
    const before = await runInDurableObject(account, async (_, state) => snapshot(state.storage));
    await runInDurableObject(session, async (instance, state) => {
      const prepared = await instance.fetch(new Request("https://session.internal/credential-binding", {
        method: "PUT", body: JSON.stringify({ owner_id: owner, session_id: id, subject: state.id.toString(), durability_import: null }),
      }));
      expect(prepared.status).toBe(204);
      const ownership = await state.storage.get<{ cleanup_at: number }>("nanocodex:credential-binding");
      expect(ownership).toBeDefined();
      const sessionBefore = state.storage.sql.exec("SELECT * FROM session_state").toArray();
      // Expire the real prepared in-memory ownership without reconstructing private fields.
      const clock = vi.spyOn(Date, "now").mockReturnValue(ownership!.cleanup_at + 1);
      try {
        await instance.alarm();
        expect(await state.storage.get("nanocodex:session-deleting")).toBeUndefined();
        expect(await state.storage.get("nanocodex:credential-binding")).toEqual(ownership);
        expect(state.storage.sql.exec("SELECT * FROM session_state").toArray()).toEqual(sessionBefore);
        expect(await state.storage.get("retained-content")).toEqual({ text: "Keep this conversation" });
      } finally {
        clock.mockRestore();
        await state.storage.deleteAlarm();
      }
    });
    expect(await runInDurableObject(account, async (_, state) => snapshot(state.storage))).toEqual(before);
  });

  it("uses persisted session team and rejects foreign registry membership without mutation", async () => {
    const { id, account, session } = await fixture("team-b");
    const before = await runInDurableObject(account, async (_, state) => snapshot(state.storage));
    expect((await account.fetch(deletion(id, "team-a"))).status).toBe(404);
    await assertSessionPreserved(session, async instance => {
      const response = await instance.fetch(new Request("https://session.internal/session?team_id=team-b", { method: "DELETE" }));
      expect(response.status).toBe(404);
    });
    expect(await runInDurableObject(account, async (_, state) => snapshot(state.storage))).toEqual(before);
  });

  it.each(["response", "throw"])("registry failure (%s) leaves session usable with no cleanup marker", async failure => {
    const { account, session } = await fixture();
    const before = await runInDurableObject(account, async (_, state) => snapshot(state.storage));
    await runInDurableObject(session, async instance => {
      Object.defineProperty(instance, "env", { value: { ...runtime, NANOCODEX_USERS: { getByName: () => ({ fetch: async () => {
        if (failure === "throw") throw new Error("registry unavailable");
        return new Response(null, { status: 503 });
      } }) } } });
    });
    await assertSessionPreserved(session, async instance => {
      expect((await instance.fetch(new Request("https://session.internal/session", { method: "DELETE" }))).status).toBe(503);
    });
    expect(await runInDurableObject(account, async (_, state) => snapshot(state.storage))).toEqual(before);
  });

  it.each(["team-a", null])("allows ordinary registry deletion with team %s and prevents resurrection", async team => {
    const { id, account } = await fixture(team);
    expect((await account.fetch(deletion(id, "team-a"))).status).toBe(204);
    await runInDurableObject(account, async (_, state) => {
      expect(state.storage.sql.exec<{ deleted_at: number | null }>("SELECT deleted_at FROM agent_registry WHERE id=?", id).one().deleted_at).toEqual(expect.any(Number));
    });
    expect((await account.fetch("https://user.internal/agents", { method: "POST", body: JSON.stringify({ agentId: id, teamId: "team-a" }) })).status).toBe(410);
    for (const kind of ["main", "project"] as const) expect((await account.fetch(registration(id, kind))).status).toBe(404);
  });

  it("allows deleting a navigation member while preserving its self-root", async () => {
    const { id, account } = await fixture();
    const root = crypto.randomUUID();
    await runInDurableObject(account, async (_, state) => {
      state.storage.sql.exec("INSERT INTO agent_registry(id,created_at,updated_at,team_id) VALUES (?,1,1,'team-a')", root);
      state.storage.sql.exec("INSERT INTO conversation_projects VALUES (?,?,?)", root, root, "Shared project");
      state.storage.sql.exec("INSERT INTO conversation_projects VALUES (?,?,?)", id, root, "Shared project");
    });
    expect((await account.fetch(deletion(id, "team-a"))).status).toBe(204);
    expect((await account.fetch(deletion(root, "team-a"))).status).toBe(409);
    await runInDurableObject(account, async (_, state) => {
      expect(state.storage.sql.exec<{ deleted_at: number | null }>("SELECT deleted_at FROM agent_registry WHERE id=?", id).one().deleted_at).toEqual(expect.any(Number));
      expect(state.storage.sql.exec<{ deleted_at: number | null }>("SELECT deleted_at FROM agent_registry WHERE id=?", root).one().deleted_at).toBeNull();
      expect(state.storage.sql.exec("SELECT * FROM conversation_projects WHERE agent_id=?", root).toArray()).toEqual([
        { agent_id: root, project_root_id: root, project_name: "Shared project" },
      ]);
    });
  });

  it("rechecks session scope after the account reservation yields", async () => {
    const { id, account, session } = await fixture();
    await runInDurableObject(session, async (instance, state) => {
      let reservationObserved = false;
      Object.defineProperty(instance, "env", { value: { ...runtime, NANOCODEX_USERS: { getByName: (userId: string) => ({ fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
        reservationObserved = true;
        const request = new Request(input, init);
        if (new URL(request.url).pathname !== `/agents/${id}`) return account.fetch(request);
        const response = await runtime.NANOCODEX_USERS.getByName(userId).fetch(request);
        state.storage.sql.exec("UPDATE session_state SET team_id='team-b'");
        return response;
      } }) } } });
      const response = await instance.fetch(new Request("https://session.internal/session", { method: "DELETE" }));
      expect(reservationObserved).toBe(true);
      expect(response.status).toBe(409);
      expect(await response.json()).toEqual({ error: "agent_scope_changed" });
      expect(await state.storage.get("nanocodex:session-deleting")).toBeUndefined();
      expect(await state.storage.get("retained-content")).toEqual({ text: "Keep this conversation" });
      expect(state.storage.sql.exec("SELECT session_id,team_id FROM session_state").toArray()).toEqual([{ session_id: id, team_id: "team-b" }]);
      expect(await state.storage.getAlarm()).toBeNull();
    });
  });

  it("tombstones ordinary registry ownership before any external session cleanup", async () => {
    const { id, owner, account, session } = await fixture();
    let cleanupObserved = false;
    await runInDurableObject(session, async (instance, state) => {
      await state.storage.put("nanocodex:credential-binding", { owner_id: owner, session_id: id, subject: state.id.toString(), cleanup_at: Date.now(), state: "active", strategy: "session_v1" });
      Object.defineProperty(instance, "env", { value: { ...runtime, NANOCODEX_MEMORY: { getByName: () => ({ fetch: async () => {
        cleanupObserved = true;
        await runInDurableObject(account, async (_, accountState) => {
          expect(accountState.storage.sql.exec<{ deleted_at: number | null }>("SELECT deleted_at FROM agent_registry WHERE id=?", id).one().deleted_at).toEqual(expect.any(Number));
        });
        return new Response(null, { status: 503 });
      } }) } } });
      expect((await instance.fetch(new Request("https://session.internal/session", { method: "DELETE" }))).status).toBe(503);
      expect(cleanupObserved).toBe(true);
      expect(await state.storage.get("nanocodex:session-deleting")).toBe(true);
      await state.storage.deleteAlarm();
    });
  });

  for (const kind of ["main", "project"] as const) {
    it(`deletion wins while ${kind} registration awaits identity verification`, async () => {
      const { id, account } = await fixture();
      await runInDurableObject(account, async (instance, state) => {
        const result = await mainThreadRegistry(registration(id, kind), state.storage, async () => {
          expect((await instance.fetch(deletion(id, "team-a"))).status).toBe(204);
          return true;
        });
        expect(result.status).toBe(404);
        expect(state.storage.sql.exec("SELECT * FROM main_threads").toArray()).toEqual([]);
        expect(state.storage.sql.exec("SELECT * FROM canonical_projects").toArray()).toEqual([]);
      });
    });

    it(`${kind} registration wins and subsequent deletion cannot tombstone its root`, async () => {
      const { id, account } = await fixture();
      expect((await account.fetch(registration(id, kind))).status).toBe(201);
      const before = await runInDurableObject(account, async (_, state) => snapshot(state.storage));
      expect((await account.fetch(deletion(id, "team-a"))).status).toBe(409);
      expect(await runInDurableObject(account, async (_, state) => snapshot(state.storage))).toEqual(before);
    });
  }
});
