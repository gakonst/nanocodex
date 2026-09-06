import assert from "node:assert/strict";
import test from "node:test";

import { createElement } from "react";
import { act, create } from "react-test-renderer";

import { AgentController, useAgentController } from "../agent/index.mjs";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

test("useAgentController projects the complete ordered semantic transcript", async () => {
  const frames = fakeAnimationFrames();
  const source = fakeAgent();
  const lifecycle = [];
  let controller;

  function Consumer() {
    controller = useAgentController(source.agent, {
      onEvent: (event) => lifecycle.push(event),
    });
    return null;
  }

  let root;
  try {
    await act(async () => { root = create(createElement(Consumer)); });
    await flushFrames(frames);

    let turn;
    await act(async () => { turn = await controller.submit("Build it"); });
    await flushFrames(frames);
    assert.equal(turn, source.turns[0]);
    assert.equal(controller.pendingTurns, 1);

    await act(async () => {
      source.emit(event(1, "run.started", { turn_id: "turn-1" }));
      source.emit(event(2, "reasoning.summary.delta", { text: "Inspect", turn_id: "turn-1" }));
      source.emit(event(3, "tool.call", {
        call_id: "parent", tool: "exec_command", arguments: { cmd: "pwd" }, turn_id: "turn-1",
      }));
      source.emit(event(4, "tool.call", {
        call_id: "parent/code-1", tool: "read_file", arguments: { path: "README.md" }, turn_id: "turn-1",
      }));
      source.emit(event(5, "tool.result", {
        call_id: "parent/code-1", status: "completed", result: "ok", turn_id: "turn-1",
      }));
      source.emit(event(6, "tool.call", {
        call_id: "plan", tool: "update_plan", turn_id: "turn-1",
        arguments: { plan: [{ step: "Ship", status: "in_progress" }] },
      }));
      source.emit(event(7, "assistant.delta", { text: "Draft", turn_id: "turn-1" }));
      source.emit(event(8, "assistant.message", { text: "Draft answer", turn_id: "turn-1" }));
      source.emit(event(9, "run.error", { message: "retained warning", turn_id: "turn-1" }));
      source.emit(event(10, "run.completed", { turn_id: "turn-1" }));
    });
    await flushFrames(frames);

    assert.deepEqual(controller.entries.map((entry) => entry.kind), [
      "user", "reasoning", "tool", "plan", "assistant", "error",
    ]);
    const tool = controller.entries.find((entry) => entry.kind === "tool").tool;
    assert.equal(tool.name, "exec_command");
    assert.equal(tool.children[0].name, "read_file");
    assert.equal(tool.children[0].status, "completed");
    assert.equal(controller.running, false);

    await act(async () => source.turns[0].complete("Final **Markdown**"));
    await flushFrames(frames);
    assert.equal(
      controller.entries.find((entry) => entry.kind === "assistant").text,
      "Final **Markdown**",
    );
    assert.equal(controller.pendingTurns, 0);
    assert.equal(source.turns[0].resultDisposals, 1);
    assert.equal(source.turns[0].disposals, 1);
    assert.ok(lifecycle.some((entry) => entry.type === "prompt.completed"));

    await act(async () => root.unmount());
    assert.equal(source.offs, 1);
    assert.equal(source.releases, 2);
  } finally {
    frames.restore();
  }
});

test("tool activities retain bounded input, successful output, duration, images, and status", async () => {
  const frames = fakeAnimationFrames();
  const source = fakeAgent();
  let controller;

  function Consumer() {
    controller = useAgentController(source.agent);
    return null;
  }

  let root;
  try {
    await act(async () => { root = create(createElement(Consumer)); });
    await flushFrames(frames);
    const calls = [
      ["sandbox", "sandbox_exec", { command: "printf hello", cwd: "." }],
      ["process", "sandbox_start_process", { command: "npm start", ready_port: 8_000 }],
      ["preview", "sandbox_preview", { port: 8_000 }],
      ["account", "accountInfo", {}],
      ["machine", "user_machine-a_exec_command", { cmd: "pwd" }],
      ["mcp", "mcp__linear__search_issues", { query: `bug ${"x".repeat(5_000)}` }],
    ];
    await act(async () => {
      source.emit(event(1, "run.started", { turn_id: "turn-tools" }));
      calls.forEach(([call_id, tool, arguments_], index) => source.emit(event(index + 2, "tool.call", {
        call_id, tool, arguments: arguments_, turn_id: "turn-tools",
      })));
    });
    await flushFrames(frames);
    const running = controller.entries.filter((entry) => entry.kind === "tool");
    assert.equal(running.length, calls.length);
    assert.ok(running.every((entry) => entry.tool.status === "running"));
    assert.equal(JSON.parse(running[0].tool.input).command, "printf hello");
    assert.ok(running.at(-1).tool.arguments.length <= 181);
    assert.ok(running.at(-1).tool.input.length <= 4_002);

    await act(async () => {
      source.emit(event(20, "tool.result", {
        call_id: "sandbox", status: "completed", duration_ns: 1_250_000_000,
        result: { success: true, exit_code: 0, stdout: "hello", stderr: "" },
        turn_id: "turn-tools",
      }));
      source.emit(event(21, "tool.result", {
        call_id: "process", status: "completed",
        result: { process_id: "proc", pid: 42, status: "running", ready_port: 8_000 },
        turn_id: "turn-tools",
      }));
      source.emit(event(22, "tool.result", {
        call_id: "preview", status: "completed",
        result: { port: 8_000, url: "https://preview.example.test/", persistent: false },
        turn_id: "turn-tools",
      }));
      source.emit(event(23, "tool.result", {
        call_id: "account", status: "completed",
        result: { status: "ready", authenticated: ["github"], connectorAccounts: {}, machines: [], vault: [] },
        turn_id: "turn-tools",
      }));
      source.emit(event(24, "tool.result", {
        call_id: "machine", status: "completed",
        result: "model-visible fallback",
        structured_result: [{ type: "input_image", image_url: "data:image/png;base64,AA==" }],
        metadata: { machine_name: "Machine A", tool_name: "exec_command" },
        turn_id: "turn-tools",
      }));
      source.emit(event(25, "tool.result", {
        call_id: "mcp", status: "completed", result: { issues: [1, 2], note: "y".repeat(5_000) },
        turn_id: "turn-tools",
      }));
    });
    await flushFrames(frames);
    const completed = controller.entries.filter((entry) => entry.kind === "tool");
    assert.ok(completed.every((entry) => entry.tool.status === "completed"));
    assert.equal(completed[0].tool.durationNs, 1_250_000_000);
    assert.equal(JSON.parse(completed[0].tool.output).stdout, "hello");
    assert.match(completed[2].tool.output, /preview\.example\.test/);
    assert.equal(JSON.parse(completed[3].tool.output).status, "ready");
    assert.deepEqual(completed[4].tool.images, ["data:image/png;base64,AA=="]);
    assert.deepEqual(JSON.parse(completed[4].tool.output), [
      { type: "input_image", image_url: "data:image/png;base64,AA==" },
    ]);
    assert.deepEqual(completed[4].tool.metadata, {
      machine_name: "Machine A",
      tool_name: "exec_command",
    });
    assert.equal(completed[5].tool.result, undefined);
    assert.match(completed[5].tool.output, /"issues"/);
    assert.ok(completed[5].tool.output.length <= 4_002);
    await act(async () => root.unmount());
  } finally {
    frames.restore();
  }
});

test("authoritative turn results release running state without a terminal stream event", async () => {
  const frames = fakeAnimationFrames();
  const source = fakeAgent();
  let controller;

  function Consumer() {
    controller = useAgentController(source.agent);
    return null;
  }

  let root;
  try {
    await act(async () => { root = create(createElement(Consumer)); });
    await flushFrames(frames);
    await act(async () => { await controller.submit("first"); });
    await act(async () => source.emit(event(1, "run.started", { turn_id: "turn-1" })));
    await flushFrames(frames);
    assert.equal(controller.running, true);

    await act(async () => source.turns[0].complete("finished authoritatively"));
    await flushFrames(frames);
    assert.equal(controller.running, false);
    assert.equal(controller.pendingTurns, 0);

    await act(async () => { await controller.submit("second", { intent: "queue" }); });
    assert.equal(source.turns[1].input, "second");
    await act(async () => source.emit(event(2, "run.started", { turn_id: "turn-2" })));
    await flushFrames(frames);
    assert.equal(controller.running, true);
    await act(async () => source.turns[1].fail(new Error("request failed")));
    await flushFrames(frames);
    assert.equal(controller.running, false);
    assert.equal(controller.pendingTurns, 0);
    await act(async () => root.unmount());
  } finally {
    frames.restore();
  }
});

test("prompt controls steer active work, queue roots, cancel the latest turn, and dispose exactly", async () => {
  const frames = fakeAnimationFrames();
  const source = fakeAgent();
  let controller;

  function Consumer() {
    controller = useAgentController(source.agent);
    return null;
  }

  let root;
  try {
    await act(async () => { root = create(createElement(Consumer)); });
    await flushFrames(frames);
    await act(async () => { await controller.submit("first"); });
    source.emit(event(1, "run.started", { turn_id: "turn-1" }));
    await flushFrames(frames);

    await act(async () => { await controller.steer("adjust it"); });
    assert.deepEqual(source.turns[0].steers, ["adjust it"]);
    await act(async () => { await controller.submit("second", { intent: "queue" }); });
    await flushFrames(frames);
    assert.equal(source.turns.length, 2);
    assert.equal(controller.pendingTurns, 2);

    await act(async () => { assert.equal(await controller.cancel(), true); });
    assert.equal(source.turns[1].cancelled, true);
    assert.equal(source.turns[0].cancelled, false);

    await act(async () => root.unmount());
    assert.deepEqual(source.turns.map((turn) => turn.disposals), [1, 1]);
    controller.dispose();
    assert.deepEqual(source.turns.map((turn) => turn.disposals), [1, 1]);
  } finally {
    frames.restore();
  }
});

for (const completion of ["cancelled", "completed"]) {
  test(`Stop fences a pending correction even when the old turn ${completion}`, async () => {
    const frames = fakeAnimationFrames();
    const source = fakeAgent();
    let controller, root;
    function Consumer() {
      controller = useAgentController(source.agent);
      return null;
    }
    const steering = Promise.withResolvers();
    const cancellation = Promise.withResolvers();
    try {
      await act(async () => { root = create(createElement(Consumer)); });
      await act(async () => { await controller.submit("original"); });
      let cancelCalls = 0;
      source.turns[0].steer = () => steering.promise;
      source.turns[0].cancel = () => { cancelCalls++; return cancellation.promise; };
      let correction, firstStop, secondStop, newMessage;
      await act(async () => {
        correction = controller.submit("older correction");
        firstStop = controller.cancel();
        secondStop = controller.cancel();
        newMessage = controller.submit("new message after Stop");
      });
      assert.equal(source.turns.length, 2, "a new message must not steer the cancelling turn");
      assert.equal(cancelCalls, 1, "repeated Stop clicks share one cancellation");
      await act(async () => {
        cancellation.resolve();
        await Promise.all([firstStop, secondStop, newMessage]);
        if (completion === "cancelled") {
          source.turns[0].fail(Object.assign(new Error("managed turn cancelled"), { code: "turn_cancelled" }));
        } else source.turns[0].complete("finished before cancellation arrived");
        steering.reject(Object.assign(new Error(`turn is ${completion}`), {
          status: 409, code: "turn_not_steerable", state: completion,
        }));
        await correction;
        source.turns[1].complete("new message completed");
      });
      await flushFrames(frames);
      assert.equal(source.turns.length, 2, "the old correction must never restart after Stop");
      assert.equal(controller.pendingTurns, 0);
      assert.equal(controller.entries.some((entry) => entry.kind === "error"), false);
      assert.ok(controller.entries.some((entry) => entry.text === "new message completed"));
    } finally {
      cancellation.resolve();
      steering.resolve();
      if (root) await act(async () => root.unmount());
      frames.restore();
    }
  });
}

test("a failed Stop can be retried without reviving an older correction", async () => {
  const frames = fakeAnimationFrames();
  const source = fakeAgent();
  let controller, root;
  function Consumer() { controller = useAgentController(source.agent); return null; }
  const steering = Promise.withResolvers();
  try {
    await act(async () => { root = create(createElement(Consumer)); });
    await act(async () => { await controller.submit("original"); });
    source.turns[0].steer = () => steering.promise;
    let calls = 0;
    source.turns[0].cancel = async () => {
      if (++calls === 1) throw new Error("temporary cancellation failure");
    };
    let correction;
    await act(async () => {
      correction = controller.submit("correction before Stop");
      assert.equal(await controller.cancel(), false);
      assert.equal(await controller.cancel(), true);
      source.turns[0].fail(Object.assign(new Error("managed turn cancelled"), { code: "turn_cancelled" }));
      steering.reject(Object.assign(new Error("turn cancelled"), { status: 409, code: "turn_not_steerable" }));
      await correction;
    });
    await flushFrames(frames);
    assert.equal(calls, 2);
    assert.equal(source.turns.length, 1);
    assert.equal(controller.pendingTurns, 0);
    assert.equal(controller.status, "Cancelled");
    assert.deepEqual(controller.entries.filter((entry) => entry.kind === "error").map((entry) => entry.text),
      ["temporary cancellation failure"]);
  } finally {
    steering.resolve();
    if (root) await act(async () => root.unmount());
    frames.restore();
  }
});

test("a correction still becomes a new turn when its target completes naturally", async () => {
  const frames = fakeAnimationFrames();
  const source = fakeAgent();
  let controller, root;
  function Consumer() { controller = useAgentController(source.agent); return null; }
  const steering = Promise.withResolvers();
  try {
    await act(async () => { root = create(createElement(Consumer)); });
    await act(async () => { await controller.submit("original"); });
    source.turns[0].steer = () => steering.promise;
    let correction;
    await act(async () => { correction = controller.submit("correction after completion"); });
    await act(async () => {
      source.turns[0].complete("original completed");
      steering.reject(Object.assign(new Error("turn completed"), { status: 409, code: "turn_not_steerable" }));
      await correction;
    });
    assert.equal(source.turns.length, 2);
    assert.equal(source.turns[1].input, "correction after completion");
  } finally {
    steering.resolve();
    if (root) await act(async () => root.unmount());
    frames.restore();
  }
});

test("a delayed steer rejection cannot start work after the controller detaches", async () => {
  const frames = fakeAnimationFrames();
  const source = fakeAgent();
  let controller, root;
  function Consumer() { controller = useAgentController(source.agent); return null; }
  const steering = Promise.withResolvers();
  try {
    await act(async () => { root = create(createElement(Consumer)); });
    await act(async () => { await controller.submit("original"); });
    source.turns[0].steer = () => steering.promise;
    let correction;
    await act(async () => { correction = controller.submit("correction before navigation"); });
    await act(async () => root.unmount());
    root = undefined;
    await act(async () => {
      steering.reject(Object.assign(new Error("turn completed"), { status: 409, code: "turn_not_steerable" }));
      await correction;
    });
    assert.equal(source.turns.length, 1);
  } finally {
    steering.resolve();
    if (root) await act(async () => root.unmount());
    frames.restore();
  }
});

test("retained history merges older pages by durable turn and exposes load state", async () => {
  const frames = fakeAnimationFrames();
  const source = fakeAgent();
  source.history = [
    event(1, "managed.prompt", { text: "recent", turn_id: "recent" }),
    event(2, "assistant.message", { text: "recent answer", turn_id: "recent" }),
  ];
  source.olderPages.push([
    event(1, "managed.prompt", { text: "older", turn_id: "older" }),
    event(2, "assistant.message", { text: "older answer", turn_id: "older" }),
    event(3, "managed.prompt", { text: "recent", turn_id: "recent" }),
    event(4, "assistant.message", { text: "recent answer", turn_id: "recent" }),
  ]);
  let controller;

  function Consumer() {
    controller = useAgentController(source.agent);
    return null;
  }

  let root;
  try {
    await act(async () => { root = create(createElement(Consumer)); });
    await flushFrames(frames);
    assert.deepEqual(controller.entries.filter(textEntry).map(({ text }) => text), [
      "recent", "recent answer",
    ]);
    assert.equal(controller.canLoadOlder, true);

    await act(async () => { assert.equal(await controller.loadOlder(), true); });
    await flushFrames(frames);
    assert.deepEqual(controller.entries.filter(textEntry).map(({ text }) => text), [
      "older", "older answer", "recent", "recent answer",
    ]);
    assert.equal(source.loadCalls, 1);

    await act(async () => { assert.equal(await controller.loadOlder(), false); });
    await flushFrames(frames);
    assert.equal(controller.canLoadOlder, false);
    await act(async () => root.unmount());
  } finally {
    frames.restore();
  }
});

test("retained history projects a repeated tool call once", async () => {
  const frames = fakeAnimationFrames();
  const source = fakeAgent();
  source.history = [
    event(1, "managed.prompt", { text: "use a tool", turn_id: "turn-1" }),
    event(2, "tool.call", {
      call_id: "call-retained", tool: "exec_command",
      arguments: { cmd: "pwd" }, turn_id: "turn-1",
    }),
    event(2, "tool.call", {
      call_id: "call-retained", tool: "exec_command",
      arguments: { cmd: "pwd" }, turn_id: "turn-1",
    }),
    event(3, "tool.result", {
      call_id: "call-retained", status: "completed", result: "done", turn_id: "turn-1",
    }),
  ];
  let controller;

  function Consumer() {
    controller = useAgentController(source.agent);
    return null;
  }

  let root;
  try {
    await act(async () => { root = create(createElement(Consumer)); });
    await flushFrames(frames);
    const tools = controller.entries.filter((entry) => entry.kind === "tool");
    assert.equal(tools.length, 1);
    assert.equal(tools[0].id, "tool-call-retained");
    assert.equal(tools[0].tool.status, "completed");
    await act(async () => root.unmount());
  } finally {
    frames.restore();
  }
});

test("hidden controllers reduce bursts and publish one visible catch-up snapshot", async () => {
  const frames = fakeAnimationFrames();
  const source = fakeAgent();
  let controller;
  let renders = 0;

  function Consumer({ visible }) {
    renders += 1;
    controller = useAgentController(source.agent, { visible });
    return null;
  }

  let root;
  try {
    await act(async () => { root = create(createElement(Consumer, { visible: false })); });
    assert.equal(frames.pending, 0);
    const hiddenRenders = renders;
    await act(async () => {
      source.emit(event(1, "run.started", { turn_id: "turn-1" }));
      for (let seq = 2; seq < 50; seq += 1) {
        source.emit(event(seq, "assistant.delta", { text: "x", turn_id: "turn-1" }));
      }
    });
    assert.equal(renders, hiddenRenders);
    assert.equal(controller.entries.length, 0);

    await act(async () => root.update(createElement(Consumer, { visible: true })));
    assert.equal(frames.pending, 1);
    const beforeCatchUp = renders;
    await flushFrames(frames);
    assert.equal(renders, beforeCatchUp + 1);
    assert.equal(controller.entries[0].text.length, 48);
    await act(async () => root.unmount());
  } finally {
    frames.restore();
  }
});

test("AgentController exposes the same stable controls through a render prop", async () => {
  const frames = fakeAnimationFrames();
  const source = fakeAgent();
  let first;
  let latest;
  let root;
  try {
    await act(async () => {
      root = create(createElement(AgentController, {
        agent: source.agent,
        children(snapshot) {
          first ??= snapshot;
          latest = snapshot;
          return null;
        },
      }));
    });
    await flushFrames(frames);
    assert.equal(first.submit, latest.submit);
    assert.equal(first.cancel, latest.cancel);
    await act(async () => root.unmount());
  } finally {
    frames.restore();
  }
});

function fakeAgent() {
  let eventListener = () => {};
  let historyListener = () => {};
  let offs = 0;
  let releases = 0;
  let loadCalls = 0;
  const turns = [];
  const olderPages = [];
  const source = {
    history: [],
    olderPages,
    turns,
    agent: {
      sessionId: "session",
      events: {
        watch() {
          return {
            onEvent(listener) {
              eventListener = listener;
              return () => { releases += 1; eventListener = () => {}; };
            },
            onHistory(listener) {
              historyListener = listener;
              listener(source.history);
              return () => { releases += 1; historyListener = () => {}; };
            },
            async loadOlder() {
              loadCalls += 1;
              const page = olderPages.shift();
              if (!page) return false;
              source.history = page;
              historyListener(page);
              return true;
            },
            off() { offs += 1; },
          };
        },
      },
      turn: {
        prompt({ input }) {
          let resolve;
          let reject;
          const pending = new Promise((yes, no) => { resolve = yes; reject = no; });
          const turn = {
            input,
            historyEntryId: `managed-user-turn-${turns.length + 1}`,
            steers: [],
            cancelled: false,
            disposals: 0,
            resultDisposals: 0,
            async steer({ input: steer }) { turn.steers.push(steer); },
            async cancel() { turn.cancelled = true; },
            result() { return pending; },
            dispose() { turn.disposals += 1; },
            complete(finalMessage) {
              resolve({
                finalMessage,
                dispose() { turn.resultDisposals += 1; },
              });
            },
            fail(error) { reject(error); },
          };
          turns.push(turn);
          return turn;
        },
      },
    },
    emit(next) { eventListener(next); },
    get loadCalls() { return loadCalls; },
    get offs() { return offs; },
    get releases() { return releases; },
  };
  return source;
}

function event(seq, type, payload = {}) {
  return { request_id: "session", seq, type, payload };
}

function textEntry(entry) {
  return "text" in entry;
}

function fakeAnimationFrames() {
  const requestDescriptor = Object.getOwnPropertyDescriptor(globalThis, "requestAnimationFrame");
  const cancelDescriptor = Object.getOwnPropertyDescriptor(globalThis, "cancelAnimationFrame");
  const callbacks = new Map();
  let nextFrame = 1;
  Object.defineProperty(globalThis, "requestAnimationFrame", {
    configurable: true,
    value(callback) {
      const frame = nextFrame++;
      callbacks.set(frame, callback);
      return frame;
    },
  });
  Object.defineProperty(globalThis, "cancelAnimationFrame", {
    configurable: true,
    value(frame) { callbacks.delete(frame); },
  });
  return {
    get pending() { return callbacks.size; },
    flush() {
      const pending = [...callbacks.values()];
      callbacks.clear();
      for (const callback of pending) callback(performance.now());
    },
    restore() {
      if (requestDescriptor) Object.defineProperty(globalThis, "requestAnimationFrame", requestDescriptor);
      else Reflect.deleteProperty(globalThis, "requestAnimationFrame");
      if (cancelDescriptor) Object.defineProperty(globalThis, "cancelAnimationFrame", cancelDescriptor);
      else Reflect.deleteProperty(globalThis, "cancelAnimationFrame");
    },
  };
}

async function flushFrames(frames) {
  await act(async () => { frames.flush(); });
}
