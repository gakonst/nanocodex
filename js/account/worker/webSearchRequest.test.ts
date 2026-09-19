import assert from "node:assert/strict";
import { test } from "node:test";
import { webSearchRequest } from "./webSearchRequest.ts";

test("search gateway preserves Codex PDF, multi-operation, model and context request", () => {
  const commands = {
    screenshot: [{ ref_id: "pdf", pageno: 0 }],
    search_query: Array.from({ length: 5 }, (_, i) => ({ q: String(i) })),
    sports: [{ fn: "schedule", league: "nba" }, { tool: "sports", fn: "standings", league: "nfl" }],
  };
  const input = [{ type: "message", role: "user", content: [{ type: "input_text", text: "paper" }] }];
  assert.deepEqual(webSearchRequest({ session_id: "s1", model: "gpt-6-astra", commands, input, max_output_tokens: 321 }), {
    id: "s1", model: "gpt-6-astra", commands, input, max_output_tokens: 321,
    settings: { allowed_callers: ["direct"], external_web_access: true },
  });
});

test("empty Codex commands and zero output budget reach the provider unchanged", () => {
  const request = webSearchRequest({ session_id: "s1", commands: {}, max_output_tokens: 0 });
  assert.deepEqual(request.commands, {});
  assert.equal(request.max_output_tokens, 0);
  assert.throws(() => webSearchRequest({ session_id: "s1", commands: [] }), /commands/);
  assert.throws(() => webSearchRequest({ session_id: "s1", commands: {}, max_output_tokens: -1 }), /budget/);
});
