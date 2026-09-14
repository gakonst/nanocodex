import assert from "node:assert/strict";
import test from "node:test";
import { applyAgentEvents, initialState } from "../agent/transcript.mjs";

function call(id, tool, args, turnId = "turn-1") {
  return { seq: id, type: "tool.call", payload: { call_id: id, tool, arguments: args, turn_id: turnId } };
}
function result(id, output, turnId = "turn-1", status = "completed") {
  return { seq: id, type: "tool.result", payload: {
    call_id: id, tool: id === "cargo" ? "exec_command" : "write_stdin", turn_id: turnId,
    status, structured_result: output, duration_ns: 1_000_000_000,
  } };
}
const start = [
  call("cargo", "exec_command", { cmd: "cargo test --workspace" }),
  result("cargo", { session_id: 42, output: "Compiling first\n" }),
];

test("yielded commands remain running and polling updates one command through its exit", () => {
  let state = applyAgentEvents(initialState(), start);
  assert.equal(state.entries[0].tool.status, "running");
  const events = [
    call("poll-1", "write_stdin", { session_id: 42, chars: "" }),
    result("poll-1", { session_id: 42, output: "Compiling second\n" }),
    call("poll-2", "write_stdin", { session_id: 42 }),
    result("poll-2", { exit_code: 101, output: "error: build failed\n" }),
  ];
  for (const event of events) state = applyAgentEvents(state, [event]);
  assert.equal(state.entries.length, 1);
  assert.equal(state.entries[0].tool.status, "failed");
  assert.equal(JSON.parse(state.entries[0].tool.output).output,
    "Compiling first\nCompiling second\nerror: build failed\n");
  assert.deepEqual(state.terminalPolls, {});
  assert.deepEqual(state, applyAgentEvents(initialState(), [...start, ...events]));
  assert.equal(state.entries[0].tool.durationNs, undefined,
    "poll wait durations cannot establish total command elapsed time");
});

test("command elapsed time includes execution between polls and survives history replay", () => {
  const timed = (event, time) => ({ ...event, payload: { ...event.payload, managed_event_created_at: time } });
  const events = [
    timed(start[0], 1788766853390),
    timed(start[1], 1788766854949),
    timed(call("poll", "write_stdin", { session_id: 42 }), 1788766963000),
    timed(result("poll", { exit_code: 0, output: "13 tests passed", wall_time_seconds: 1.732 }), 1788766965479),
  ];
  let live = initialState();
  for (const event of events) live = applyAgentEvents(live, [event]);
  assert.equal(live.entries[0].tool.durationNs, 112_089_000_000);
  assert.deepEqual(live, applyAgentEvents(initialState(), events));
});

test("successful exits and immediate command failures use process exit status", () => {
  const state = applyAgentEvents(initialState(), [
    ...start,
    call("poll", "write_stdin", { session_id: 42 }),
    result("poll", { exit_code: 0, output: "test result: ok\n" }),
    call("bad-cwd", "exec_command", { cmd: "cd /missing && cargo test" }),
    result("bad-cwd", { exit_code: 1, output: "No such file or directory" }),
  ]);
  assert.deepEqual(state.entries.map(({ tool }) => tool.status), ["completed", "failed"]);
});

test("terminal output remains bounded valid JSON and retains the newest error", () => {
  const state = applyAgentEvents(initialState(), [
    ...start,
    call("poll", "write_stdin", { session_id: 42 }),
    result("poll", { exit_code: 101, output: "compiling\n".repeat(2000) + "error: latest failure" }),
  ]);
  const output = JSON.parse(state.entries[0].tool.output);
  assert.equal(output.exit_code, 101);
  assert.ok(output.output.startsWith("…\n"));
  assert.ok(output.output.endsWith("error: latest failure"));
  assert.ok(output.output.length <= 4000);
});

test("separate sessions and continued sessions across turns update their own commands", () => {
  const state = applyAgentEvents(initialState(), [
    ...start,
    call("other", "exec_command", { cmd: "sleep 60" }),
    result("other", { session_id: 43, output: "" }),
    call("poll", "write_stdin", { session_id: 42 }, "turn-2"),
    result("poll", { exit_code: 0, output: "passed" }, "turn-2"),
  ]);
  assert.deepEqual(state.entries.map(({ tool }) => tool.status), ["completed", "running"]);
  assert.equal(state.entries[0].turnId, "turn-1");
});

test("polls without retained command history stay visible", () => {
  const state = applyAgentEvents(initialState(), [
    call("poll", "write_stdin", { session_id: 42 }),
    result("poll", { exit_code: 101, output: "error: missing dependency" }),
  ]);
  assert.equal(state.entries[0].tool.name, "write_stdin");
  assert.equal(state.entries[0].tool.status, "failed");
  assert.match(state.entries[0].tool.output, /missing dependency/);
});

test("code-mode children receive progress and typed input stays visible", () => {
  const state = applyAgentEvents(initialState(), [
    call("code", "exec", "await tools.exec_command({cmd: 'cargo test'})"),
    call("code/code-1", "exec_command", { cmd: "cargo test" }),
    result("code/code-1", { session_id: 42, output: "" }),
    call("poll", "write_stdin", { session_id: 42, chars: "\u0003" }),
    result("poll", { exit_code: 130, output: "interrupted" }),
  ]);
  assert.equal(state.entries[0].tool.children[0].status, "failed");
  assert.equal(state.entries[1].tool.name, "write_stdin");
});
