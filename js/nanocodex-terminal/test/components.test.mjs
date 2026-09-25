import assert from "node:assert/strict";
import test from "node:test";
import React from "react";
import TestRenderer, { act } from "react-test-renderer";

import {
  AgentTerminalView,
  ElevenLabsSettings,
  GeneratedOutputView,
  TerminalComposer,
  TerminalTranscriptSurface,
  interleaveTranscriptEntries,
} from "../dist/index.js";
import { VoiceControl } from "../dist/AgentTerminalView.js";

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

test("hiding a full terminal retains its transcript and accessory state", async () => {
  let mounts = 0;
  function Accessory() {
    const [count, setCount] = React.useState(0);
    React.useEffect(() => { mounts++; }, []);
    return React.createElement("button", { onClick: () => setCount(count + 1) }, `Count ${count}`);
  }
  const props = {
    mode: "full",
    onConversationActivity() {},
    onStateChange() {},
    retryAgent() {},
    accessory: () => React.createElement(Accessory),
  };
  let renderer;
  await act(async () => {
    renderer = TestRenderer.create(React.createElement(AgentTerminalView, props), {
      createNodeMock(element) {
        return element.type === "div"
          ? { clientHeight: 300, firstElementChild: null, scrollHeight: 300, scrollTop: 0 }
          : {};
      },
    });
  });
  const transcript = renderer.root.findByType(TerminalTranscriptSurface);
  await act(async () => renderer.root.findByType(Accessory).findByType("button").props.onClick());
  for (const mode of ["hidden", "full"]) {
    await act(async () => renderer.update(React.createElement(AgentTerminalView, { ...props, mode })));
    assert.equal(renderer.root.findByType(TerminalTranscriptSurface), transcript);
    assert.equal(renderer.root.findByType(Accessory).findByType("button").children.join(""), "Count 1");
  }
  assert.equal(mounts, 1);
  await act(async () => renderer.unmount());
});

function voiceSnapshot(overrides = {}) {
  return {
    error: undefined,
    status: "idle",
    statusText: undefined,
    muted: false, microphoneLevel: 0, speakerLevel: 0,
    setMuted() {}, toggleMuted() {}, noteTypedInput: async () => {},
    transcripts: [],
    voice: undefined,
    isActive: false,
    isConnecting: false,
    isError: false,
    isIdle: true,
    cancel: async () => false,
    start: async () => {},
    stop: async () => {},
    toggle: async () => {},
    ...overrides,
  };
}

test("voice preferences reconnect an active call with the saved subscription settings", async () => {
  const calls = [];
  let renderer;
  await act(async () => {
    renderer = TestRenderer.create(React.createElement(VoiceControl, {
      agentReady: true,
      voice: voiceSnapshot({ isActive: true, voice: "cove",
        stop: async () => { calls.push("stop"); },
        start: async (settings) => { calls.push(settings); },
      }),
    }));
  });
  await act(async () => renderer.root.findByProps({ "aria-label": "Voice settings" }).props.onClick());
  const group = renderer.root.findByProps({ "aria-label": "Voice preferences" });
  const select = (label) => group.findAllByType("label").find((node) => node.children[0] === label).findByType("select");
  await act(async () => select("Voice for this call").props.onChange({ target: { value: "maple" } }));
  await act(async () => select("Pace").props.onChange({ target: { value: "fast" } }));
  await act(async () => select("Spoken updates").props.onChange({ target: { value: "results" } }));
  await act(async () => select("Acknowledge requests").props.onChange({ target: { value: "false" } }));
  await act(async () => group.findByType("textarea").props.onChange({ target: { value: "bad\0text" } }));
  await act(async () => group.findByProps({ "aria-label": "Save voice settings" }).props.onClick());
  assert.equal(calls.length, 0, "Invalid preferences must not interrupt a call");
  assert.ok(group.findByProps({ role: "alert" }));
  const instructions = "Speak Greek. ".repeat(400);
  await act(async () => group.findByType("textarea").props.onChange({ target: { value: instructions } }));
  await act(async () => group.findByProps({ "aria-label": "Save voice settings" }).props.onClick());
  assert.deepEqual(calls, ["stop", { voice: "maple", pace: "fast", updates: "results", acknowledgements: false, instructions }]);
  assert.equal(renderer.root.findAllByProps({ "aria-label": "Voice preferences" }).length, 0);
  await act(async () => renderer.unmount());
});

test("composer keeps stop available beside send throughout an active turn", async () => {
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
    ["Stop response", "Send message"],
  );
  await act(async () => renderer.root.findByProps({ "aria-label": "Stop response" }).props.onClick());
  assert.equal(cancelled, 1);

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
  assert.equal(renderer.root.findByProps({ "aria-label": "Send message" }).props.disabled, true);
  await act(async () => renderer.root.findByProps({ "aria-label": "Stop response" }).props.onClick());
  assert.equal(cancelled, 2);
  await act(async () => renderer.unmount());
});

test("automatic history keeps the reader anchored while output streams and the reader moves", async () => {
  let prependHeight = 0;
  const viewport = {
    clientHeight: 300, scrollHeight: 1000, scrollTop: 0,
    getBoundingClientRect: () => ({ top: 0, bottom: 300 }),
    contains: (row) => rows.includes(row),
  };
  const rows = [0, 200, 400, 600, 800].map((top) => ({
    isConnected: true,
    getBoundingClientRect: () => ({
      top: top + prependHeight - viewport.scrollTop,
      bottom: top + prependHeight + 200 - viewport.scrollTop,
    }),
  }));
  viewport.firstElementChild = { children: rows };
  let resolveOlder;
  let requests = 0;
  let props = {
    canLoadOlder: true,
    composer: null,
    entries: [{ id: "first", kind: "user", text: "Earlier" }, { id: "tail", kind: "assistant", text: "Still writing", streaming: true }],
    voiceEntries: [{ id: "live-voice", kind: "user", source: "voice", text: "Live voice before durable history", streaming: true }],
    inactiveMessage: "", isLoadingOlder: false, mode: "full", status: "ready",
    onLoadOlder() {
      requests++;
      return new Promise((resolve) => { resolveOlder = resolve; });
    },
  };
  let renderer;
  await act(async () => {
    renderer = TestRenderer.create(React.createElement(TerminalTranscriptSurface, props), {
      createNodeMock: (element) => element.props.className === "agent-dom-transcript" ? viewport : {},
    });
  });
  const scroll = async (top) => {
    viewport.scrollTop = top;
    await act(async () => renderer.root.findByProps({ role: "log" }).props.onScroll({ currentTarget: viewport }));
  };
  assert.equal(requests, 0, "opening the tail must not fetch the entire history");
  await scroll(500);
  await scroll(200);
  assert.equal(requests, 1);
  await scroll(500);
  await scroll(200);
  assert.equal(requests, 1, "a delayed loading prop must not permit a second request to replace the anchor");
  props = { ...props, isLoadingOlder: true, entries: [props.entries[0], { ...props.entries[1], text: "Still writing more output" }] };
  viewport.scrollHeight += 100;
  await act(async () => renderer.update(React.createElement(TerminalTranscriptSurface, props)));
  assert.equal(viewport.scrollTop, 200, "streaming below the reader must not consume the pending history anchor");
  await scroll(160);
  await act(async () => resolveOlder(true));
  assert.equal(viewport.scrollTop, 160, "promise resolution can precede the controller's frame-published history");
  prependHeight = 400;
  viewport.scrollHeight += prependHeight;
  props = { ...props, entries: [{ id: "older", kind: "user", text: "Oldest" }, ...props.entries] };
  await act(async () => renderer.update(React.createElement(TerminalTranscriptSurface, props)));
  assert.equal(viewport.scrollTop, 560, "preserve where the reader moved while the page was in flight");
  assert.equal(viewport.scrollTop, 560, "settling an already rendered page must not scroll twice");
  assert.equal(requests, 1);
  await act(async () => renderer.unmount());
});

test("short history pages respond to upward gestures without initial fetches or failure loops", async () => {
  const viewport = {
    clientHeight: 300, scrollHeight: 300, scrollTop: 0,
    firstElementChild: { children: [] },
    getBoundingClientRect: () => ({ top: 0, bottom: 300 }),
  };
  let requests = 0;
  let fail = false;
  let renderer;
  await act(async () => {
    renderer = TestRenderer.create(React.createElement(TerminalTranscriptSurface, {
      canLoadOlder: true, composer: null, entries: [{ id: "first", kind: "user", text: "Short page" }],
      inactiveMessage: "", isLoadingOlder: false, mode: "full", status: "ready",
      async onLoadOlder() { requests++; if (fail) throw new Error("Offline"); return true; },
    }), {
      createNodeMock: (element) => element.props.className === "agent-dom-transcript" ? viewport : {},
    });
  });
  viewport.scrollTop = 0;
  const log = () => renderer.root.findByProps({ role: "log" });
  await act(async () => log().props.onScroll({ currentTarget: viewport }));
  assert.equal(requests, 0);
  await act(async () => log().props.onWheel({ currentTarget: viewport, deltaY: -20 }));
  assert.equal(requests, 1, "even a page shorter than the viewport accepts an upward gesture");
  await act(async () => log().props.onWheel({ currentTarget: viewport, deltaY: -20 }));
  assert.equal(requests, 2, "a successful short page permits another deliberate request");
  fail = true;
  await act(async () => log().props.onWheel({ currentTarget: viewport, deltaY: -20 }));
  await act(async () => log().props.onWheel({ currentTarget: viewport, deltaY: -20 }));
  assert.equal(requests, 3, "failed pages must not retry on every wheel event");
  await act(async () => log().props.onWheel({ currentTarget: viewport, deltaY: 20 }));
  await act(async () => {
    log().props.onTouchStart({ touches: [{ clientY: 100 }] });
    log().props.onTouchMove({ currentTarget: viewport, touches: [{ clientY: 140 }] });
  });
  assert.equal(requests, 4, "gesturing away and back permits retry even when short content cannot scroll");
  await act(async () => renderer.unmount());
});

test("generated code output stays visible outside activity, deduplicates child media, and survives hidden tool details", async () => {
  const image = { kind: "image", url: "data:image/png;base64,AA==", name: "Chart.png", mimeType: "image/png" };
  const props = {
    canLoadOlder: false, composer: null, inactiveMessage: "", isLoadingOlder: false,
    mode: "full", status: "ready", onLoadOlder: async () => false,
    entries: [{ id: "generated", kind: "tool", tool: {
      callId: "exec", name: "exec", arguments: "image(result)", status: "completed",
      generatedOutput: [image, { kind: "text", text: "**Generated report**" },
        { kind: "audio", url: "data:audio/wav;base64,UklGRg==", name: "Audio.wav" },
        { kind: "file", url: "data:text/csv;charset=utf-8,item%2Ccount%0AA%2C2", name: "Report.csv" }],
      children: [{ callId: "exec/code-1", name: "view_image", arguments: "chart.png", status: "completed", generatedOutput: [image], children: [] }],
    } }],
  };
  let renderer;
  await act(async () => {
    renderer = TestRenderer.create(React.createElement(TerminalTranscriptSurface, props), {
      createNodeMock: () => ({ clientHeight: 300, firstElementChild: null, scrollHeight: 300, scrollTop: 0 }),
    });
  });
  assert.equal(renderer.root.findAllByType("img").length, 1);
  const img = renderer.root.findByType("img");
  for (let parent = img.parent; parent; parent = parent.parent) assert.notEqual(parent.type, "details");
  assert.equal(renderer.root.findByProps({ "data-streamdown": "strong" }).children.join(""), "Generated report");
  assert.equal(renderer.root.findByType("audio").props.controls, true);
  assert.equal(renderer.root.findByProps({ "aria-label": "Download Report.csv" }).props.download, "Report.csv");
  await act(async () => renderer.update(React.createElement(TerminalTranscriptSurface, { ...props, showToolCalls: false })));
  assert.equal(renderer.root.findAllByType("details").length, 0);
  assert.equal(renderer.root.findAllByType("img").length, 1);
  assert.equal(renderer.root.findAllByType("audio").length, 1);
  await act(async () => renderer.unmount());
});

test("outer exec text arriving before child media retains the playing media element", async () => {
  const audio = { kind: "audio", url: "data:audio/wav;base64,UklGRg==", name: "Audio.wav" };
  let renderer;
  await act(async () => { renderer = TestRenderer.create(React.createElement(GeneratedOutputView, { items: [audio] })); });
  const playingElement = renderer.root.findByType("audio");
  await act(async () => renderer.update(React.createElement(GeneratedOutputView, {
    items: [{ kind: "text", text: "Generated audio" }, { ...audio }],
  })));
  assert.equal(renderer.root.findByType("audio"), playingElement);
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

test("voice projection hides lifecycle and incomplete envelopes and retains input-only speech", () => {
  const project = (text) => interleaveTranscriptEntries([{ id: "voice", kind: "user", text }], []);
  assert.equal(project('\n<realtime_delegation><input>Ship &quot;it&quot; &amp; keep &amp;lt;</input></realtime_delegation>')[0].text, 'Ship "it" & keep &lt;');
  for (const text of [
    "<realtime_delegation><source>transcript_tail_flush</source><input>Synthetic tail instruction</input></realtime_delegation>",
    "<realtime_delegation><soruce>transcript_tail_flush</soruce><input>Synthetic tail instruction</input></realtime_delegation>",
    "  <realtime_delegation><transcript_delta>unfinished",
    "<realtime_conversation>Internal mode instructions</realtime_conversation>",
    "<startup_context>Internal startup</startup_context>",
    "<source>Internal metadata</source>",
    "<soruce>Internal metadata</soruce>",
    "<realtime_delegation><input>Hidden</input><transcript_delta>unfinished",
  ]) assert.deepEqual(project(text), []);
  assert.equal(project("<realtime_delegation><source>voice_bootstrap</source><input>Hello</input></realtime_delegation>")[0].text, "Hello");
  for (const text of ["Explain <realtime_delegation>", "Use <source> here", "2 < 3 & 4 > 1"]) {
    assert.equal(project(text)[0].text, text);
  }
});

test("transcript keeps partial and final assistant text visible with tool activity hidden", async () => {
  const { applyAgentEvents, initialState } = await import("../../nanocodex-react/agent/transcript.mjs");
  let state = initialState();
  let renderer;
  const props = { canLoadOlder: false, composer: null, inactiveMessage: "", isLoadingOlder: false, mode: "full", showToolCalls: false, status: "running", onLoadOlder: async () => false };
  const project = async (seq, type, text) => {
    state = applyAgentEvents(state, [{ protocol_version: 1, request_id: "session", seq, type, payload: { text, turn_id: "turn" } }]);
    await act(async () => {
      const element = React.createElement(TerminalTranscriptSurface, { ...props, entries: state.entries });
      if (renderer) renderer.update(element);
      else renderer = TestRenderer.create(element, { createNodeMock: () => ({ clientHeight: 300, scrollHeight: 600, scrollTop: 0 }) });
    });
  };
  try {
    await project(1, "assistant.delta", "Partial");
    assert.match(JSON.stringify(renderer.toJSON()), /Partial/);
    assert.equal(renderer.root.findAllByType("details").length, 0);
    assert.equal(renderer.root.findAllByProps({ className: "agent-terminal-markdown is-assistant" }).length, 1);
    assert.equal(state.entries[0].streaming, true);
    await project(2, "assistant.delta", " answer");
    assert.match(JSON.stringify(renderer.toJSON()), /Partial answer/);
    await project(3, "assistant.message", "Completed answer");
    const rendered = JSON.stringify(renderer.toJSON());
    assert.match(rendered, /Completed answer/);
    assert.doesNotMatch(rendered, /Partial answer/);
    assert.equal(renderer.root.findAllByProps({ className: "agent-terminal-markdown is-assistant" }).length, 1);
    assert.equal(state.entries.length, 1);
    assert.equal(state.entries[0].streaming, false);
  } finally {
    if (renderer) await act(async () => renderer.unmount());
  }
});

test("client-owned tool forms render nested requests even when tool details are hidden", async () => {
  const child = { callId: "intake", name: "request_vault_intake", status: "completed", arguments: "", children: [], output: '{"type":"vault_intake"}' };
  const parent = { ...child, callId: "exec", name: "exec", children: [child] };
  let renderer;
  await act(async () => {
    renderer = TestRenderer.create(React.createElement(TerminalTranscriptSurface, {
      canLoadOlder: false, composer: null, entries: [{ id: "exec", kind: "tool", tool: parent }],
      inactiveMessage: "", isLoadingOlder: false, mode: "full", showToolCalls: false, status: "ready", onLoadOlder: async () => false,
      renderTool: tool => tool.name === "request_vault_intake" ? React.createElement("button", { "data-intake": tool.callId }, "Open secure form") : null,
    }), { createNodeMock: () => ({ clientHeight: 300, scrollHeight: 600, scrollTop: 0 }) });
  });
  assert.equal(renderer.root.findAllByProps({ "data-intake": "intake" }).length, 1);
  assert.equal(renderer.root.findAllByType("details").length, 0);
  await act(async () => renderer.unmount());
});

test("ElevenLabs preferences select provider and voice before reconnect", async () => {
  const calls = [];
  let renderer;
  await act(async () => { renderer = TestRenderer.create(React.createElement(VoiceControl, {
    agentReady: true,
    elevenLabsManager: { listVoices: async () => [{ voiceId: "sample", name: "Sample" }] },
    voice: voiceSnapshot({ isActive: true, stop: async () => calls.push("stop"), start: async settings => calls.push(settings) }),
  })); });
  await act(async () => renderer.root.findByProps({ "aria-label": "Voice settings" }).props.onClick());
  const provider = renderer.root.findAllByType("label").find(node => node.children[0] === "Speech provider").findByType("select");
  await act(async () => provider.props.onChange({ target: { value: "elevenlabs" } }));
  await act(async () => renderer.root.findByProps({ "aria-label": "Save voice settings" }).props.onClick());
  assert.equal(calls.length, 0);
  await act(async () => renderer.root.findByProps({ "aria-label": "ElevenLabs voice" }).props.onChange({ target: { value: "sample" } }));
  await act(async () => renderer.root.findByProps({ "aria-label": "Save voice settings" }).props.onClick());
  assert.equal(calls[0], "stop");
  assert.equal(calls[1].outputProvider, "elevenlabs");
  assert.equal(calls[1].elevenLabsVoiceId, "sample");
  await act(async () => renderer.unmount());
});


test("clone verification leaves current voice unchanged and clears recordings", async () => {
  const selected = [];
  let renderer;
  await act(async () => { renderer = TestRenderer.create(React.createElement(ElevenLabsSettings, {
    voiceId: "existing", onSelect: id => selected.push(id), manager: {
      listVoices: async () => [],
      cloneVoice: async input => { assert.equal(input.consent, true); return { voiceId: "clone", name: input.name, requiresVerification: true }; },
    },
  })); });
  const root = renderer.root;
  await act(async () => root.findAllByType("input").find(node => node.props.maxLength === 100).props.onChange({ target: { value: "Sample" } }));
  await act(async () => root.findByProps({ type: "file" }).props.onChange({ target: { files: [new File(["audio"], "sample.wav", { type: "audio/wav" })] } }));
  const cloneButton = () => root.findAllByType("button").find(node => node.children[0] === "Create voice clone");
  assert.equal(cloneButton().props.disabled, true);
  await act(async () => root.findByProps({ type: "checkbox" }).props.onChange({ target: { checked: true } }));
  await act(async () => cloneButton().props.onClick());
  assert.deepEqual(selected, []);
  assert.match(root.findByProps({ role: "status" }).children.join(""), /Complete verification/);
  assert.equal(root.findByProps({ type: "checkbox" }).props.checked, false);
  await act(async () => renderer.unmount());
});

test("child JSON is disclosed separately while root JSON survives live and replay", async () => {
  const { applyAgentEvents, initialState } = await import("../../nanocodex-react/agent/transcript.mjs");
  const events = [
    ["assistant.delta", '{"answer":', {}],
    ["assistant.delta", '{"report":', { managed_agent_id: 7 }],
    ["assistant.message", '{"report":"child"}', { managed_agent_id: 7 }],
    ["assistant.message", '{"answer":"root"}', {}],
  ].map(([type, text, identity], seq) => ({ protocol_version: 1, request_id: "session", seq, type, payload: { turn_id: "turn", text, ...identity } }));
  const replay = applyAgentEvents(initialState(), events);
  const live = events.reduce((state, event) => applyAgentEvents(state, [event]), initialState());
  assert.deepEqual(live.entries, replay.entries);
  for (const state of [live, replay]) {
    let renderer;
    await act(async () => {
      renderer = TestRenderer.create(React.createElement(TerminalTranscriptSurface, {
        entries: state.entries, canLoadOlder: false, composer: null, inactiveMessage: "", isLoadingOlder: false,
        mode: "full", status: "ready", onLoadOlder: async () => false,
      }), { createNodeMock: () => ({ clientHeight: 300, scrollHeight: 600, scrollTop: 0 }) });
    });
    try {
      const child = renderer.root.findByProps({ "data-agent-id": 7 });
      assert.equal(child.type, "details");
      assert.equal(child.props.open, undefined);
      assert.equal(child.findByType("summary").children.join(""), "Agent 7 activity");
      assert.ok(child.findAll(node => node.props.children === '{"report":"child"}').length > 0);
      const root = renderer.root.findAllByType("article").find(article => article.findAll(node => node.props.children === '{"answer":"root"}').length > 0);
      assert.ok(root);
      assert.match(JSON.stringify(renderer.toJSON()), /root/);
      assert.equal(state.entries.filter(entry => entry.responseIdentity?.agentId === 7).length, 1);
    } finally { await act(async () => renderer.unmount()); }
  }
});

test("optimistic creation draft is editable before the agent attaches and survives readiness props", async () => {
  let renderer;
  const props = {
    agent: undefined,
    agentError: undefined,
    initialDraft: "I have an idea",
    mode: "preview",
    onConversationActivity() {},
    onStateChange() {},
    retryAgent() {},
  };
  await act(async () => {
    renderer = TestRenderer.create(React.createElement(AgentTerminalView, props), {
      createNodeMock(element) {
        return element.type === "div"
          ? { clientHeight: 300, firstElementChild: null, scrollHeight: 300, scrollTop: 0 }
          : {};
      },
    });
  });
  const input = () => renderer.root.findByType("textarea");
  assert.equal(input().props.value, "I have an idea");
  assert.equal(renderer.root.findByProps({ "aria-label": "Send message" }).props.disabled, true);
  await act(async () => input().props.onChange({ currentTarget: { value: "I have a better idea" } }));
  await act(async () => renderer.update(React.createElement(AgentTerminalView, {
    ...props, initialDraft: "stale optimistic text",
  })));
  assert.equal(input().props.value, "I have a better idea", "the handoff never overwrites edits");
  await act(async () => renderer.unmount());
});
