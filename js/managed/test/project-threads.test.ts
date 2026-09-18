import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import type { ToolContext } from "nanocodex";
import { projectThreadTools, projectThreadRegistry, initializeProjectThreads, spawnPersistentProjectThread, retainProjectSpawn, sendPersistentThreadFollowup, projectFollowupTurnId, type ProjectThread } from "../src/project-threads";

const parent = "11111111-1111-4111-8111-111111111111";
const child = "22222222-2222-4222-8222-222222222222";
const nested = "33333333-3333-4333-8333-333333333333";
const context = { callId: "call", parentCallId: "cell", sessionId: parent, model: "test", signal: new AbortController().signal } satisfies ToolContext;
const body = (id = child) => ({ agent_id: id, origin_turn_id: "origin", turn_id: "project:fix", title: "Fix sign-in", request_hash: "a".repeat(64) });
const req = (owner = parent, value?: unknown) => new Request(`https://user.internal/project-threads/${owner}`, {
  method: value === undefined ? "GET" : "POST", ...(value === undefined ? {} : { body: JSON.stringify(value) }),
});
async function inside(test: (storage: DurableObjectStorage) => Promise<void>) {
  const ns = (env as unknown as { NANOCODEX_USERS: DurableObjectNamespace }).NANOCODEX_USERS;
  await runInDurableObject(ns.getByName(crypto.randomUUID()), async (_, state) => {
    initializeProjectThreads(state.storage);
    for (const id of [parent, child, nested]) state.storage.sql.exec("INSERT INTO agent_registry(id,created_at,updated_at) VALUES (?,1,1)", id);
    await test(state.storage);
  });
}

describe("persistent project threads", () => {
  it("links exact retries once, keeps nested threads in the root project, and rejects changed requests", () => inside(async storage => {
    expect((await projectThreadRegistry(req(parent, body()), storage)).status).toBe(201);
    expect((await projectThreadRegistry(req(parent, body()), storage)).status).toBe(200);
    expect((await projectThreadRegistry(req(parent, { ...body(), request_hash: "b".repeat(64) }), storage)).status).toBe(409);
    expect((await projectThreadRegistry(req(child, body(nested)), storage)).status).toBe(201);
    const result = await (await projectThreadRegistry(req(child), storage)).json<{ project_root_id: string; data: unknown[] }>();
    expect(result.project_root_id).toBe(parent);
    expect(result.data).toHaveLength(2);
    expect(result.data[1]).toMatchObject({ agent_id: nested, parent_agent_id: child, project_root_id: parent });
    expect((await projectThreadRegistry(req(nested, body(parent)), storage)).status).toBe(409);
  }));
  it("does not link foreign or deleted agents and omits deleted children", () => inside(async storage => {
    const foreign = "44444444-4444-4444-8444-444444444444";
    expect((await projectThreadRegistry(req(foreign), storage)).status).toBe(404);
    expect((await projectThreadRegistry(req(parent, body(foreign)), storage)).status).toBe(409);
    expect((await projectThreadRegistry(req(parent, body()), storage)).status).toBe(201);
    storage.sql.exec("UPDATE agent_registry SET deleted_at=2 WHERE id=?", child);
    const result = await (await projectThreadRegistry(req(), storage)).json<{ data: unknown[] }>();
    expect(result.data).toEqual([]);
  }));
  it("recovers a failed admission with the same child and turn, and does not admit conflicting retries", () => inside(async storage => {
    let fail = true;
    const admitted = new Set<string>();
    const host = {
      sessionId: parent, originTurnId: "origin",
      identity: vi.fn(async () => child), create: vi.fn(async () => {}),
      existing: async (id: string) => storage.sql.exec<ProjectThread>("SELECT * FROM project_threads WHERE agent_id=?", id).toArray()[0],
      link: async (value: unknown) => {
        const response = await projectThreadRegistry(req(parent, value), storage);
        if (!response.ok) throw new Error("conflict");
        return response.json<ProjectThread>();
      },
      admit: vi.fn(async (agent: string, turn: string) => {
        if (fail) { fail = false; throw new Error("transport lost"); }
        admitted.add(agent + ":" + turn);
      }),
    };
    const input = { id: "fix", title: "Fix sign-in", input: "Reproduce the loop" };
    await expect(spawnPersistentProjectThread(input, host)).rejects.toThrow("transport lost");
    const receipt = await spawnPersistentProjectThread(input, host);
    await spawnPersistentProjectThread(input, host);
    expect(receipt).toMatchObject({ agent_id: child, parent_agent_id: parent, project_root_id: parent, turn_id: "project:fix", status: "accepted" });
    expect(admitted.size).toBe(1);
    expect(host.create).toHaveBeenCalledTimes(1);
    expect(host.identity).toHaveBeenLastCalledWith(`project:${parent}:fix`);
    await expect(spawnPersistentProjectThread({ ...input, input: "Different task" }, host)).rejects.toThrow("conflict");
    expect(host.admit).toHaveBeenCalledTimes(3);
  }));

  it("retains original creation configuration and origin through a retry after settings change", () => inside(async storage => {
    const input = { id: "fix", title: "Fix", input: "Reproduce" };
    const original = retainProjectSpawn(storage, input, '{"settings":{"model":"original"}}', "first-turn");
    expect(retainProjectSpawn(storage, input, '{"settings":{"model":"changed"}}', "later-turn")).toEqual(original);
    expect(original.originTurnId).toBe("first-turn");
    expect(() => retainProjectSpawn(storage, { ...input, input: "different" }, "{}", "later")).toThrow("conflicts");
  }));

  it("sends to unrelated conversations with sender-scoped IDs and preserves legacy retries", async () => {
    const admit = vi.fn(async () => {});
    const resolve = vi.fn(async () => "Existing conversation");
    const legacyTurnId = vi.fn(async (): Promise<string | undefined> => undefined);
    const host = { sessionId: parent, resolve, legacyTurnId, admit };
    const input = { agent_id: nested, id: "review", input: "Read this reference" };
    const first = await sendPersistentThreadFollowup(input, host);
    expect(await sendPersistentThreadFollowup(input, host)).toEqual(first);
    const second = await sendPersistentThreadFollowup(input, { ...host, sessionId: child });
    expect(first.turn_id).not.toBe(second.turn_id);
    expect(resolve).toHaveBeenCalledWith(nested);
    expect(admit).toHaveBeenCalledWith(nested, first.turn_id, "Existing conversation", input.input);
    expect(projectFollowupTurnId(parent, "x".repeat(64))).toMatch(/^[A-Za-z0-9._:-]{1,128}$/);
    legacyTurnId.mockResolvedValue("project-followup:review");
    expect(await sendPersistentThreadFollowup(input, host)).toMatchObject({ turn_id: "project-followup:review" });
  });

  it("does not admit or acknowledge a follow-up when target authorization or admission fails", async () => {
    const resolve = vi.fn(async (): Promise<string> => { throw new Error("thread is not accessible"); });
    const legacyTurnId = vi.fn(async () => undefined);
    const admit = vi.fn(async () => { throw new Error("conflicting input"); });
    const host = { sessionId: parent, resolve, legacyTurnId, admit };
    const input = { agent_id: nested, id: "review", input: "Reference" };
    await expect(sendPersistentThreadFollowup(input, host)).rejects.toThrow("not accessible");
    expect(legacyTurnId).not.toHaveBeenCalled();
    expect(admit).not.toHaveBeenCalled();
    resolve.mockResolvedValue("Conversation");
    await expect(sendPersistentThreadFollowup(input, host)).rejects.toThrow("conflicting input");
  });

  it("passes the exact invoking context and rejects authority overrides before execution", async () => {
    const spawn = vi.fn(async () => ({ agent_id: child }));
    const list = vi.fn(async () => []); const read = vi.fn(async () => ({}));
    const send = vi.fn(async () => ({}));
    const tools = projectThreadTools({ spawn, list, read, send });
    await tools[0]!.handler({ id: "fix", title: "Fix sign-in", input: "Reproduce and fix the loop" }, context);
    expect(spawn).toHaveBeenCalledExactlyOnceWith({ id: "fix", title: "Fix sign-in", input: "Reproduce and fix the loop" }, context);
    for (const extra of [{ owner_id: parent }, { capabilities: ["admin"] }, { configuration: {} }]) {
      await expect(async () => tools[0]!.handler({ id: "fix", title: "Fix", input: "Work", ...extra }, context)).rejects.toThrow();
    }
    expect(spawn).toHaveBeenCalledTimes(1);
    await expect(async () => tools[2]!.handler({ agent_id: "../../other" }, context)).rejects.toThrow();
    expect(read).not.toHaveBeenCalled();
    await tools[2]!.handler({ agent_id: child, turn_id: 'project-followup:review' }, context);
    expect(read).toHaveBeenCalledExactlyOnceWith(child, context, 'project-followup:review');
    await tools[3]!.handler({ agent_id: child, id: 'review', input: 'Address review comments' }, context);
    expect(send).toHaveBeenCalledExactlyOnceWith({ agent_id: child, id: 'review', input: 'Address review comments' }, context);
    await expect(async () => tools[3]!.handler({ agent_id: child, id: 'review', input: 'Work', capabilities: ['admin'] }, context)).rejects.toThrow();
    expect(send).toHaveBeenCalledTimes(1);
  });
});
