import assert from "node:assert/strict";
import { createServer, type ServerResponse } from "node:http";
import test from "node:test";
import { Agent, type ManagedEventData } from "nanocodex/managed";
import type { AgentEvent } from "nanocodex-react/agent";
import {
  createManagedAgentSource,
  type ManagedHistoryCache,
  type RetainedManagedHistory,
} from "nanocodex-connect-embed/managed";

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>(ready => { resolve = ready; });
  return { promise, resolve };
}

// The managed service is the only external dependency: use its real HTTP/SSE
// contract so this exercises the published source and managed client together.
for (const hostFailure of [false, true]) test(`public managed source pages, streams, resumes cached history and cancels over HTTP${hostFailure ? " with throwing host callbacks" : ""}`, { timeout: 10_000 }, async (t) => {
  const agentId = "018f0000-0000-7000-8000-000000000011";
  const path = `/v1/agents/${agentId}`;
  const trace: string[] = [];
  const history: ManagedEventData[] = [];
  const accepted = (id: string, input: string): ManagedEventData => ({
    type: "turn_accepted", cursor: String(history.length + 1), created_at: 1,
    turn_id: id, id, input, replayed: false,
  });
  const completed = (id: string, text: string): ManagedEventData => ({
    type: "turn_completed", cursor: String(history.length + 1), created_at: 1,
    turn_id: id, id, final_message: text, usage: null, citations: [],
  });
  for (const id of ["older", "recent"]) {
    history.push(accepted(id, `Prompt ${id}`));
    history.push(completed(id, `Answer ${id}`));
  }
  const streams = new Set<ServerResponse>();
  let streamOpened = deferred<void>();
  let streamClosed = deferred<void>();
  const emit = (event: ManagedEventData) => {
    history.push(event);
    for (const stream of streams) stream.write(`id: ${event.cursor}\nevent: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`);
  };
  const server = createServer(async (request, response) => {
    const url = new URL(request.url!, "http://localhost");
    trace.push(`${request.method} ${url.pathname}${url.search}`);
    const json = (value: unknown) => {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify(value));
    };
    if (url.pathname === `${path}/prepare`) return json({});
    if (url.pathname === path) return json({ latest_event_cursor: String(history.length) });
    if (url.pathname === `${path}/events/history`) {
      const before = url.searchParams.get("before");
      return json({ data: before ? history.slice(0, 2) : history.slice(2, 4), has_more: !before, latest_cursor: String(history.length) });
    }
    if (url.pathname === `${path}/events`) {
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.flushHeaders();
      streams.add(response);
      const closed = streamClosed;
      response.on("close", () => { streams.delete(response); closed.resolve(); });
      for (const event of history) if (Number(event.cursor) > Number(url.searchParams.get("cursor"))) {
        response.write(`id: ${event.cursor}\nevent: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`);
      }
      streamOpened.resolve();
      return;
    }
    if (url.pathname === `${path}/turns` && request.method === "POST") {
      let body = "";
      for await (const chunk of request) body += chunk;
      const { id, input } = JSON.parse(body) as { id: string; input: string };
      const event = accepted(id, input);
      json({ turn_id: id, accepted_cursor: event.cursor });
      emit(event);
      if (input !== "Cancel this turn") emit(completed(id, "Public managed answer"));
      return;
    }
    const cancelled = url.pathname.match(/\/turns\/([^/]+)\/cancel$/);
    if (cancelled && request.method === "POST") {
      const id = cancelled[1]!;
      json({ turn_id: id, state: "cancelled" });
      emit({ type: "turn_cancelled", cursor: String(history.length + 1), created_at: 1, turn_id: id, id });
      return;
    }
    response.writeHead(404, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: "not_found", message: "Unexpected fixture request" }));
  });
  server.listen(0, "127.0.0.1");
  await new Promise<void>(resolve => server.once("listening", resolve));
  t.after(() => { server.closeAllConnections(); server.close(); });
  const address = server.address();
  assert.ok(address && typeof address === "object");
  const managed = Agent.open(agentId, { baseUrl: `http://127.0.0.1:${address.port}` });
  let snapshot: RetainedManagedHistory | undefined;
  let attachments = 0;
  const cache: ManagedHistoryCache = {
    attach(id) {
      assert.equal(id, agentId);
      attachments++;
      return {
        snapshot,
        retain(value) {
          snapshot = value;
          if (hostFailure) throw new Error("Host cache retain failed");
        },
        release() {
          attachments--;
          if (hostFailure) throw new Error("Host cache release failed");
        },
      };
    },
  };
  const activity: string[] = [];
  const source = createManagedAgentSource(managed, { cache, onActivity: event => {
    activity.push(event.type);
    if (hostFailure) throw new Error("Host activity callback failed");
  } });
  const detach = (watcher: ReturnType<typeof source.events.watch>) => {
    if (hostFailure) assert.throws(() => watcher.off(), /Host cache release failed/);
    else watcher.off();
    assert.doesNotThrow(() => watcher.off(), "detach stays idempotent after a cache error");
  };
  const watcher = source.events.watch();
  t.after(() => watcher.off());
  const first = await new Promise<readonly AgentEvent[]>(resolve => watcher.onHistory!(resolve));
  assert.deepEqual(first.filter(event => event.type === "assistant.message").map(event => event.payload.text), ["Answer recent"]);
  assert.equal(await watcher.loadOlder!(), true);
  let loaded: readonly AgentEvent[] = [];
  watcher.onHistory!(events => { loaded = events; });
  assert.deepEqual(loaded.filter(event => event.type === "assistant.message").map(event => event.payload.text), ["Answer older", "Answer recent"]);
  await streamOpened.promise;
  const live: AgentEvent[] = [];
  const finished = deferred<void>();
  watcher.onEvent(event => { live.push(event); if (event.type === "run.completed") finished.resolve(); });
  const turn = source.turn.prompt({ input: "Say hello" });
  assert.equal((await turn.result()).finalMessage, "Public managed answer");
  await finished.promise;
  assert.deepEqual(live.map(event => event.type), ["managed.prompt", "assistant.message", "run.completed"]);
  assert.deepEqual(activity, ["turn_accepted", "turn_completed"]);
  turn.dispose();
  detach(watcher);
  await streamClosed.promise;
  assert.equal(attachments, 0);
  assert.equal(snapshot?.latestCursor, "6");

  streamOpened = deferred<void>();
  streamClosed = deferred<void>();
  const resumed = source.events.watch();
  t.after(() => resumed.off());
  let immediate: readonly AgentEvent[] = [];
  resumed.onHistory!(events => { immediate = events; });
  assert.equal(immediate.at(-1)?.type, "run.completed", "cache is visible synchronously");
  await streamOpened.promise;
  const cancellation = deferred<AgentEvent>();
  const admission = deferred<void>();
  resumed.onEvent(event => {
    if (event.type === "managed.prompt") admission.resolve();
    if (event.type === "run.failed") cancellation.resolve(event);
  });
  const cancelledTurn = source.turn.prompt({ input: "Cancel this turn" });
  await admission.promise;
  await cancelledTurn.cancel!();
  assert.equal((await cancellation.promise).payload.status, "cancelled");
  cancelledTurn.dispose();
  detach(resumed);
  await streamClosed.promise;
  assert.equal(attachments, 0);
  assert.equal(trace.filter(request => request.includes("/events/history")).length, 2, "resume must not refetch history");
  assert.ok(trace.includes(`GET ${path}/events?cursor=6`));
  assert.equal(snapshot?.latestCursor, "8");

  streamOpened = deferred<void>();
  streamClosed = deferred<void>();
  const noHistory = createManagedAgentSource(managed, { history: false }).events.watch();
  t.after(() => noHistory.off());
  assert.equal(noHistory.loadOlder, undefined, "no-history sources must not offer pagination");
  await streamOpened.promise;
  assert.equal(trace.filter(request => request.includes("/events/history")).length, 2,
    "no-history attachment performs no history reads");
  noHistory.off();
  await streamClosed.promise;
  assert.equal(streams.size, 0);

  let prepareSignal: AbortSignal | undefined;
  let prepareRequest: Promise<Response> | undefined;
  const failingSetup = createManagedAgentSource(Agent.open(agentId, {
    baseUrl: `http://127.0.0.1:${address.port}`,
    fetch(input, init) {
      prepareSignal = init?.signal ?? undefined;
      prepareRequest = fetch(input, init);
      return prepareRequest;
    },
  }), { cache: { attach() { throw new Error("Host cache attach failed"); } } });
  assert.throws(() => failingSetup.events.watch(), /Host cache attach failed/);
  assert.ok(prepareSignal?.aborted, "failed cache setup aborts the real prepare request");
  assert.ok(prepareRequest);
  await assert.rejects(prepareRequest, { name: "AbortError" });
  assert.equal(streams.size, 0);
  t.diagnostic(JSON.stringify({ hostFailure, trace, activity, retainedCursor: snapshot?.latestCursor, remainingStreams: streams.size }));
});
