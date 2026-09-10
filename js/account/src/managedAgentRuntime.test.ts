import type { AgentEvent } from "nanocodex-react/agent";
import type { ManagedEvent } from "nanocodex/managed";
import { appQueryClient, clearOtherAccountQueries } from "./queryClient.ts";
import assert from "node:assert/strict";
import test from "node:test";
import {
  listManagedConversations,
  loadManagedConversationSelection,
  terminalEvent,
  managedTerminalAgent,
  type ManagedTerminalSource,
} from "./managedAgentRuntime.ts";

// Detached mock watchers can outlive removal; avoid real GC timers in Node.
appQueryClient.setDefaultOptions({ ...appQueryClient.getDefaultOptions(), queries: { ...appQueryClient.getDefaultOptions().queries, gcTime: Infinity } });

const FIRST_AGENT_ID = "018f0000-0000-7000-8000-000000000001";
const SECOND_AGENT_ID = "018f0000-0000-7000-8000-000000000002";
const FORBIDDEN_AGENT_ID = "018f0000-0000-7000-8000-000000000003";

test("terminal projection preserves the persisted event time for command elapsed duration", () => {
  const projected = terminalEvent({
    cursor: "26", createdAt: 1788766853390, turnId: "cargo-turn", type: "event",
    data: {
      type: "event", cursor: "26", created_at: 1788766853390, turn_id: "cargo-turn",
      event: { protocol_version: 1, request_id: "internal", seq: 1, type: "tool.call",
        payload: { call_id: "cargo", tool: "exec_command", arguments: { cmd: "cargo test" } } },
    },
  }, "public-session", undefined, 26);
  assert.equal(projected?.payload.managed_event_created_at, 1788766853390);
  assert.equal(projected?.payload.managed_event_cursor, "26");
  assert.equal(projected?.payload.turn_id, "cargo-turn");
});

test("an exact agent route survives a successful list cached before another client created it", async (t) => {
  t.after(() => appQueryClient.clear());
  const originalLocation = Object.getOwnPropertyDescriptor(globalThis, "location");
  const originalFetch = globalThis.fetch;
  Object.defineProperty(globalThis, "location", {
    configurable: true,
    value: new URL("https://account.example"),
  });
  let agentIds = [FIRST_AGENT_ID];
  let listCalls = 0;
  const exactCalls: string[] = [];
  globalThis.fetch = async (input, init) => {
    const request = new Request(input, init);
    const path = new URL(request.url).pathname;
    if (request.method === "GET" && path === "/v1/agents") {
      listCalls += 1;
      return Response.json({
        data: agentIds,
        summaries: Object.fromEntries(agentIds.map((id, index) => [id, {
          title: `Agent ${index + 1}`,
          created_at: index + 1,
          updated_at: agentIds.length - index,
          turn_count: index,
        }])),
      });
    }
    const exactId = decodeURIComponent(path.slice("/v1/agents/".length));
    exactCalls.push(exactId);
    if (request.method === "GET" && agentIds.includes(exactId)) return Response.json({});
    return Response.json(
      { error: "forbidden", message: "That exact agent is not available to this account." },
      { status: 403 },
    );
  };
  t.after(() => {
    globalThis.fetch = originalFetch;
    if (originalLocation) Object.defineProperty(globalThis, "location", originalLocation);
    else Reflect.deleteProperty(globalThis, "location");
  });

  const initial = await listManagedConversations("stale-list-client");
  assert.deepEqual(initial.map(({ id }) => id), [FIRST_AGENT_ID]);
  agentIds = [SECOND_AGENT_ID, FIRST_AGENT_ID];

  const stale = await listManagedConversations("stale-list-client");
  assert.deepEqual(stale.map(({ id }) => id), [FIRST_AGENT_ID]);
  assert.equal(listCalls, 1);

  const routed = await loadManagedConversationSelection({
    accountId: "stale-list-client",
    routeAgentId: SECOND_AGENT_ID,
    retainedAgentId: FIRST_AGENT_ID,
    hasCredential: true,
  });
  assert.equal(routed.selectedId, SECOND_AGENT_ID);
  assert.equal(routed.replaceRoute, false);
  assert.deepEqual(routed.conversations.map(({ id }) => id), [SECOND_AGENT_ID, FIRST_AGENT_ID]);
  assert.deepEqual(exactCalls, [SECOND_AGENT_ID]);
  assert.equal(listCalls, 1);

  const augmented = await listManagedConversations("stale-list-client");
  assert.deepEqual(augmented.map(({ id }) => id), [SECOND_AGENT_ID, FIRST_AGENT_ID]);
  assert.equal(listCalls, 1);

  const refreshed = await listManagedConversations("stale-list-client", { refresh: true });
  assert.deepEqual(refreshed.map(({ id }) => id), [SECOND_AGENT_ID, FIRST_AGENT_ID]);
  assert.equal(listCalls, 2);

  await assert.rejects(
    loadManagedConversationSelection({
      accountId: "stale-list-client",
      routeAgentId: FORBIDDEN_AGENT_ID,
      retainedAgentId: FIRST_AGENT_ID,
      hasCredential: true,
    }),
    /That exact agent is not available to this account/,
  );
  assert.deepEqual(exactCalls, [SECOND_AGENT_ID, FORBIDDEN_AGENT_ID]);
});

function historyFixture(id: string) {
  const event = (cursor: string, text: string): ManagedEvent => ({
    cursor, createdAt: 1, turnId: `turn-${cursor}`, type: "turn_accepted",
    data: { cursor, created_at: 1, turn_id: `turn-${cursor}`, type: "turn_accepted", id: `turn-${cursor}`, input: text, replayed: false },
  });
  const events = [event("1", `History for ${id}`)];
  const cursors: string[] = [];
  let pages = 0;
  const source: ManagedTerminalSource = {
    id, type: "managed",
    events: {
      async page() { pages++; return { data: [...events], hasMore: false, latestCursor: events.at(-1)!.cursor }; },
      async *watch({ cursor = "0", signal } = {}) {
        cursors.push(cursor);
        for (const envelope of events) if (Number(envelope.cursor) > Number(cursor)) yield envelope;
        if (!signal?.aborted) await new Promise<void>((resolve) => signal?.addEventListener("abort", () => resolve(), { once: true }));
      },
    },
    turn: { prompt() { throw new Error("not used"); } },
  };
  return { source, cursors, get pages() { return pages; }, append: (text: string) => events.push(event(String(events.length + 1), text)) };
}

async function watchHistory(source: ManagedTerminalSource, accountId: string) {
  const watcher = managedTerminalAgent(source, { accountId }).events.watch();
  const history = await new Promise<readonly AgentEvent[]>((resolve) => watcher.onHistory!(resolve));
  return { watcher, history };
}

test("A → B → A restores cached history immediately and resumes after A's last cursor", async (t) => {
  t.after(() => appQueryClient.clear());
  const a = historyFixture(FIRST_AGENT_ID);
  const b = historyFixture(SECOND_AGENT_ID);
  const first = await watchHistory(a.source, "account-a");
  first.watcher.off();
  const second = await watchHistory(b.source, "account-a");
  second.watcher.off();
  a.append("Arrived while viewing B");
  const returning = managedTerminalAgent(a.source, { accountId: "account-a" }).events.watch();
  let immediate: readonly AgentEvent[] | undefined;
  returning.onHistory!((events) => { immediate = events; });
  assert.deepEqual(immediate, first.history);
  const live = await new Promise<AgentEvent>((resolve) => returning.onEvent(resolve));
  assert.equal(live.payload.text, "Arrived while viewing B");
  returning.off();
  assert.equal(a.pages, 1);
  assert.equal(b.pages, 1);
  assert.deepEqual(a.cursors, ["1", "1"]);
  const final = await watchHistory(a.source, "account-a");
  assert.equal(final.history.filter((event) => event.payload.text === "Arrived while viewing B").length, 1);
  final.watcher.off();
  assert.equal(a.cursors.at(-1), "2");
});

test("account changes discard thread snapshots, including a watcher detached after cache removal", async (t) => {
  t.after(() => appQueryClient.clear());
  const source = historyFixture(FIRST_AGENT_ID);
  const old = await watchHistory(source.source, "account-a");
  clearOtherAccountQueries(appQueryClient, "account-b");
  old.watcher.off();
  assert.equal(appQueryClient.getQueryCache().findAll({ queryKey: ["account", "account-a"] }).length, 0);
  const next = await watchHistory(source.source, "account-b");
  next.watcher.off();
  assert.equal(source.pages, 2);
});

test("managed live watcher delivers partial chunks before authoritative completion", async (t) => {
  t.after(() => appQueryClient.clear());
  let release!: () => void;
  const completion = new Promise<void>(resolve => { release = resolve; });
  const raw = (cursor: string, text: string): ManagedEvent => ({
    cursor, createdAt: 1, turnId: "stream-turn", type: "event",
    data: { type: "event", cursor, created_at: 1, turn_id: "stream-turn", event: {
      protocol_version: 1, request_id: "internal", seq: Number(cursor), type: "assistant.delta", payload: { text },
    } },
  });
  const source: ManagedTerminalSource = {
    id: FIRST_AGENT_ID, type: "managed",
    events: {
      async page() { return { data: [], hasMore: false, latestCursor: "0" }; },
      async *watch() {
        yield raw("1", "Hell");
        yield raw("2", "o");
        await completion;
        yield { cursor: "3", createdAt: 1, turnId: "stream-turn", type: "turn_completed" as const,
          data: { type: "turn_completed" as const, cursor: "3", created_at: 1, turn_id: "stream-turn", id: "stream-turn", final_message: "Hello!", usage: null, citations: [] } };
      },
    },
    turn: { prompt() { throw new Error("not used"); } },
  };
  const watcher = managedTerminalAgent(source, { accountId: "live-stream-test" }).events.watch();
  t.after(() => { release(); watcher.off(); });
  const received: AgentEvent[] = [];
  let partial!: () => void;
  let done!: () => void;
  const partialReady = new Promise<void>(resolve => { partial = resolve; });
  const finished = new Promise<void>(resolve => { done = resolve; });
  watcher.onEvent(event => {
    received.push(event);
    if (received.length === 2) partial();
    if (event.type === "run.completed") done();
  });
  await partialReady;
  assert.deepEqual(received.map(event => [event.type, event.payload.text]), [["assistant.delta", "Hell"], ["assistant.delta", "o"]]);
  assert.ok(received.every(event => event.request_id === FIRST_AGENT_ID && event.payload.turn_id === "stream-turn"));
  release();
  await finished;
  assert.deepEqual(received.map(event => event.type), ["assistant.delta", "assistant.delta", "assistant.message", "run.completed"]);
  assert.equal(received[2]!.payload.text, "Hello!");
});

test("incomplete live answers retain every chunk for history reprojection beyond the envelope count limit", async (t) => {
  t.after(() => appQueryClient.clear());
  const { MAX_MANAGED_RETAINED_ENVELOPES, managedHistoryEvents } = await import("./managedAgentRuntime.ts");
  const count = MAX_MANAGED_RETAINED_ENVELOPES + 20;
  let delivered!: () => void;
  const ready = new Promise<void>(resolve => { delivered = resolve; });
  const source: ManagedTerminalSource = {
    id: FIRST_AGENT_ID, type: "managed", turn: { prompt() { throw new Error("not used"); } },
    events: {
      async page() { return { data: [], hasMore: false, latestCursor: "0" }; },
      async *watch({ signal } = {}) {
        for (let seq = 1; seq <= count; seq++) yield {
          cursor: String(seq), createdAt: 1, turnId: "long", type: "event",
          data: { type: "event" as const, cursor: String(seq), created_at: 1, turn_id: "long", event: {
            type: "assistant.delta", payload: { text: seq === 1 ? "prefix" : "x", item_id: "answer" },
          } },
        };
        delivered();
        if (!signal?.aborted) await new Promise<void>(resolve => signal?.addEventListener("abort", () => resolve(), { once: true }));
      },
    },
  };
  const watcher = managedTerminalAgent(source, { accountId: "long-answer" }).events.watch();
  t.after(() => watcher.off());
  await ready;
  watcher.off();
  const cached = appQueryClient.getQueryCache().findAll().map(q => q.state.data).find((data): data is { envelopes: ManagedEvent[] } =>
    !!data && typeof data === "object" && "envelopes" in data);
  assert.equal(cached?.envelopes.length, count);
  const projected = managedHistoryEvents(cached!.envelopes, FIRST_AGENT_ID, undefined);
  assert.equal(projected.map(event => event.payload.text).join(""), "prefix" + "x".repeat(count - 1));
});

test("helper and commentary messages preserve identity and cannot suppress the authoritative root final", async () => {
  const { managedHistoryEvents } = await import("./managedAgentRuntime.ts");
  const history: ManagedEvent[] = [
    { cursor: "1", createdAt: 1, turnId: "turn", type: "event", data: { type: "event", cursor: "1", created_at: 1, turn_id: "turn", agent_id: 1,
      event: { type: "assistant.message", payload: { text: "Helper", phase: "final_answer", item_id: "helper" } } } },
    { cursor: "2", createdAt: 1, turnId: "turn", type: "event", data: { type: "event", cursor: "2", created_at: 1, turn_id: "turn",
      event: { type: "assistant.message", payload: { text: "Working", phase: "commentary", item_id: "comment" } } } },
    { cursor: "3", createdAt: 1, turnId: "turn", type: "turn_completed", data: { type: "turn_completed", cursor: "3", created_at: 1, turn_id: "turn", id: "turn", final_message: "Root", usage: null, citations: [] } },
  ];
  const projected = managedHistoryEvents(history, FIRST_AGENT_ID, undefined);
  assert.equal(projected[0]!.payload.managed_agent_id, 1);
  assert.deepEqual(projected.filter(event => event.type === "assistant.message").map(event => event.payload.text), ["Helper", "Working", "Root"]);
});


test("null-phase root message does not synthesize a duplicate final", async () => {
  const { managedHistoryEvents } = await import("./managedAgentRuntime.ts");
  const history: ManagedEvent[] = [
    { cursor: "1", createdAt: 1, turnId: "turn", type: "event", data: { type: "event", cursor: "1", created_at: 1, turn_id: "turn",
      event: { type: "assistant.message", payload: { text: "Root", phase: null, item_id: null } } } },
    { cursor: "2", createdAt: 1, turnId: "turn", type: "turn_completed", data: { type: "turn_completed", cursor: "2", created_at: 1, turn_id: "turn", id: "turn", final_message: "Root", usage: null, citations: [] } },
  ];
  assert.equal(managedHistoryEvents(history, FIRST_AGENT_ID, undefined).filter(event => event.type === "assistant.message").length, 1);
});

test("long-history tabs fetch only requested pages and preserve the loaded window across offline gaps", async (t) => {
  t.after(() => appQueryClient.clear());
  const events: ManagedEvent[] = [];
  const append = (count: number) => {
    for (let i = 0; i < count; i++) {
      const cursor = String(events.length + 1), id = `turn-${cursor}`;
      events.push({ cursor, createdAt: 1, turnId: id, type: "turn_accepted",
        data: { type: "turn_accepted", cursor, created_at: 1, turn_id: id, id, input: `message-${cursor}`, replayed: false } });
    }
  };
  append(1024);
  const pages: (string | undefined)[] = [];
  const cursors: string[] = [];
  let caughtUp!: () => void;
  let gapReady = new Promise<void>(resolve => { caughtUp = resolve; });
  const source: ManagedTerminalSource = {
    id: FIRST_AGENT_ID, type: "managed", turn: { prompt() { throw new Error("not used"); } },
    events: {
      async page({ before, limit = 128 } = {}) {
        pages.push(before);
        const end = before === undefined ? events.length : Number(before) - 1;
        const start = Math.max(0, end - limit);
        return { data: events.slice(start, end), hasMore: start > 0, latestCursor: String(events.length) };
      },
      async *watch({ cursor = "0", signal } = {}) {
        cursors.push(cursor);
        for (const event of events) if (Number(event.cursor) > Number(cursor)) yield event;
        caughtUp();
        if (!signal?.aborted) await new Promise<void>(resolve => signal?.addEventListener("abort", () => resolve(), { once: true }));
      },
    },
  };
  const first = await watchHistory(source, "paged-tabs");
  await gapReady;
  assert.deepEqual(pages, [undefined]);
  assert.equal(first.history.length, 128);
  assert.equal(await first.watcher.loadOlder!(), true);
  let loaded: readonly AgentEvent[] = [];
  first.watcher.onHistory!(history => { loaded = history; });
  assert.equal(loaded.length, 256);
  first.watcher.off();
  const original = loaded;
  append(3);
  gapReady = new Promise<void>(resolve => { caughtUp = resolve; });
  const returning = managedTerminalAgent(source, { accountId: "paged-tabs" }).events.watch();
  t.after(() => returning.off());
  let immediate: readonly AgentEvent[] = [];
  returning.onHistory!(history => { immediate = history; });
  assert.deepEqual(immediate, original);
  await gapReady;
  assert.deepEqual(pages, [undefined, "897"]);
  assert.deepEqual(cursors, ["1024", "1024"]);
  let restored: readonly AgentEvent[] = [];
  returning.onHistory!(history => { restored = history; });
  assert.equal(restored.length, 259);
  assert.deepEqual(restored.slice(0, 256), original);
  assert.deepEqual(restored.slice(-3).map(event => event.payload.text), ["message-1025", "message-1026", "message-1027"]);
  assert.equal(original.length, 256, "previous immutable snapshots remain unchanged");
  assert.equal(await returning.loadOlder!(), true);
  assert.deepEqual(pages, [undefined, "897", "769"]);
  assert.equal(restored.length, 387);
  assert.deepEqual(restored.slice(128, 384).map(event => event.payload.text), original.map(event => event.payload.text));
});

test("streaming work grows linearly instead of rescanning the incomplete transcript for every token", async (t) => {
  t.after(() => appQueryClient.clear());
  const count = 1200;
  let reads = 0;
  let delivered!: () => void;
  const ready = new Promise<void>(resolve => { delivered = resolve; });
  const source: ManagedTerminalSource = {
    id: FIRST_AGENT_ID, type: "managed", turn: { prompt() { throw new Error("not used"); } },
    events: {
      async page() { return { data: [], hasMore: false, latestCursor: "0" }; },
      async *watch({ signal } = {}) {
        for (let seq = 1; seq <= count; seq++) {
          const data: ManagedEvent["data"] = { type: "event", cursor: String(seq), created_at: 1, turn_id: "long",
            event: { type: "assistant.delta", payload: { text: "x", item_id: "answer" } } };
          yield { cursor: String(seq), createdAt: 1, turnId: "long", type: "event",
            get data() { reads++; return data; } };
        }
        delivered();
        if (!signal?.aborted) await new Promise<void>(resolve => signal?.addEventListener("abort", () => resolve(), { once: true }));
      },
    },
  };
  const watcher = managedTerminalAgent(source, { accountId: "linear-stream" }).events.watch();
  t.after(() => watcher.off());
  await ready;
  t.diagnostic(`${reads} envelope reads for ${count} deltas`);
  assert.ok(reads < count * 50, `${reads} envelope reads for ${count} deltas`);
  let history: readonly AgentEvent[] = [];
  watcher.onHistory!(events => { history = events; });
  assert.equal(history.length, count);
  assert.equal(history.map(event => event.payload.text).join(""), "x".repeat(count));
  assert.ok(Object.isFrozen(history));
});
