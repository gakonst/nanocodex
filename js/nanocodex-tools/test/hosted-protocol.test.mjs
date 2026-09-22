import assert from "node:assert/strict";
import test from "node:test";
import { parseHostedToolsManagedFrame } from "../dist/hosted/index.js";

const call = {
  type: "call", session_id: "session:1", call_id: "call:1", model: "gpt-6-astra",
  name: "lookup", input: {}, output_token_budget: 100, output_byte_budget: 1024,
  deadline_at: 1,
};

test("model routing metadata survives parsing without relaxing identities", () => {
  for (const model of ["gpt-6-astra", "openai/gpt-6-astra", "@openai/gpt-6-astra", "auto(openai/gpt-6-astra,anthropic/claude)", "x".repeat(128)]) {
    assert.deepEqual(parseHostedToolsManagedFrame(JSON.stringify({ ...call, model })), { ...call, model });
  }
  for (const model of [undefined, null, 1, {}, "", "x y", "x\n", "x\t", "x\0", "x\x7f", "é", "x".repeat(129)]) {
    assert.throws(() => parseHostedToolsManagedFrame(JSON.stringify({ ...call, model })), /model must/);
  }
  for (const field of ["session_id", "call_id", "name"]) {
    assert.throws(() => parseHostedToolsManagedFrame(JSON.stringify({ ...call, [field]: "provider/name" })), /safe ASCII/);
  }
});
