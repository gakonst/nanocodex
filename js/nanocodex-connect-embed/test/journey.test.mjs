import assert from "node:assert/strict";
import test from "node:test";
import { createElement as h } from "react";
import { act, create } from "react-test-renderer";
import { AgentEmbed, AgentProvider, AgentMessages, AgentComposer, AgentLoadOlder, AgentStatus } from "nanocodex-connect-embed/primitives";
import { createConnectAgentSource } from "nanocodex-connect-embed/connect";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

// Synthetic backend boundary only: the shipped provider/controller/reducer and all UI actions are real.
function source(sessionId = "synthetic-session") {
  let sequence = 0;
  const events = new Set(), histories = new Set();
  const turns = [];
  let offCount = 0, historyFailure = false;
  const history = [{ request_id: sessionId, seq: 1, type: "assistant.message", payload: { text: "Earlier answer", turn_id: "earlier" } }];
  return {
    turns,
    get offCount() { return offCount; },
    setHistoryFailure(value) { historyFailure = value; },
    emit(type, payload = {}) { for (const listener of events) listener({ request_id: sessionId, seq: ++sequence, type, payload }); },
    agent: {
      sessionId,
      events: { watch() { return {
        onEvent(listener) { events.add(listener); return () => events.delete(listener); },
        onHistory(listener) { histories.add(listener); return () => histories.delete(listener); },
        async loadOlder() {
          if (historyFailure) throw new Error("History temporarily unavailable");
          for (const listener of histories) listener(history);
          return false;
        },
        off() { offCount++; },
      }; } },
      turn: { prompt({ input }) {
        if (input === "reject me") throw new Error("Permission denied for this prompt");
        const completion = Promise.withResolvers();
        const turn = {
          input, historyEntryId: `managed-user-turn-${turns.length + 1}`, steers: [], cancelled: 0, disposed: 0,
          async steer({ input }) { if (input === "Rejected correction") throw new Error("Correction refused"); this.steers.push(input); },
          async cancel() { this.cancelled++; completion.reject(Object.assign(new Error("cancelled"), { code: "turn_cancelled" })); },
          result() { return completion.promise; },
          dispose() { this.disposed++; },
          complete(finalMessage) { completion.resolve({ finalMessage, dispose() {} }); },
        };
        turns.push(turn);
        return turn;
      } },
    },
  };
}
const visible = root => JSON.stringify(root.toJSON());
const button = (root, text) => root.root.findAllByType("button").find(node => node.children.join("") === text);
async function send(root, input) {
  await act(async () => root.root.findByType("textarea").props.onChange({ currentTarget: { value: input } }));
  await act(async () => root.root.findByType("form").props.onSubmit({ preventDefault() {} }));
}

test("compose, stream tools/media, steer, queue and cancel through the real controller", async t => {
  const backend = source();
  const submitted = [];
  let root;
  await act(async () => { root = create(h(AgentEmbed, { agent: backend.agent, composer: { onSubmitted: input => submitted.push(input) } })); });
  t.after(async () => { await act(async () => root.unmount()); });
  assert.match(visible(root), /No messages yet/);
  await send(root, "Find a track");
  assert.equal(backend.turns[0].input, "Find a track");
  assert.equal(root.root.findByType("textarea").props.value, "");
  await act(async () => {
    backend.emit("run.started", { turn_id: "turn-1" });
    backend.emit("assistant.delta", { text: "Searching", turn_id: "turn-1" });
    backend.emit("tool.call", { call_id: "search", tool: "search_tracks", arguments: { query: "jazz" }, turn_id: "turn-1" });
    backend.emit("tool.result", { call_id: "search", status: "completed", result: { content: [{ type: "image", data: "AAAA", mimeType: "image/png" }, { type: "text", text: "Track found" }] }, turn_id: "turn-1" });
  });
  assert.match(visible(root), /Searching/);
  assert.match(visible(root), /search_tracks/);
  assert.match(visible(root), /Track found/);
  assert.equal(root.root.findAllByType("img").length, 1);
  await send(root, "Rejected correction");
  assert.equal(root.root.findByType("textarea").props.value, "Rejected correction");
  assert.match(visible(root), /Correction refused/);
  assert.deepEqual(submitted, ["Find a track"]);
  await send(root, "Make it acoustic");
  assert.deepEqual(backend.turns[0].steers, ["Make it acoustic"]);
  await act(async () => root.update(h(AgentEmbed, { agent: backend.agent, composer: { promptIntent: "queue" } })));
  await send(root, "Next track");
  assert.equal(backend.turns.length, 2);
  const queued = button(root, "Cancel queued message");
  assert.ok(queued);
  await act(async () => queued.props.onClick());
  assert.equal(backend.turns[1].cancelled, 1);
  assert.equal(backend.turns[0].cancelled, 0);
  await act(async () => backend.turns[0].complete("Try this acoustic recording."));
  assert.match(visible(root), /Try this acoustic recording/);
  assert.equal(button(root, "Stop").props.disabled, true);
  t.diagnostic("Observed prompt → live Searching text → search_tracks completed + generated image → steer → queued root withdrawn independently → final answer; controller released both turns.");
});

test("recover rejected prompts/history and release source ownership when switching sessions", async t => {
  const first = source("first"), next = source("next");
  let root;
  const layout = (agent, extra = {}) => h(AgentProvider, { agent, ...extra },
    h(AgentStatus), h(AgentLoadOlder), h(AgentMessages), h(AgentComposer));
  await act(async () => { root = create(layout(first.agent)); });
  t.after(async () => { await act(async () => root.unmount()); });
  await send(root, "reject me");
  assert.match(visible(root), /Permission denied/);
  assert.equal(root.root.findByType("textarea").props.value, "reject me");
  first.setHistoryFailure(true);
  await act(async () => button(root, "Load earlier messages").props.onClick());
  assert.match(visible(root), /History temporarily unavailable/);
  first.setHistoryFailure(false);
  await act(async () => button(root, "Load earlier messages").props.onClick());
  assert.match(visible(root), /Earlier answer/);
  assert.doesNotMatch(visible(root), /History temporarily unavailable/);
  await send(root, "Recover");
  await act(async () => first.emit("run.started", { turn_id: "turn-1" }));
  await act(async () => button(root, "Stop").props.onClick());
  assert.equal(first.turns[0].cancelled, 1);
  assert.match(visible(root), /Cancelled/);
  await act(async () => root.root.findByType("textarea").props.onChange({ currentTarget: { value: "Private unsent draft" } }));
  await act(async () => root.update(layout(next.agent)));
  assert.equal(first.offCount, 1);
  assert.equal(first.turns[0].disposed, 1);
  assert.equal(root.root.findByType("textarea").props.value, "");
  assert.doesNotMatch(visible(root), /Earlier answer|Permission denied|Private unsent draft/);
  await act(async () => first.emit("assistant.message", { text: "Old source leak" }));
  assert.doesNotMatch(visible(root), /Old source leak/);
  let retries = 0;
  await act(async () => root.update(layout(undefined, { error: "Connect approval expired", retry() { retries++; } })));
  assert.equal(root.root.findByType("textarea").props.disabled, true);
  await act(async () => button(root, "Retry connection").props.onClick());
  assert.equal(retries, 1);
  t.diagnostic("Rejected prompt preserved draft; failed history retry preserved transcript and recovered; Stop cancelled active work; source swap removed old messages/draft and detached old stream; disconnected embed disabled composer and exposed retry.");
});

test("Connect history-disabled adapter filters other turns before UI presentation and aborts on unmount", async t => {
  const queue = [];
  let wake, signal, prompt;
  const complete = Promise.withResolvers();
  const connectAgent = {
    id: "synthetic-connect", sessionId: "synthetic-connect",
    events: {
      async page() { assert.fail("history-disabled embeds must not request history"); },
      async *watch(options) {
        signal = options.signal;
        assert.equal(options.cursor, "latest");
        while (!signal.aborted) {
          if (queue.length) { yield queue.shift(); continue; }
          await new Promise(resolve => {
            wake = resolve;
            signal.addEventListener("abort", resolve, { once: true });
          });
        }
      },
    },
    turn: { prompt(options) { prompt = options; return {
      async steer() {}, async cancel() {},
      result({ signal }) { return Promise.race([complete.promise, new Promise((_, reject) => signal.addEventListener("abort", () => reject(signal.reason), { once: true }))]); },
    }; } },
  };
  const agent = createConnectAgentSource(connectAgent, { history: false });
  let root;
  await act(async () => { root = create(h(AgentEmbed, { agent })); });
  await send(root, "Only mine");
  await act(async () => {
    queue.push(...[
      ["1", "someone-else", "Private peer answer"], ["2", prompt.id, "My answer"],
    ].map(([cursor, turnId, text]) => ({ cursor, createdAt: Number(cursor), turnId, type: "event", data: {
      cursor, created_at: Number(cursor), turn_id: turnId, type: "event",
      event: { protocol_version: 1, request_id: "internal", seq: 1, type: "assistant.message", payload: { text } },
    } })));
    wake();
    await new Promise(resolve => setImmediate(resolve));
  });
  assert.match(visible(root), /My answer/);
  assert.doesNotMatch(visible(root), /Private peer answer/);
  await act(async () => complete.resolve({ finalMessage: "My final answer" }));
  assert.match(visible(root), /My final answer/);
  await act(async () => root.unmount());
  assert.equal(signal.aborted, true);
  t.diagnostic("Real createConnectAgentSource + AgentProvider/controller rendered only source-owned turn and final result with history disabled; unmount aborted live Connect watch.");
});
