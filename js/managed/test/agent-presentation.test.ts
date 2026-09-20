import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import worker from "../src/index";
import { attachAgent, listAgents } from "../src/account-auth";
import { AgentPresentationWriter, cleanPresentationText, generatePresentationText, PRESENTATION_MODEL, presentationPending, type AgentPresentation } from "../src/agent-presentation";
const runtime = env as Parameters<typeof worker.fetch>[1];

describe("agent sidebar presentation", () => {
  it("uses the small model and bounds source and output", async () => {
    let body: Record<string, any> = {};
    const fetcher = { fetch: async (request: Request) => {
      body = await request.json();
      expect(request.headers.get("x-nanocodex-subject")).toBe("synthetic-subject");
      return Response.json({ output: [{ type: "message", content: [{ type: "output_text", text: "Fix sidebar status" }] }] });
    } } as unknown as Fetcher;
    expect(await generatePresentationText(fetcher, "synthetic-subject", "title", "x".repeat(10_000))).toBe("Fix sidebar status");
    expect(body.model).toBe(PRESENTATION_MODEL);
    expect(body.reasoning).toEqual({ effort: "low" });
    expect(body.input[0].content[0].text).toHaveLength(4000);
    expect(body.tools).toBeUndefined();
    expect(cleanPresentationText("SKIP", 45)).toBeUndefined();
    expect(cleanPresentationText("x".repeat(46), 45)).toBeUndefined();
    expect(cleanPresentationText("First line\nSecond line", 45)).toBeUndefined();
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

  it("keeps newer terminal state when registry deliveries arrive out of order", async () => {
    const owner = crypto.randomUUID(), id = crypto.randomUUID();
    await attachAgent(runtime, owner, id);
    const stub = runtime.NANOCODEX_USERS.getByName(owner);
    const post = (value: AgentPresentation) => stub.fetch(`https://user.internal/agents/${id}/presentation`, {
      method: "POST", body: JSON.stringify(value),
    });
    expect((await post({ revision: 3, status: "completed", activeTurnIds: [], updatedAt: 30, title: "Fix sidebar" })).status).toBe(204);
    await post({ revision: 2, status: "running", activeTurnIds: ["a"], updatedAt: 20, activity: "I'm checking state" });
    expect((await listAgents(runtime, owner))[0]).toMatchObject({ title: "Fix sidebar", presentation: { revision: 3, status: "completed" } });
  });
});
