import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import worker from "../src/index";
import { attachAgent, listAgents } from "../src/account-auth";
import { AgentPresentationWriter, presentationPending, type AgentPresentation } from "../src/agent-presentation";
const runtime = env as Parameters<typeof worker.fetch>[1];

describe("agent sidebar presentation", () => {
  it("keeps user-message recency independent of output, continuations and retries", async () => {
    await runInDurableObject(runtime.NANOCODEX_USERS.getByName(crypto.randomUUID()), async (_, state) => {
      const pending: Promise<unknown>[] = [], published: AgentPresentation[] = [];
      const writer = new AgentPresentationWriter(state.storage, async value => { published.push(value); }, async () => undefined, p => pending.push(p));
      writer.observe("running", ["cron:initial"], "");
      await Promise.all(pending);
      expect(published.at(-1)?.lastUserMessageAt).toBe(0);
      writer.recordUserMessage("turn:first", 10, "Original request");
      writer.observe("running", ["cron:later"], "");
      writer.observe("completed", [], "");
      await Promise.all(pending);
      expect(published.at(-1)?.lastUserMessageAt).toBe(10);
      expect(published.at(-1)?.lastUserPrompt).toBe("Original request");
      const restored = new AgentPresentationWriter(state.storage, async value => { published.push(value); }, async () => undefined, p => pending.push(p));
      restored.recordUserMessage("turn:first", 100, "Duplicate request");
      await restored.flush();
      expect(published.at(-1)?.lastUserMessageAt).toBe(10);
      expect(published.at(-1)?.lastUserPrompt).toBe("Original request");
      restored.recordUserMessage("steer:second", 20, "  New\n request " + "x".repeat(600));
      await Promise.all(pending);
      expect(published.at(-1)?.lastUserMessageAt).toBe(20);
      expect(published.at(-1)?.lastUserPrompt).toHaveLength(500);
      expect(published.at(-1)?.lastUserPrompt).toMatch(/^New request /);
      restored.recordUserMessage("delayed", 15, "Older request");
      restored.observe("completed", [], "Stale first prompt");
      await restored.flush();
      expect(published.at(-1)?.lastUserPrompt).toMatch(/^New request /);
    });
  });

  it("defers advisory title inference until commentary or a terminal turn", async () => {
    await runInDurableObject(runtime.NANOCODEX_USERS.getByName(crypto.randomUUID()), async (_, state) => {
      const pending: Promise<unknown>[] = [], published: AgentPresentation[] = [], calls: string[] = [];
      const writer = new AgentPresentationWriter(state.storage, async value => { published.push(value); }, async kind => {
        calls.push(kind); return kind === "title" ? "Fix startup" : "I'm checking startup timing";
      }, p => pending.push(p));
      writer.observe("running", ["first"], "Fix startup", "first");
      writer.observe("stopping", ["first"], "Fix startup", "first");
      await Promise.all(pending);
      expect(calls).toEqual([]);
      expect(published.at(-1)).toMatchObject({ status: "stopping", activeTurnIds: ["first"] });
      writer.observe("running", ["first"], "Fix startup", "first", "Checking startup timing");
      await Promise.all(pending);
      writer.observe("completed", [], "Fix startup", "first");
      await Promise.all(pending);
      expect(calls).toEqual(["title", "activity"]);
      expect(published.at(-1)).toMatchObject({ status: "completed", title: "Fix startup" });
    });
    await runInDurableObject(runtime.NANOCODEX_USERS.getByName(crypto.randomUUID()), async (_, state) => {
      const pending: Promise<unknown>[] = [], calls: string[] = [];
      const writer = new AgentPresentationWriter(state.storage, async () => {}, async kind => {
        calls.push(kind); return "Fix startup";
      }, p => pending.push(p));
      writer.observe("running", ["first"], "Fix startup", "first");
      await Promise.all(pending);
      expect(calls).toEqual([]);
      writer.observe("completed", [], "Fix startup", "first");
      await Promise.all(pending);
      expect(calls).toEqual(["title"]);
    });
  });

  it("discards late activity after completion and retries durable delivery", async () => {
    await runInDurableObject(runtime.NANOCODEX_USERS.getByName(crypto.randomUUID()), async (_, state) => {
      const pending: Promise<unknown>[] = [];
      const published: AgentPresentation[] = [];
      let completeActivity!: (value: string) => void;
      let fail = true;
      const writer = new AgentPresentationWriter(state.storage, async value => {
        if (fail) throw new Error("unavailable");
        published.push(value);
      }, async kind => kind === "title" ? "Repair sidebar" : new Promise(resolve => { completeActivity = resolve; }), p => pending.push(p));
      writer.observe("running", ["turn-a"], "Repair sidebar", "turn-a", "I found stale sidebar state");
      writer.observe("completed", [], "Repair sidebar");
      completeActivity("I'm fixing sidebar state");
      await Promise.all(pending);
      expect(presentationPending(state.storage)).toBe(true);
      fail = false;
      const restored = new AgentPresentationWriter(state.storage, async value => { published.push(value); }, async () => undefined, p => pending.push(p));
      await restored.flush();
      expect(presentationPending(state.storage)).toBe(false);
      expect(published.at(-1)).toMatchObject({ status: "completed", activeTurnIds: [], title: "Repair sidebar" });
      expect(published.at(-1)?.activity).toBeUndefined();
    });
  });

  it("throttles progress calls and never requests a model-derived status", async () => {
    await runInDurableObject(runtime.NANOCODEX_USERS.getByName(crypto.randomUUID()), async (_, state) => {
      const pending: Promise<unknown>[] = [], calls: string[] = [];
      const writer = new AgentPresentationWriter(state.storage, async () => {}, async (kind) => {
        calls.push(kind); return kind === "title" ? "Fix sidebar" : "I'm checking sidebar state";
      }, p => pending.push(p));
      writer.observe("running", ["a"], "Fix sidebar", "a", "Checking sidebar state");
      await Promise.all(pending);
      writer.observe("running", ["a"], "Fix sidebar", "a", "Checking the next state");
      await Promise.all(pending);
      expect(calls).toEqual(["title", "activity"]);
    });
  });

  it("keeps presentations without a retained prompt valid for legacy agents", async () => {
    const owner = crypto.randomUUID(), id = crypto.randomUUID();
    await attachAgent(runtime, owner, id);
    const stub = runtime.NANOCODEX_USERS.getByName(owner);
    const post = (value: AgentPresentation) => stub.fetch(`https://user.internal/agents/${id}/presentation`, {
      method: "POST", body: JSON.stringify(value),
    });
    expect((await post({ revision: 1, status: "idle", activeTurnIds: [], updatedAt: 10 })).status).toBe(204);
    expect((await listAgents(runtime, owner))[0]?.presentation?.lastUserPrompt).toBe("");
    expect((await post({ revision: 2, status: "running", activeTurnIds: ["a"], updatedAt: 20 })).status).toBe(204);
    expect((await listAgents(runtime, owner))[0]?.presentation?.lastUserPrompt).toBe("");
    expect((await post({ revision: 3, status: "idle", activeTurnIds: [], updatedAt: 30,
      lastUserPrompt: null } as unknown as AgentPresentation)).status).toBe(400);
  });

  it("keeps newer terminal state when registry deliveries arrive out of order", async () => {
    const owner = crypto.randomUUID(), id = crypto.randomUUID();
    await attachAgent(runtime, owner, id);
    const stub = runtime.NANOCODEX_USERS.getByName(owner);
    const post = (value: AgentPresentation) => stub.fetch(`https://user.internal/agents/${id}/presentation`, {
      method: "POST", body: JSON.stringify(value),
    });
    expect((await post({ revision: 3, status: "completed", activeTurnIds: [], updatedAt: 30, title: "Fix sidebar", lastUserMessageAt: 15, lastUserPrompt: "Latest request" })).status).toBe(204);
    await post({ revision: 2, status: "running", activeTurnIds: ["a"], updatedAt: 20, activity: "I'm checking state", lastUserPrompt: "Stale request" });
    expect((await listAgents(runtime, owner))[0]).toMatchObject({ title: "Fix sidebar", presentation: { revision: 3, status: "completed", lastUserMessageAt: 15, lastUserPrompt: "Latest request" } });
    await post({ revision: 4, status: "running", activeTurnIds: ["scheduled"], updatedAt: 40 });
    expect((await listAgents(runtime, owner))[0]?.presentation).toMatchObject({ lastUserMessageAt: 15, lastUserPrompt: "Latest request" });
    expect((await post({ revision: 5, status: "idle", activeTurnIds: [], updatedAt: 50, lastUserPrompt: "x".repeat(501) })).status).toBe(400);
    expect((await post({ revision: 5, status: "idle", activeTurnIds: [], updatedAt: 50, lastUserPrompt: 42 } as unknown as AgentPresentation)).status).toBe(400);
  });
});
