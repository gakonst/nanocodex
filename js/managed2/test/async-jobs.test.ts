import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { AsyncJobs } from "../src/asyncJobs";
import { ToolTiming } from "../src/toolTiming";
import type { NamedTool, ToolContext } from "nanocodex";

it("keys background jobs by stable turn+call, caps active jobs, and never admits a mutating tool", async () => {
  const stub = (env as unknown as { SESSIONS: DurableObjectNamespace }).SESSIONS.getByName(`jobs-test:${crypto.randomUUID()}`);
  await runInDurableObject(stub, (_session, state) => {
    const read: NamedTool = { name: "web__run", description: "test read", handler: () => new Promise(() => {}) };
    const jobs = new AsyncJobs(state.storage, { web__run: read }, () => "original-turn", () => "accepted",
      async () => {}, () => {});
    const handler = jobs.tool(read).handler;
    const context = (id: string): ToolContext => ({ callId: id, parentCallId: "", sessionId: "s",
      turnId: "turn-1", model: "test", signal: new AbortController().signal });
    const first = handler({ q: "stable" }, context("call-1")) as { job_id: string };
    expect(handler({ q: "stable" }, context("call-1"))).toMatchObject({ job_id: first.job_id });
    expect(() => handler({ q: "changed" }, context("call-1"))).toThrow("async invocation conflict");
    for (let n = 2; n <= 8; n++) expect(handler({ q: `q${n}` }, context(`call-${n}`))).toHaveProperty("job_id");
    expect(() => handler({ q: "over capacity" }, context("call-9"))).toThrow("capacity reached");
    expect(() => jobs.tool({ name: "exec_command", description: "mutable", handler: () => "" }))
      .toThrow("not allowlisted");
    expect(jobs.status(first.job_id)).toMatchObject({ job_id: first.job_id, tool: "web__run" });
    const restored = new AsyncJobs(state.storage, { web__run: read }, () => "original-turn",
      () => "accepted", async () => {}, () => {});
    expect(restored.status(first.job_id)).toMatchObject({ job_id: first.job_id, tool: "web__run" });
    expect(restored.tool(read).handler({ q: "stable" }, context("call-1")))
      .toMatchObject({ job_id: first.job_id });
    const timing = new ToolTiming(state.storage.sql);
    timing.observe("internal-a", "external-a", "tool.call", { call_id: "same", tool: "web__run" }, Date.now());
    timing.observe("internal-b", "external-b", "tool.call", { call_id: "same", tool: "web__run" }, Date.now());
    expect(timing.externalTurn(context("same"))).toBeUndefined(); // refuse ambiguous cross-turn attribution
  });
});
