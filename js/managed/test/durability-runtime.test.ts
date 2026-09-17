import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import { Agent } from "nanocodex/cloudflare";
import { Subagents } from "nanocodex/host";
import { createTools } from "nanocodex/tools";

it("shares a cache key through the Worker transport without explicit cache options", async () => {
  const namespace = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
  await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const requests: { input: { type: string; role?: string; content?: {
      type: string; text?: string; prompt_cache_breakpoint?: unknown;
    }[] }[]; prompt_cache_options?: unknown; prompt_cache_key: string }[] = [];
    class ModelSocket extends EventTarget {
      readyState = 1;
      accept() {}
      close() { this.readyState = 3; }
      send(data: string) {
        requests.push(JSON.parse(data));
        queueMicrotask(() => this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify({
          type: "response.completed", response: {
            id: `cache-fixture-${requests.length}`, status: "completed", end_turn: true,
            output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "Hello" }] }],
            usage: { input_tokens: 100, output_tokens: 1, total_tokens: 101 },
          },
        }) })));
      }
    }
    const owner = { ctx, env: { NANOCODEX: { async fetch() {
      return { status: 101, headers: new Headers(), webSocket: new ModelSocket() };
    } } } };
    const options = { instructions: "Stable host instructions", eventPersistence: "caller" as const };
    Object.defineProperty(options, Symbol.for("nanocodex.cloudflare.internalRuntime"), {
      value: { responseControls: { promptCacheKey: "owner-team-key" } },
    });
    const agent = await Agent.create(owner, options);
    try {
      await agent.session.appendDeveloperMessage("Dynamic retrieved startup context");
      expect((await agent.turn.prompt({ input: "hi hi" }).result()).finalMessage).toBe("Hello");
      const request = requests.at(-1)!;
      expect(request.prompt_cache_options).toBeUndefined();
      expect(request.prompt_cache_key).toBe("owner-team-key");
      const messages = request.input.filter(item => item.role === "developer" && item.content);
      expect(messages.length).toBeGreaterThanOrEqual(2);
      expect(messages[0]!.content!.at(-1)!.prompt_cache_breakpoint).toBeUndefined();
      const retrieved = messages.find(item => item.content!.some(part => part.text === "Dynamic retrieved startup context"));
      expect(retrieved).toBeDefined();
      expect(retrieved!.content!.every(part => part.prompt_cache_breakpoint === undefined)).toBe(true);
    } finally { await agent.session.shutdown(); }
  });
}, 20_000);

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

it("keeps a delayed compaction owned until its checkpoint survives SQLite reconstruction", async () => {
  const namespace = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
  await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const compactStarted = Promise.withResolvers<void>();
    let completeCompaction: (() => void) | undefined;
    let requests = 0;
    class ModelSocket extends EventTarget {
      readyState = 1;
      accept() {}
      close() { this.readyState = 3; }
      emit(value: unknown) {
        this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(value) }));
      }
      send(_data: string) {
        requests++;
        if (requests === 1) {
          queueMicrotask(() => this.emit({ type: "response.completed", response: {
            id: "before-compaction", status: "completed", end_turn: true,
            output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "Hello" }] }],
            usage: { input_tokens: 100, output_tokens: 1, total_tokens: 101 },
          } }));
          return;
        }
        completeCompaction = () => {
          this.emit({ type: "response.output_item.done", item: {
            id: "delayed-summary", type: "compaction", encrypted_content: "opaque-summary",
          } });
          this.emit({ type: "response.completed", response: {
            id: "after-compaction", status: "completed", output: [],
            usage: { input_tokens: 100, output_tokens: 1, total_tokens: 101 },
          } });
        };
        compactStarted.resolve();
      }
    }
    const owner = { ctx, env: { NANOCODEX: { async fetch() {
      return { status: 101, headers: new Headers(), webSocket: new ModelSocket() };
    } } } };
    const options = { eventPersistence: "caller" as const };
    const agent = await Agent.create(owner, options);
    let restored: Awaited<ReturnType<typeof Agent.create>> | undefined;
    try {
      await agent.turn.prompt({ input: "retain this before compaction" }).result();
      let settled = false;
      const compact = agent.session.compact().finally(() => { settled = true; });
      ctx.waitUntil(compact);
      await compactStarted.promise;
      // The provider response is explicitly controlled: no sleeps or network
      // timing determine whether the operation is still owned.
      await Promise.resolve();
      expect(settled).toBe(false);
      expect(requests).toBe(2);
      completeCompaction!();
      await compact;
      const compacted = await agent.session.context();
      expect(JSON.stringify(compacted)).toContain("opaque-summary");
      restored = await Agent.create({ ...owner, ctx: {
        id: ctx.id, storage: ctx.storage,
        acceptWebSocket: ctx.acceptWebSocket.bind(ctx), getWebSockets: ctx.getWebSockets.bind(ctx),
      } }, options);
      agent.dispose();
      expect(await restored.session.context()).toEqual(compacted);
      expect(requests).toBe(2);
    } finally { await (restored ?? agent).session.shutdown(); }
  });
}, 20_000);

it("reconstructs SQLite ownership while provider compaction is pending and completes a replacement compact", async () => {
  const namespace = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace }).NANOCODEX_MEMORY;
  await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    let compactStarted = Promise.withResolvers<void>();
    let completeCompaction: (() => void) | undefined;
    let requests = 0;
    class ModelSocket extends EventTarget {
      readyState = 1;
      accept() {}
      close() { this.readyState = 3; }
      emit(value: unknown) {
        this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(value) }));
      }
      send(_data: string) {
        requests++;
        if (requests === 1) {
          queueMicrotask(() => this.emit({ type: "response.completed", response: {
            id: "before-compaction", status: "completed", end_turn: true,
            output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "Hello" }] }],
            usage: { input_tokens: 100, output_tokens: 1, total_tokens: 101 },
          } }));
          return;
        }
        completeCompaction = () => {
          this.emit({ type: "response.output_item.done", item: {
            id: "delayed-summary", type: "compaction", encrypted_content: "opaque-summary",
          } });
          this.emit({ type: "response.completed", response: {
            id: "after-compaction", status: "completed", output: [],
            usage: { input_tokens: 100, output_tokens: 1, total_tokens: 101 },
          } });
        };
        compactStarted.resolve();
      }
    }
    const owner = { ctx, env: { NANOCODEX: { async fetch() {
      return { status: 101, headers: new Headers(), webSocket: new ModelSocket() };
    } } } };
    const options = { eventPersistence: "caller" as const };
    const agent = await Agent.create(owner, options);
    let restored: Awaited<ReturnType<typeof Agent.create>> | undefined;
    try {
      await agent.turn.prompt({ input: "retain this before compaction" }).result();
      let settled = false;
      const compact = agent.session.compact().finally(() => { settled = true; });
      ctx.waitUntil(compact.catch(() => {}));
      const interrupted = compact.catch(error => error);
      await compactStarted.promise;
      // The provider response is explicitly controlled: no sleeps or network
      // timing determine whether the operation is still owned.
      await Promise.resolve();
      expect(settled).toBe(false);
      expect(requests).toBe(2);
      const originalHistory = await agent.session.context();
      const staleCompletion = completeCompaction!;
      restored = await Agent.create({ ...owner, ctx: {
        id: ctx.id, storage: ctx.storage,
        acceptWebSocket: ctx.acceptWebSocket.bind(ctx), getWebSockets: ctx.getWebSockets.bind(ctx),
      } }, options);
      expect(await restored.session.context()).toEqual(originalHistory);
      compactStarted = Promise.withResolvers<void>();
      const replacementCompact = restored.session.compact();
      await compactStarted.promise;
      expect(requests).toBe(3);
      // A late response from the retired transport cannot own the replacement
      // checkpoint. Complete it before the replacement's response on purpose.
      staleCompletion();
      completeCompaction!();
      await replacementCompact;
      await interrupted;
      agent.dispose();
      expect(JSON.stringify(await restored.session.context())).toContain("opaque-summary");
    } finally { await (restored ?? agent).session.shutdown(); }
  });
}, 20_000);
