import assert from "node:assert/strict";
import test from "node:test";

import {
  MAX_HISTORY_TOOL_TEXT_BYTES,
  findSessionsToolInputSchema,
  parseHistoryFindSessionsInput,
  projectFindSessionsToolResult,
  projectReadSessionToolResult,
  readSessionToolInputSchema,
} from "nanocodex-tools/session";

const sessionId = "018f1f9a-7b3c-7a09-8000-000000000009";


test("session tool contracts bound input and strip host metadata from output", () => {
  assert.equal(findSessionsToolInputSchema().additionalProperties, false);
  assert.equal(readSessionToolInputSchema().properties.turn_ids.maxItems, 20);
  assert.throws(
    () => parseHistoryFindSessionsInput({ query: "x".repeat(4_097) }),
    /must not exceed 4096 bytes/,
  );

  const found = projectFindSessionsToolResult({
    query: "deploy",
    provider_token: "must-not-be-visible",
    results: [{
      thread_id: sessionId,
      title: "Deploy",
      turn_id: "turn-1",
      cursor: "2",
      score: 0.8,
      snippet: "Preview",
      raw_provider_metadata: { secret: true },
    }],
  }, 1);
  assert.deepEqual(found, {
    sessions: [{
      session_id: sessionId,
      title: "Deploy",
      turn_id: "turn-1",
      cursor: "2",
      score: 0.8,
      preview: "Preview",
    }],
  });

  const read = projectReadSessionToolResult({
    citations: [{ credential: "must-not-be-visible" }],
    turns: [{
      thread_id: sessionId,
      title: "Deploy",
      turn_id: "turn-1",
      cursor: "2",
      user: "🙂".repeat(2_000),
      assistant: "done",
      internal: "hidden",
    }],
  });
  assert.deepEqual(Object.keys(read.turns[0]).sort(), [
    "assistant", "cursor", "session_id", "title", "turn_id", "user",
  ]);
  assert.ok(new TextEncoder().encode(read.turns[0].user).byteLength
    <= MAX_HISTORY_TOOL_TEXT_BYTES);
});
