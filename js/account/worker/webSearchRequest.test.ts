import assert from "node:assert/strict";
import { test } from "node:test";
import { webSearchRequest } from "./webSearchRequest.ts";

test("empty Codex commands and zero output budget reach the provider unchanged", () => {
  const request = webSearchRequest({ session_id: "s1", commands: {}, max_output_tokens: 0 });
  assert.deepEqual(request.commands, {});
  assert.equal(request.max_output_tokens, 0);
  assert.throws(() => webSearchRequest({ session_id: "s1", commands: [] }), /commands/);
  assert.throws(() => webSearchRequest({ session_id: "s1", commands: {}, max_output_tokens: -1 }), /budget/);
});
