import assert from "node:assert/strict";
import { test } from "node:test";
import { timeline } from "../src/renderer/timeline.ts";
test("durable events deduplicate cursors and reconcile streamed messages", () => {
  const envelope = (cursor, data) => ({
    cursor,
    turnId: "turn",
    type: data.type,
    data,
  });
  const events = [
    envelope("1", { type: "turn_accepted", id: "turn", input: "hello" }),
    envelope("2", {
      type: "event",
      event: { type: "assistant.delta", payload: { text: "he" } },
    }),
    envelope("3", {
      type: "event",
      event: { type: "assistant.message", payload: { text: "hello" } },
    }),
    envelope("4", {
      type: "turn_completed",
      id: "turn",
      final_message: "hello",
    }),
  ];
  const entries = timeline([...events, ...events]);
  assert.deepEqual(
    entries.map((entry) => [entry.kind, entry.text]),
    [
      ["user", "hello"],
      ["assistant", "hello"],
    ],
  );
  assert.equal(entries[1].streaming, false);
});

test("a durable steering event displays its receipt without inventing missing input", () => {
  const event = {
    cursor: "7",
    turnId: "turn",
    type: "event",
    data: {
      type: "event",
      event: {
        type: "run.steered",
        payload: { steer_index: 1, instruction_bytes: 42 },
      },
    },
  };
  assert.deepEqual(
    timeline([event, event]).map(({ kind, text }) => ({ kind, text })),
    [{ kind: "status", text: "Direction updated" }],
  );
});

test("cancellation has one clear terminal receipt", () => {
  const events = [
    {
      cursor: "1",
      turnId: "turn",
      type: "event",
      data: {
        type: "event",
        event: {
          type: "run.error",
          payload: { message: "the turn was cancelled" },
        },
      },
    },
    {
      cursor: "2",
      turnId: "turn",
      type: "turn_cancelled",
      data: { type: "turn_cancelled", id: "turn" },
    },
  ];
  assert.deepEqual(
    timeline(events).map((entry) => entry.text),
    ["Stopped by you."],
  );
});
