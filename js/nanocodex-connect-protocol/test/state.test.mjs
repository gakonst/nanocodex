import assert from "node:assert/strict";
import { test } from "node:test";

import {
  callbackCompletion,
  callbackCompletionChannelName,
  callbackCompletionFor,
  callbackCompletionStorageKey,
  isCallbackCompletion,
  isCallbackCompletionState,
  isScopedConnectConnectorState,
  scopedConnectConnectorState,
  unscopedConnectConnectorState,
} from "nanocodex-connect-protocol";

test("scopes valid broker states and reverses the framing", () => {
  for (const brokerState of [
    "0123456789abcdef",
    "A_b-C_d-E_f-G_h-",
    "a".repeat(480),
  ]) {
    const scoped = scopedConnectConnectorState(brokerState);
    assert.equal(scoped, `connect.${brokerState}`);
    assert.equal(isScopedConnectConnectorState(scoped), true);
    assert.equal(unscopedConnectConnectorState(scoped), brokerState);
  }
});

test("rejects broker states outside the canonical alphabet and length", () => {
  for (const value of [
    undefined,
    null,
    42,
    "a".repeat(15),
    "a".repeat(481),
    "0123456789abcde!",
    "connect.0123456789abcdef",
  ]) {
    assert.throws(
      () => scopedConnectConnectorState(value),
      /connector authorization state is invalid/,
    );
  }
});

test("recognizes only exactly framed connector callback states", () => {
  const brokerState = "0123456789abcdef";
  for (const value of [
    undefined,
    null,
    brokerState,
    `other.${brokerState}`,
    "connect.short",
    `connect.${brokerState}!`,
    `connect.${"a".repeat(481)}`,
  ]) {
    assert.equal(isScopedConnectConnectorState(value), false);
    assert.equal(unscopedConnectConnectorState(value), undefined);
  }
});

test("frames secret-free callback completion with exact connector and state matching", () => {
  const state = "s".repeat(43);
  const completion = callbackCompletion({
    connector: `mcp:${"m".repeat(43)}`,
    state,
    result: "success",
  });
  assert.deepEqual(callbackCompletionFor(completion, {
    connector: `mcp:${"m".repeat(43)}`,
    state,
  }), completion);
  assert.equal(callbackCompletionFor(completion, {
    connector: `mcp:${"m".repeat(43)}`,
    state: "x".repeat(43),
  }), undefined);
  assert.equal(callbackCompletionFor({ ...completion, token: "secret" }, {
    connector: completion.connector,
    state,
  }), undefined);
  assert.equal(isCallbackCompletion({ ...completion, state: "short" }), false);
  assert.equal(JSON.stringify(completion).includes("secret"), false);
});

test("creates canonical states and derives state-bounded same-origin transport names", () => {
  const state = "s".repeat(43);
  assert.equal(isCallbackCompletionState(state), true);
  assert.equal(callbackCompletionStorageKey(state), `nanocodex:oauth-completion:${state}`);
  assert.equal(callbackCompletionChannelName(state), `nanocodex-oauth-completion-${state}`);
  assert.throws(() => callbackCompletionStorageKey("short"), /completion state is invalid/);
});
