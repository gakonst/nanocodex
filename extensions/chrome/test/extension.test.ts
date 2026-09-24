import assert from "node:assert/strict";
import test from "node:test";
import {
  createConversationId,
  isConversationId,
  isManagedAgentId,
  migrateLegacyConversationSession,
} from "../lib/connect.ts";
import {
  cleanupPrompt,
  validateCleanupInput,
  visibleCleanupPrompt,
} from "../lib/extension.ts";

test("rejects unsupported cleanup actions before dispatch", () => {
  assert.throws(() => validateCleanupInput({ action: "click", selector: "button" }), /Unsupported cleanup action/);
  assert.throws(() => validateCleanupInput({ action: "preview", recipe: {} }), /document_revision/);
  assert.throws(() => validateCleanupInput({ action: "inspect", tab_id: 12 }), /unsupported field/);
  assert.deepEqual(validateCleanupInput({ action: "list_tabs" }), { action: "list_tabs" });
  const tabRef = "8968d6c8-05ea-4d9c-b8f5-e1fe12193be7";
  assert.deepEqual(validateCleanupInput({ action: "list_tabs", cursor: tabRef }), {
    action: "list_tabs",
    cursor: tabRef,
  });
  assert.deepEqual(validateCleanupInput({ action: "inspect", tab_ref: tabRef }), {
    action: "inspect",
    tab_ref: tabRef,
  });
  assert.throws(() => validateCleanupInput({ action: "inspect", tab_ref: "" }), /non-empty/);
  assert.throws(() => validateCleanupInput({ action: "inspect", tab_ref: "not-opaque" }), /opaque reference/);
  assert.throws(() => validateCleanupInput({ action: "list_tabs", cursor: "not-opaque" }), /opaque reference/);
});

test("recognizes only durable managed agent identifiers", () => {
  assert.equal(isManagedAgentId("d9428888-122b-4f2e-989a-0874c494beb7"), true);
  assert.equal(isManagedAgentId("agent_legacy-account-hash"), false);
  assert.equal(isManagedAgentId("D9428888-122B-4F2E-989A-0874C494BEB7"), false);
  assert.equal(isManagedAgentId("d9428888-122b-4f2e-789a-0874c494beb7-extra"), false);
});

test("creates isolated durable conversation identifiers", () => {
  const first = createConversationId();
  const second = createConversationId();
  assert.equal(isConversationId(first), true);
  assert.equal(isConversationId(second), true);
  assert.notEqual(first, second);
  assert.equal(isConversationId("legacy"), true);
  assert.equal(isConversationId("../../another-agent"), false);
});

test("keeps cleanup policy out of the visible transcript", () => {
  const visible = "hide everything except the timeline";
  const modelInput = cleanupPrompt(visible);
  assert.notEqual(modelInput, visible);
  assert.equal(visibleCleanupPrompt(modelInput), visible);
  assert.equal(visibleCleanupPrompt("an unrelated retained prompt"), "an unrelated retained prompt");
});

test("legacy session migration cannot resurrect a disconnected grant", () => {
  const oldKey = "nanocodex:connect:nanocodex-chrome:session";
  const migratedKey = `nanocodex:chrome:conversation:legacy:${oldKey}`;
  const values = new Map([[oldKey, "retained-grant"]]);
  const storage = {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
    removeItem: (key: string) => { values.delete(key); },
  };
  migrateLegacyConversationSession(storage);
  assert.equal(values.get(migratedKey), "retained-grant");
  assert.equal(values.has(oldKey), false);
  values.delete(migratedKey);
  migrateLegacyConversationSession(storage);
  assert.equal(values.has(migratedKey), false);
});
