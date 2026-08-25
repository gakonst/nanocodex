import assert from "node:assert/strict";
import test from "node:test";
import React from "react";
import TestRenderer, { act } from "react-test-renderer";

import {
  AgentTerminalView,
  TerminalComposer,
  TerminalTranscriptSurface,
  interleaveTranscriptEntries,
  terminalComposerAction,
} from "../dist/index.js";

globalThis.IS_REACT_ACT_ENVIRONMENT = true;
globalThis.ResizeObserver = class {
  observe() {}
  disconnect() {}
};
globalThis.getComputedStyle = () => ({ lineHeight: "22px" });
globalThis.window = {
  cancelAnimationFrame() {},
  matchMedia: () => ({ matches: true }),
  requestAnimationFrame: () => 1,
};
globalThis.document = { activeElement: null, body: {} };

test("controller-backed terminal remains caller-owned when no Agent is attached", async () => {
  const states = [];
  let renderer;
  await act(async () => {
    renderer = TestRenderer.create(React.createElement(AgentTerminalView, {
      agent: undefined,
      agentError: undefined,
      mode: "preview",
      onConversationActivity() {},
      onStateChange(state) { states.push(state); },
      retryAgent() {},
    }), {
      createNodeMock(element) {
        return element.type === "div"
          ? { clientHeight: 300, firstElementChild: null, scrollHeight: 300, scrollTop: 0 }
          : {};
      },
    });
  });
  assert.equal(renderer.root.findByProps({ role: "log" }).props["aria-live"], "off");
  assert.equal(renderer.root.findByType("form").props["aria-label"], "Nanocodex message composer");
  assert.equal(renderer.root.findAllByProps({ "aria-label": "Start voice" }).length, 0);
  assert.equal(states.at(-1).status, "starting");
  await act(async () => renderer.update(React.createElement(AgentTerminalView, {
    agent: undefined,
    agentError: undefined,
    mode: "preview",
    onConversationActivity() {},
    onStateChange(state) { states.push(state); },
    retryAgent() {},
    voice: true,
  })));
  assert.equal(renderer.root.findByProps({ "aria-label": "Start voice" }).props.disabled, true);
  await act(async () => renderer.unmount());
});

test("composer keeps send stable while exposing stop separately", async () => {
  assert.equal(terminalComposerAction(true, ""), "stop");
  assert.equal(terminalComposerAction(true, "steer"), "send");
  const changes = [];
  const submissions = [];
  const textareaNode = { value: "ship it" };
  let cancelled = 0;
  let renderer;
  await act(async () => {
    renderer = TestRenderer.create(React.createElement(TerminalComposer, {
      draft: "ship it",
      pending: false,
      running: true,
      status: "ready",
      onCancel() { cancelled += 1; },
      onChange(value) { changes.push(value); },
      onSubmit(value) { submissions.push(value); },
    }), {
      createNodeMock(element) {
        return element.type === "textarea" ? textareaNode : {};
      },
    });
  });
  const form = renderer.root.findByType("form");
  await act(async () => form.props.onSubmit({ preventDefault() {} }));
  assert.deepEqual(submissions, ["ship it"]);
  const textarea = renderer.root.findByType("textarea");
  textareaNode.value = "live native input";
  let prevented = 0;
  await act(async () => textarea.props.onKeyDown({
    nativeEvent: {
      isComposing: false,
      key: "Enter",
      keyCode: 229,
      shiftKey: false,
    },
    preventDefault() { prevented += 1; },
  }));
  assert.deepEqual(submissions, ["ship it", "live native input"]);
  assert.equal(prevented, 1);
  await act(async () => textarea.props.onCompositionStart());
  await act(async () => textarea.props.onKeyDown({
    nativeEvent: {
      isComposing: false,
      key: "Enter",
      keyCode: 229,
      shiftKey: false,
    },
    preventDefault() { prevented += 1; },
  }));
  await act(async () => textarea.props.onCompositionEnd({ currentTarget: { value: "composed input" } }));
  assert.deepEqual(changes, ["composed input"]);
  assert.deepEqual(submissions, ["ship it", "live native input"]);
  assert.equal(prevented, 1);
  assert.deepEqual(
    renderer.root.findAllByType("button").map((button) => button.props["aria-label"]),
    ["Send message"],
  );
  assert.equal(cancelled, 0);

  await act(async () => renderer.update(React.createElement(TerminalComposer, {
    draft: "",
    pending: false,
    running: true,
    status: "ready",
    onCancel() { cancelled += 1; },
    onChange() {},
    onSubmit(value) { submissions.push(value); },
  })));
  assert.deepEqual(
    renderer.root.findAllByType("button").map((button) => button.props["aria-label"]),
    ["Stop response", "Send message"],
  );
  assert.equal(renderer.root.findByProps({ "aria-label": "Send message" }).props.disabled, false);
  await act(async () => renderer.root.findByProps({ "aria-label": "Stop response" }).props.onClick());
  assert.equal(cancelled, 1);
  await act(async () => renderer.unmount());
});

test("welcome is replaced by the first visible durable or voice entry", async () => {
  const props = {
    canLoadOlder: false,
    composer: null,
    entries: [],
    inactiveMessage: "",
    isLoadingOlder: false,
    mode: "full",
    status: "ready",
    welcome: "Welcome to Nanocodex",
    onLoadOlder: async () => false,
  };
  let renderer;
  await act(async () => {
    renderer = TestRenderer.create(React.createElement(TerminalTranscriptSurface, props), {
      createNodeMock(element) {
        return element.type === "div"
          ? { clientHeight: 300, firstElementChild: null, scrollHeight: 300, scrollTop: 0 }
          : {};
      },
    });
  });
  assert.equal(renderer.root.findAllByProps({ className: "agent-terminal-markdown is-assistant is-welcome" }).length, 1);

  await act(async () => renderer.update(React.createElement(TerminalTranscriptSurface, {
    ...props,
    entries: [{ id: "durable", kind: "assistant", text: "Ready", streaming: false }],
  })));
  assert.equal(renderer.root.findAllByProps({ className: "agent-terminal-markdown is-assistant is-welcome" }).length, 0);

  await act(async () => renderer.update(React.createElement(TerminalTranscriptSurface, {
    ...props,
    voiceEntries: [{
      id: "voice",
      kind: "user",
      source: "voice",
      streaming: false,
      text: "Hello",
    }],
  })));
  assert.equal(renderer.root.findAllByProps({ className: "agent-terminal-markdown is-assistant is-welcome" }).length, 0);
  assert.equal(renderer.root.findAllByProps({ "data-source": "voice" }).length, 1);
  await act(async () => renderer.unmount());
});

test("transcript renders semantic reasoning, plans, and nested tools", async () => {
  const entries = [
    { id: "r", kind: "reasoning", text: "checking", streaming: true },
    { id: "a", kind: "assistant", text: "**done**", streaming: false },
    { id: "p", kind: "plan", update: { plan: [{ step: "verify", status: "completed" }] } },
    {
      id: "t",
      kind: "tool",
      tool: {
        callId: "root", name: "shell", arguments: "{}", result: "ok", status: "completed",
        children: [{
          callId: "child", name: "read", arguments: "{}", status: "running", children: [],
        }],
      },
    },
  ];
  let renderer;
  await act(async () => {
    renderer = TestRenderer.create(React.createElement(TerminalTranscriptSurface, {
      canLoadOlder: false,
      composer: null,
      entries,
      inactiveMessage: "",
      isLoadingOlder: false,
      mode: "full",
      status: "ready",
      onLoadOlder: async () => false,
    }), {
      createNodeMock(element) {
        return element.type === "div"
          ? { clientHeight: 300, firstElementChild: null, scrollHeight: 600, scrollTop: 0 }
          : {};
      },
    });
  });
  const labels = renderer.root.findAllByProps({ className: "agent-terminal-entry-label" });
  assert.equal(labels[0].children.join(""), "thinking…");
  assert.equal(renderer.root.findAllByType("li")[0].children[1], "verify");
  assert.deepEqual(
    renderer.root.findAllByType("header").map((header) => header.children.at(-1)),
    ["shell", "read"],
  );
  assert.equal(renderer.root.findAllByProps({ className: "agent-terminal-brand" }).length, 0);
  await act(async () => renderer.update(React.createElement(TerminalTranscriptSurface, {
    canLoadOlder: false,
    composer: null,
    entries,
    followTailRequest: 1,
    inactiveMessage: "",
    isLoadingOlder: false,
    mode: "full",
    showToolCalls: false,
    status: "ready",
    onLoadOlder: async () => false,
  })));
  assert.equal(renderer.root.findAllByType("header").length, 0);
  await act(async () => renderer.unmount());
});

test("voice transcripts interleave with durable entries", async () => {
  const entries = [
    { id: "before", kind: "assistant", text: "Ready", streaming: false },
    { id: "prompt", kind: "user", text: "ship the release" },
    { id: "result", kind: "assistant", text: "Shipped", streaming: false },
  ];
  const voiceEntries = [
    {
      afterEntryId: "before",
      id: "voice-user",
      kind: "user",
      source: "voice",
      streaming: false,
      text: "ship   the release",
    },
    {
      afterEntryId: "result",
      id: "voice-assistant",
      kind: "assistant",
      source: "voice",
      streaming: false,
      text: "All done",
    },
  ];
  assert.deepEqual(
    interleaveTranscriptEntries(entries, voiceEntries).map((entry) => entry.id),
    ["before", "voice-user", "prompt", "result", "voice-assistant"],
  );
  assert.deepEqual(
    interleaveTranscriptEntries(entries, [{ ...voiceEntries[0], afterEntryId: "prompt" }])
      .map((entry) => entry.id),
    ["before", "prompt", "voice-user", "result"],
  );
  assert.deepEqual(
    interleaveTranscriptEntries([entries[2]], [{ ...voiceEntries[0], afterEntryId: "expired" }])
      .map((entry) => entry.id),
    ["result"],
  );

  let renderer;
  await act(async () => {
    renderer = TestRenderer.create(React.createElement(TerminalTranscriptSurface, {
      canLoadOlder: false,
      composer: null,
      entries,
      inactiveMessage: "",
      isLoadingOlder: false,
      mode: "full",
      status: "ready",
      voiceEntries,
      onLoadOlder: async () => false,
    }), {
      createNodeMock(element) {
        return element.type === "div"
          ? { clientHeight: 300, firstElementChild: null, scrollHeight: 600, scrollTop: 0 }
          : {};
      },
    });
  });
  assert.equal(renderer.root.findAllByProps({ "data-source": "voice" }).length, 2);
  assert.deepEqual(
    renderer.root.findAllByProps({ className: "agent-terminal-entry-label" })
      .map((label) => label.children.join("")),
    ["voice", "voice"],
  );
  await act(async () => renderer.unmount());
});

test("durable realtime handoffs project spoken history instead of internal markup", () => {
  const delegation = {
    id: "delegation",
    kind: "user",
    text: `<realtime_delegation>
  <input>Continue the task</input>
  <transcript_delta>user: ship &amp; verify
assistant: on it</transcript_delta>
</realtime_delegation>`,
  };
  const durable = interleaveTranscriptEntries([delegation], []);
  assert.deepEqual(
    durable.map(({ id, kind, text }) => ({ id, kind, text })),
    [
      { id: "delegation-voice-0", kind: "user", text: "ship & verify" },
      { id: "delegation-voice-1", kind: "assistant", text: "on it" },
    ],
  );
  assert.deepEqual(
    interleaveTranscriptEntries([delegation], [{
      afterEntryId: "delegation",
      id: "live-user",
      kind: "user",
      source: "voice",
      streaming: false,
      text: "ship & verify",
    }]).map((entry) => entry.id),
    ["live-user", "delegation-voice-1"],
  );
  assert.deepEqual(
    interleaveTranscriptEntries([{
      ...delegation,
      text: `<realtime_delegation>
  <input>Continue the task</input>
  <transcript_delta>…retained transcript tail</transcript_delta>
</realtime_delegation>`,
    }], []).map(({ kind, text }) => ({ kind, text })),
    [{ kind: "assistant", text: "…retained transcript tail" }],
  );
});
