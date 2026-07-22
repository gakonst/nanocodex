import assert from "node:assert/strict";
import { performance } from "node:perf_hooks";
import { test } from "node:test";

import {
  applyAgentEvents,
  initialTerminalState,
  queuePrompt,
  queueSteer,
  steerAdmitted,
  type AgentEvent,
} from "./agentTerminalModel.ts";

function event(seq: number, type: string, payload: Record<string, unknown>): AgentEvent {
  return { protocol_version: 1, request_id: "test", seq, type, payload };
}

test("a streaming burst remains one semantic transcript entry", () => {
  const events = Array.from({ length: 20_000 }, (_, index) =>
    event(index + 1, "assistant.delta", { text: "x" }),
  );
  const startedAt = performance.now();
  const state = applyAgentEvents(initialTerminalState(), events);
  const elapsed = performance.now() - startedAt;

  assert.equal(state.entries.length, 1);
  assert.equal(state.entries[0]?.kind, "assistant");
  assert.equal("text" in state.entries[0]! ? state.entries[0].text.length : 0, 20_000);
  assert.ok(elapsed < 750, `20,000 deltas took ${elapsed.toFixed(1)} ms`);
});

test("tool results update their original activity without growing the transcript", () => {
  const state = applyAgentEvents(initialTerminalState(), [
    event(1, "tool.call", {
      call_id: "call-1",
      tool: "browserInfo",
      arguments: {},
    }),
    event(2, "tool.result", {
      call_id: "call-1",
      status: "completed",
      result: { online: true },
      duration_ns: 12_000,
    }),
  ]);

  assert.equal(state.entries.length, 1);
  assert.deepEqual(state.entries[0], {
    id: "tool-call-1",
    kind: "tool",
    tool: {
      callId: "call-1",
      name: "browserInfo",
      arguments: "{}",
      result: undefined,
      status: "completed",
      durationNs: 12_000,
      children: [],
    },
  });
});

test("generated image content remains attached to its Code Mode row", () => {
  const state = applyAgentEvents(initialTerminalState(), [
    event(1, "tool.call", { call_id: "call-image", tool: "exec", arguments: "generatedImage(result)" }),
    event(2, "tool.result", {
      call_id: "call-image",
      status: "completed",
      result: [
        { type: "input_text", text: "Script completed" },
        { type: "input_image", image_url: "data:image/png;base64,a", detail: "high" },
      ],
      duration_ns: 2_000,
    }),
  ]);

  assert.equal(state.entries[0]?.kind, "tool");
  if (state.entries[0]?.kind !== "tool") return;
  assert.deepEqual(state.entries[0].tool.images, ["data:image/png;base64,a"]);
});

test("a final assistant message seals the streaming tail", () => {
  const state = applyAgentEvents(initialTerminalState(), [
    event(1, "run.started", {}),
    event(2, "assistant.delta", { text: "hel" }),
    event(3, "assistant.delta", { text: "lo" }),
    event(4, "assistant.message", { text: "hello" }),
    event(5, "run.completed", {}),
  ]);

  assert.equal(state.status, "Ready");
  assert.equal(state.running, false);
  assert.deepEqual(state.entries, [
    { id: "assistant-2", kind: "assistant", text: "hello", streaming: false },
  ]);
});

test("a completed turn seals a reasoning-only streaming tail", () => {
  const state = applyAgentEvents(initialTerminalState(), [
    event(1, "run.started", {}),
    event(2, "reasoning.summary.delta", { text: "checking" }),
    event(3, "run.completed", {}),
  ]);

  assert.deepEqual(state.entries, [
    { id: "reasoning-2", kind: "reasoning", text: "checking", streaming: false },
  ]);
});

test("native queue and steer lifecycles stay visually distinct", () => {
  let state = queuePrompt(initialTerminalState(), 1, "first");
  state = applyAgentEvents(state, [event(1, "run.started", {})]);
  state = queueSteer(state, 2, "steer now");
  state = steerAdmitted(state, 2);
  state = queuePrompt(state, 3, "run later");

  assert.equal(state.pendingSteers[0]?.state, "admitted");
  assert.equal(state.queuedPrompts[0]?.text, "run later");

  state = applyAgentEvents(state, [event(2, "run.steered", {})]);
  assert.equal(state.pendingSteers.length, 0);
  const steered = state.entries.at(-1);
  assert.equal(steered?.kind, "user");
  assert.equal(steered && "text" in steered ? steered.text : "", "steer now");
});

test("code-mode children render under their parent tool activity", () => {
  const state = applyAgentEvents(initialTerminalState(), [
    event(1, "tool.call", { call_id: "call-exec", tool: "exec", arguments: "await tools.browserInfo()" }),
    event(2, "tool.call", { call_id: "call-exec/code-1", tool: "browserInfo", arguments: null }),
    event(3, "tool.result", { call_id: "call-exec/code-1", tool: "browserInfo", status: "completed", duration_ns: 4_000 }),
  ]);
  const entry = state.entries[0];
  assert.equal(entry?.kind, "tool");
  assert.equal(entry?.kind === "tool" ? entry.tool.children.length : 0, 1);
  assert.equal(entry?.kind === "tool" ? entry.tool.children[0]?.status : undefined, "completed");
});

test("apply_patch activities summarize added and removed lines", () => {
  const patch = "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old();\n+new();\n keep();\n*** Add File: README.md\n+# Demo\n+\n*** End Patch";
  const state = applyAgentEvents(initialTerminalState(), [
    event(1, "tool.call", { call_id: "call-patch", tool: "apply_patch", arguments: patch }),
  ]);

  const entry = state.entries[0];
  assert.equal(entry?.kind, "tool");
  assert.equal(
    entry?.kind === "tool" ? entry.tool.arguments : undefined,
    "src/main.rs, README.md (+3 -1)",
  );
});
