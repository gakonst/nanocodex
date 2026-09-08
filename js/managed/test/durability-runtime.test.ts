import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { Agent } from "nanocodex/cloudflare";
import { Subagents } from "nanocodex/host";
import { createTools } from "nanocodex/tools";

it("persists voice start and end in Worker SQLite while Responses preconnect stays pending", async () => {
  const namespace = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
  await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    let release!: () => void;
    const handshake = new Promise<void>((resolve) => { release = resolve; });
    let opened = 0;
    const sockets: { closed: boolean }[] = [];
    const owner = { ctx, env: { NANOCODEX: { async fetch(_input: unknown, init?: RequestInit) {
      const headers = new Headers(init?.headers);
      expect(headers.get("x-nanocodex-subject")).toBe(ctx.id.toString());
      expect(headers.get("authorization")).toBe("Bearer NANOCODEX_PROVIDER_CREDENTIAL");
      const socket = {
        closed: false,
        addEventListener() {}, accept() {}, send() {},
        close() { this.closed = true; },
      };
      sockets.push(socket);
      await handshake;
      opened += 1;
      return { status: 101, headers: new Headers(), webSocket: socket };
    } } } };
    const options = { eventPersistence: "caller" as const };
    Object.defineProperty(options, Symbol.for("nanocodex.cloudflare.internalRuntime"), {
      value: { waitForPreconnect: false },
    });
    const revision = () => BigInt(ctx.storage.sql.exec<{ revision: string }>(
      "SELECT revision FROM nanocodex_durable_states",
    ).toArray()[0]?.revision ?? "0");
    let agent: Awaited<ReturnType<typeof Agent.create>> | undefined;
    try {
      agent = await Agent.create(owner, options);
      const initial = await agent.session.context();
      const initialRevision = revision();
      const started = await agent.session.realtime.start();
      expect(started.history.length).toBeGreaterThan(initial.history.length);
      expect(revision()).toBeGreaterThan(initialRevision);
      const startedRevision = revision();
      const ended = await agent.session.realtime.end();
      expect(ended.history.length).toBeGreaterThan(started.history.length);
      expect(revision()).toBeGreaterThan(startedRevision);
      expect(sockets).toHaveLength(1);
      expect(opened).toBe(0);
      const sessionId = agent.sessionId;

      // Reconstruct from the actual SQLite checkpoint while neither owned
      // Responses handshake has completed, rather than reading in-memory state.
      await agent.session.shutdown();
      agent = undefined;
      agent = await Agent.create(owner, options);
      expect(agent.sessionId).toBe(sessionId);
      expect(await agent.session.context()).toEqual(ended);
      expect(sockets).toHaveLength(2);
      expect(opened).toBe(0);
      await agent.session.shutdown();
      agent = undefined;
    } finally {
      release();
      await agent?.session.shutdown();
    }
    await expect.poll(() => sockets.every((socket) => socket.closed)).toBe(true);
    expect(opened).toBe(2);
  });
}, 20_000);

it("admits more than eight children with prepared tools and keeps checkpointed messaging usable", async () => {
  const namespace = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
  await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const owner = { ctx, env: { NANOCODEX: { async fetch() {
      return {
        status: 101, headers: new Headers(),
        webSocket: { addEventListener() {}, accept() {}, send() {}, close() {} },
      };
    } } } };
    const tools = await createTools({ tools: [] });
    const options = { tools, eventPersistence: "caller" as const };
    const agent = await Agent.create(owner, options);
    try {
      const attempts = await Promise.allSettled(Array.from({ length: 16 }, (_, index) => Subagents.spawn(agent, {
        role: `researcher-${index}`, task: "Research fixture without further delegation", outputSchema: { type: "object" },
      })));
      expect(attempts.filter((attempt) => attempt.status === "fulfilled")).toHaveLength(16);
      expect(attempts.filter((attempt) => attempt.status === "rejected")).toHaveLength(0);
      const directory = await Subagents.list(agent);
      expect(directory.agents).toHaveLength(16);
      await expect(Subagents.send(agent, {
        agentId: directory.agents[0]!.agent_id, priority: "urgent", purpose: "question", message: "Still available?",
      })).resolves.toMatchObject({ to_agent_id: directory.agents[0]!.agent_id });
    } finally { await agent.session.shutdown(); }
  });
}, 30_000);

it("retains interrupted children and bounds new delegation after Worker SQLite reconstruction", async () => {
  const namespace = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
  await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    // Transport stays open without making provider calls, so both children
    // remain active while directed messages are durably admitted.
    const owner = { ctx, env: { NANOCODEX: { async fetch() {
      return {
        status: 101, headers: new Headers(),
        webSocket: { addEventListener() {}, accept() {}, send() {}, close() {} },
      };
    } } } };
    const agent = await Agent.create(owner, { eventPersistence: "caller" });
    let reopened: Awaited<ReturnType<typeof Agent.create>> | undefined;
    try {
      const children = await Promise.all(["one", "two"].map((role) => Subagents.spawn(agent, {
        role, task: `Research fixture ${role}`, outputSchema: { type: "object" },
      })));
      for (let index = 0; index < 12; index++) {
        await expect(Subagents.send(agent, {
          agentId: children[index % 2]!.agent_id, priority: "urgent", purpose: "question",
          message: `Verified URLs? Ελληνικά 😀 ${index}`,
        })).resolves.toMatchObject({ to_agent_id: children[index % 2]!.agent_id });
      }
      expect((await Subagents.list(agent)).agents).toHaveLength(2);
      // A new context over retained storage models an evicted DO. Explicit
      // session.shutdown closes children, so it is not a restart simulation.
      const restoredOptions = { eventPersistence: "caller" as const };
      Object.defineProperty(restoredOptions, Symbol.for("nanocodex.cloudflare.internalRuntime"), {
        value: { subagentMaxConcurrency: 1 },
      });
      reopened = await Agent.create({ ...owner, ctx: {
        id: ctx.id, storage: ctx.storage,
        acceptWebSocket: ctx.acceptWebSocket.bind(ctx), getWebSockets: ctx.getWebSockets.bind(ctx),
      } }, restoredOptions);
      agent.dispose();
      const restoredChildren = (await Subagents.list(reopened, { includeCompleted: true })).agents;
      expect(restoredChildren).toHaveLength(2);
      expect(restoredChildren.map(({ agent_id }) => agent_id).sort()).toEqual(children.map(({ agent_id }) => agent_id).sort());
      for (const child of restoredChildren) expect(child.status).toEqual({ state: "interrupted" });
      // Reconstruction preserves logical children, not their running harnesses.
      // Fresh work must still obey the replacement host's concurrency policy.
      const fresh = await Subagents.spawn(reopened, {
        role: "replacement", task: "Continue research after restart", outputSchema: { type: "object" },
      });
      await expect(Subagents.spawn(reopened, {
        role: "excess", task: "Exceed the replacement host limit", outputSchema: { type: "object" },
      })).rejects.toThrow("sub-agent concurrency limit of 1");
      await expect(Subagents.send(reopened, {
        agentId: fresh.agent_id, priority: "urgent", message: "Still available after reconstruction?",
      })).resolves.toMatchObject({ to_agent_id: fresh.agent_id });
    } finally { await (reopened ?? agent).session.shutdown(); }
  });
}, 30_000);
