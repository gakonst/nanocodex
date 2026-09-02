import assert from "node:assert/strict";
import test from "node:test";

import { callbackCompletion, callbackCompletionStorageKey } from "nanocodex-connect-protocol";
import { observePopupCallback } from "../dist/popupCallback.js";

class FakeBroadcastChannel extends EventTarget {
  static channels = new Map();
  constructor(name) {
    super();
    this.name = name;
    FakeBroadcastChannel.channels.set(name, this);
  }
  close() { FakeBroadcastChannel.channels.delete(this.name); }
  emit(value) { this.dispatchEvent(new MessageEvent("message", { data: value })); }
}

class FakeRuntime extends EventTarget {
  location = { origin: "https://nanocodex.example" };
  values = new Map();
  localStorage = {
    getItem: (key) => this.values.get(key) ?? null,
    removeItem: (key) => { this.values.delete(key); },
    setItem: (key, value) => { this.values.set(key, value); },
  };
  BroadcastChannel = FakeBroadcastChannel;
}

test("settles an exact callback only once across broadcast and opener delivery", () => {
  const runtime = new FakeRuntime();
  const popup = {};
  const expected = {
    connector: `mcp:${"m".repeat(43)}`,
    origin: runtime.location.origin,
    source: popup,
    state: "s".repeat(43),
  };
  const completion = callbackCompletion({ ...expected, result: "success" });
  const received = [];
  observePopupCallback(expected, (value) => received.push(value), runtime);
  const channel = [...FakeBroadcastChannel.channels.values()][0];
  channel.emit({ ...completion, state: "x".repeat(43) });
  channel.emit(completion);
  runtime.dispatchEvent(Object.assign(new Event("message"), {
    data: completion,
    origin: expected.origin,
    source: popup,
  }));
  assert.deepEqual(received, [completion]);
});

test("consumes a persisted same-origin completion after listener reload", () => {
  const runtime = new FakeRuntime();
  const expected = {
    connector: `mcp:${"m".repeat(43)}`,
    origin: runtime.location.origin,
    state: "s".repeat(43),
  };
  const completion = callbackCompletion({ ...expected, result: "success" });
  const key = callbackCompletionStorageKey(expected.state);
  runtime.localStorage.setItem(key, JSON.stringify(completion));
  const received = [];
  observePopupCallback(expected, (value) => received.push(value), runtime);
  assert.deepEqual(received, [completion]);
  assert.equal(runtime.localStorage.getItem(key), null);
});

test("a replacement observer cannot be settled by the abandoned attempt state", () => {
  const runtime = new FakeRuntime();
  const connector = `mcp:${"m".repeat(43)}`;
  const abandoned = {
    connector,
    origin: runtime.location.origin,
    state: "a".repeat(43),
  };
  const replacement = {
    ...abandoned,
    state: "r".repeat(43),
  };
  const received = [];
  const disposeAbandoned = observePopupCallback(
    abandoned,
    (value) => received.push(value),
    runtime,
  );
  const abandonedChannel = FakeBroadcastChannel.channels.get(
    `nanocodex-oauth-completion-${abandoned.state}`,
  );
  disposeAbandoned();
  observePopupCallback(replacement, (value) => received.push(value), runtime);
  abandonedChannel.emit(callbackCompletion({ ...abandoned, result: "success" }));
  FakeBroadcastChannel.channels.get(
    `nanocodex-oauth-completion-${replacement.state}`,
  ).emit(callbackCompletion({ ...replacement, result: "success" }));
  assert.deepEqual(received, [
    callbackCompletion({ ...replacement, result: "success" }),
  ]);
});

test("rejects a subscriber origin outside the current same-origin transport", () => {
  assert.throws(() => observePopupCallback({
    connector: "github",
    origin: "https://evil.example",
    state: "s".repeat(43),
  }, () => {}, new FakeRuntime()), /origin does not match/);
});
