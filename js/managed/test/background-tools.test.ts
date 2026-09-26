import { env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import type { NamedTool, ToolContext } from "nanocodex";
import { BackgroundReadToolRunner, type BackgroundToolDelivery } from "../src/background-tools";
import type { DurableAgentSession } from "../src/index";

const sessions = () => (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
const context = (): ToolContext => ({ sessionId: "session", turnId: "turn", callId: "call", parentCallId: "", model: "test", signal: new AbortController().signal });
const query = { search_query: [{ q: "weather" }] };

function setup(storage: DurableObjectStorage, handler: NamedTool["handler"], deliver: (delivery: BackgroundToolDelivery) => Promise<void>, authorize = () => true) {
  const tasks: Promise<void>[] = [];
  const runner = new BackgroundReadToolRunner({ storage, tool: { name: "web__run", description: "read", handler },
    waitUntil: promise => { tasks.push(promise); }, deliver, authorize });
  return { runner, tasks };
}

describe("opt-in background read-only web pilot", () => {
  it("returns before network completes, then durably delivers bounded lower-trust output", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_, state) => {
      let resolve!: (value: unknown) => void;
      const network = new Promise<unknown>(r => { resolve = r; });
      const received: BackgroundToolDelivery[] = [];
      const read = vi.fn(async () => network);
      const { runner, tasks } = setup(state.storage, read, async delivery => { received.push(delivery); });
      const result = await runner.tool().handler(query, context()) as { job_id: string; status: string };
      expect(result).toMatchObject({ status: "pending" });
      expect((await runner.tool().handler(query, context()) as { job_id: string }).job_id).toBe(result.job_id);
      await vi.waitFor(() => expect(read).toHaveBeenCalledOnce());
      expect(runner.pendingForTurn("turn")).toBe(1);
      expect(runner.get(result.job_id, "another-session")).toBeUndefined();
      expect(received).toEqual([]);
      resolve("x".repeat(20_000));
      await Promise.all(tasks);
      expect(received).toHaveLength(1);
      expect(received[0]).toMatchObject({ jobId: result.job_id, sessionId: "session", sourceTurnId: "turn", trust: "untrusted_tool_result" });
      expect(received[0]!.content.length).toBeLessThan(17_000);
      expect(runner.get(result.job_id, "session")?.status).toBe("delivered");
      expect(runner.get(result.job_id, "session")?.result).toContain("[truncated background result]");
      expect(runner.pendingForTurn("turn")).toBe(0);
    });
  });

  it("rejects unsafe tools, extra web operations and absent authorization", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_, state) => {
      expect(() => new BackgroundReadToolRunner({ storage: state.storage,
        tool: { name: "email", description: "write", handler: () => {} }, waitUntil: () => {}, deliver: async () => {}, authorize: () => true,
      })).toThrow("read-only");
      const run = vi.fn(async () => "ok");
      const { runner } = setup(state.storage, run, async () => {}, () => false);
      await expect(runner.tool().handler(query, context())).rejects.toThrow("not authorized");
      const { runner: allowed } = setup(state.storage, run, async () => {});
      await expect(allowed.tool().handler({ open: [{ ref_id: "something" }] }, context())).rejects.toThrow("search_query");
      await expect(allowed.tool().handler({ search_query: [{ q: "hello", danger: "write" }] }, context())).rejects.toThrow("invalid search query");
      expect(run).not.toHaveBeenCalled();
    });
  });

  it("retries uncertain read-only work after restart with the same ID", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_, state) => {
      const received: BackgroundToolDelivery[] = [];
      const first = setup(state.storage, async () => "unused", async message => { received.push(message); });
      state.storage.sql.exec("INSERT INTO managed_background_read_jobs VALUES (?, ?, ?, ?, ?, 'running', NULL)",
        "crashed-job", "session", "turn", JSON.stringify({ callId: "call", parentCallId: "", sessionId: "session", turnId: "turn", model: "test" }), JSON.stringify(query));
      const rerun = vi.fn(async () => "recovered");
      const next = setup(state.storage, rerun, async message => { received.push(message); });
      next.runner.resume();
      await Promise.all(next.tasks);
      expect(rerun).toHaveBeenCalledOnce();
      expect(received).toMatchObject([{ jobId: "crashed-job", content: "recovered" }]);
      expect(first.runner.get("crashed-job", "session")?.status).toBe("delivered");
    });
  });

  it("keeps completion on failed delivery and replays without re-executing read", async () => {
    await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_, state) => {
      const read = vi.fn(async () => ({ text: "late" }));
      const first = setup(state.storage, read, async () => { throw new Error("message append failed"); });
      const { job_id } = await first.runner.start(query, context());
      await expect(Promise.all(first.tasks)).rejects.toThrow("message append failed");
      expect(first.runner.undelivered("session")).toMatchObject([{ jobId: job_id, trust: "untrusted_tool_result" }]);
      const deliver = vi.fn(async () => {});
      const recovered = setup(state.storage, read, deliver);
      recovered.runner.resume();
      await Promise.all(recovered.tasks);
      expect(read).toHaveBeenCalledOnce();
      expect(deliver).toHaveBeenCalledOnce();
      expect(recovered.runner.undelivered("session")).toEqual([]);
    });
  });
});
