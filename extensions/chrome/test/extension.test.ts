import assert from "node:assert/strict";
import test from "node:test";
import {
  migrateLegacyConversationSession,
} from "../lib/connect.ts";
import {
  validateCleanupInput,
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
