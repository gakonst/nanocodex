import assert from "node:assert/strict";
import test from "node:test";
import React from "react";
import TestRenderer, { act } from "react-test-renderer";

import {
  AgentTerminalView,
  ConversationHistoryRail,
  TerminalComposer,
  TerminalTranscriptSurface,
  interleaveTranscriptEntries,
  terminalComposerAction,
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

test("conversation rail owns selection and creation controls without duplicating ids", async () => {
  const selected = [];
  let created = 0;
  let renderer;
  const props = {
    agentStatus: "ready",
    conversations: [{ id: "one", title: "First", turnCount: 2 }],
    mobileOpen: false,
    onClose() {},
    onCreate() { created += 1; },
    onOpen() {},
    onRetry() {},
    onSelect(id) { selected.push(id); },
    pending: false,
    runtime: "managed",
    selectedId: "one",
  };
  await act(async () => {
    renderer = TestRenderer.create(React.createElement("main", null,
      React.createElement(ConversationHistoryRail, props),
      React.createElement(ConversationHistoryRail, props),
    ));
  });
  const labelledIds = renderer.root.findAllByType("aside").map((node) => node.props["aria-labelledby"]);
  assert.equal(new Set(labelledIds).size, 2);
  assert.equal(renderer.root.findAllByProps({ "aria-current": "location" }).length, 2);
  await act(async () => renderer.root.findAllByProps({ "aria-label": "New conversation" })[0].props.onClick());
  await act(async () => renderer.root.findAllByProps({ "aria-current": "location" })[0].props.onClick());
  assert.equal(created, 1);
  assert.deepEqual(selected, ["one"]);
  await act(async () => renderer.unmount());
});

test("controller-backed terminal remains caller-owned when no Agent is attached", async () => {
  const states = [];
  let renderer;
  await act(async () => {
    renderer = TestRenderer.create(React.createElement(AgentTerminalView, {
      agent: undefined,
      agentError: undefined,
      mode: "preview",
      composerPlaceholder: "Ask your agent",
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
  assert.equal(renderer.root.findByType("textarea").props.placeholder, "Ask your agent");
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

test("caller can lock the composer without remounting the controller-backed terminal", async () => {
  let renderer;
  const props = {
    agent: undefined,
    agentError: undefined,
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
  assert.equal(renderer.root.findAllByType("form").length, 1);
  await act(async () => renderer.update(React.createElement(AgentTerminalView, {
    ...props,
    composer: React.createElement("div", { "data-trial-exhausted": true }, "Connect or fund"),
  })));
  assert.equal(renderer.root.findAllByType("form").length, 0);
  assert.equal(renderer.root.findByProps({ "data-trial-exhausted": true }).children.join(""), "Connect or fund");
  assert.equal(renderer.root.findAllByProps({ role: "log" }).length, 1);
  await act(async () => renderer.unmount());
});

function voiceSnapshot(overrides = {}) {
  return {
    error: undefined,
    status: "idle",
    statusText: undefined,
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

test("ready voice control separates transport, coding-turn cancel, status, and failure actions", async () => {
  const calls = [];
  let renderer;
  const idle = voiceSnapshot({
    toggle: async () => { calls.push("start"); },
  });
  await act(async () => {
    renderer = TestRenderer.create(React.createElement(VoiceControl, {
      agentReady: true,
      voice: idle,
    }));
  });
  assert.deepEqual(
    renderer.root.findAllByType("button").map((button) => button.props["aria-label"]),
    ["Start voice"],
  );
  await act(async () => renderer.root.findByProps({ "aria-label": "Start voice" }).props.onClick());
  assert.deepEqual(calls, ["start"]);

  const connecting = voiceSnapshot({
    status: "connecting",
    isConnecting: true,
    isIdle: false,
    toggle: async () => { calls.push("stop"); },
  });
  await act(async () => renderer.update(React.createElement(VoiceControl, {
    agentReady: true,
    voice: connecting,
  })));
  assert.equal(renderer.root.findByProps({ "aria-label": "Stop voice" }).props["aria-pressed"], true);
  assert.equal(renderer.root.findAllByProps({ "aria-label": "Cancel voice turn" }).length, 0);

  let cancelled = 0;
  const active = voiceSnapshot({
    status: "active",
    statusText: "Voice connected — tap once to enable speaker audio",
    voice: "cove",
    isActive: true,
    isIdle: false,
    toggle: async () => { calls.push("stop"); },
    cancel: async () => { cancelled += 1; return true; },
  });
  await act(async () => renderer.update(React.createElement(VoiceControl, {
    agentReady: true,
    voice: active,
  })));
  assert.equal(renderer.root.findByProps({ "aria-label": "Stop voice" }).props["aria-pressed"], true);
  await act(async () => renderer.root.findByProps({ "aria-label": "Stop voice" }).props.onClick());
  await act(async () => renderer.root.findByProps({ "aria-label": "Cancel voice turn" }).props.onClick());
  assert.deepEqual(calls, ["start", "stop"]);
  assert.equal(cancelled, 1);
  assert.equal(renderer.root.findByProps({ role: "status" }).children.join(""), active.statusText);

  const error = new Error("Microphone permission denied — allow access and retry");
  await act(async () => renderer.update(React.createElement(VoiceControl, {
    agentReady: true,
    voice: voiceSnapshot({
      status: "error",
      statusText: error.message,
      error,
      isError: true,
    }),
  })));
  assert.equal(renderer.root.findByProps({ role: "alert" }).children.join(""), error.message);
  assert.equal(renderer.root.findAllByProps({ role: "status" }).length, 0);
  assert.equal(renderer.root.findByProps({ "aria-label": "Start voice" }).props.disabled, false);

  await act(async () => renderer.update(React.createElement(VoiceControl, {
    agentReady: true,
    voice: voiceSnapshot(),
  })));
  assert.equal(renderer.root.findAllByProps({ role: "status" }).length, 0);
  assert.equal(renderer.root.findAllByProps({ role: "alert" }).length, 0);
  await act(async () => renderer.unmount());
});

test("composer keeps stop available beside send throughout an active turn", async () => {
  assert.equal(terminalComposerAction(true, ""), "stop");
  assert.equal(terminalComposerAction(true, "steer"), "stop");
  assert.equal(terminalComposerAction(false, "steer"), "send");
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

test("transcript renders semantic reasoning, plans, and accessible nested tools", async () => {
  const entries = [
    { id: "r", kind: "reasoning", text: "checking", streaming: true },
    {
      id: "a",
      kind: "assistant",
      text: "**done** [Authorize Google](https://accounts.google.com/o/oauth2/v2/auth?state=opaque)",
      streaming: false,
    },
    { id: "p", kind: "plan", update: { plan: [{ step: "verify", status: "completed" }] } },
    {
      id: "t",
      kind: "tool",
      tool: {
        callId: "root", name: "exec", arguments: "text(await tools.sandbox_exec(...))",
        result: "{\"content\":[{\"type\":\"text\"}]}", status: "completed",
        children: [{
          callId: "child", name: "sandbox_exec",
          arguments: "{\"command\":\"pwd\",\"cwd\":\"/workspace\"}",
          result: "{\"exit_code\":0,\"stdout\":\"/workspace\",\"stderr\":\"\"}",
          status: "completed", children: [],
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
  assert.deepEqual(renderer.root.findAllByType("strong").map((strong) => strong.children.join("")), ["Run code", "Run command"]);
  assert.deepEqual(renderer.root.findAllByProps({ className: "agent-terminal-tool-status" })
    .map((status) => status.children.join("")), ["Succeeded", "Succeeded"]);
  assert.deepEqual(renderer.root.findAllByProps({ className: "agent-terminal-tool-source" })
    .map((source) => source.children.join("")), ["Code mode", "Sandbox · /workspace"]);
  assert.equal(
    renderer.root.findByProps({ "data-streamdown": "link" }).children.join(""),
    "Authorize Google",
  );
  assert.deepEqual(renderer.root.findAllByType("h4").map((heading) => heading.children.join("")), [
    "Command", "Stdout", "Stderr",
  ]);
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
  assert.equal(renderer.root.findAllByType("details").length, 0);
  await act(async () => renderer.unmount());
});

test("all-tool renderer adapts known families and keeps unknown tools excellent", async () => {
  const tool = (callId, name, arguments_, result, status = "completed", extra = {}) => ({
    callId,
    name,
    arguments: JSON.stringify(arguments_, null, 2),
    result: result === undefined ? undefined : JSON.stringify(result, null, 2),
    status,
    children: [],
    ...extra,
  });
  const entries = [
    { id: "exec", kind: "tool", tool: tool(
      "exec", "sandbox_exec", { command: "printf hello", cwd: "/workspace/nanocodex-spin" },
      { success: true, exit_code: 0, stdout: "hello", stderr: "" },
      "completed", { durationNs: 1_250_000_000 },
    ) },
    { id: "process", kind: "tool", tool: tool(
      "process", "sandbox_start_process", { command: "npm start", ready_port: 8_000 },
      { process_id: "proc", pid: 42, status: "running", ready_port: 8_000 },
    ) },
    { id: "process-status", kind: "tool", tool: tool(
      "process-status", "sandbox_get_process", { process_id: "proc" },
      {
        found: true, process_id: "proc", command: "npm start", status: "failed",
        terminal: true, exit_code: 1, stdout: "started\n", stderr: "crashed\n",
      },
    ) },
    { id: "process-stop", kind: "tool", tool: tool(
      "process-stop", "sandbox_kill_process", { process_id: "proc" },
      { found: true, process_id: "proc", status: "killed", terminal: true, kill_requested: true },
    ) },
    { id: "preview", kind: "tool", tool: tool(
      "preview", "sandbox_preview", { port: 8_000 },
      { port: 8_000, url: "https://preview.example.test/app", persistent: false },
    ) },
    { id: "unsafe-preview", kind: "tool", tool: tool(
      "unsafe-preview", "sandbox_preview", { port: 8_001 },
      { port: 8_001, url: "javascript:alert(1)", persistent: false },
    ) },
    { id: "account", kind: "tool", tool: tool(
      "account", "accountInfo", {}, {
        status: "ready",
        authenticated: ["github"],
        connectorAccounts: { github: [{ id: "one" }, { id: "two" }] },
        machines: [{ id: "sandbox" }],
        vault: [{ id: "login" }],
      },
    ) },
    { id: "machine", kind: "tool", tool: tool(
      "machine", "user_build_agent_exec_command", { cmd: "pwd" }, { exit_code: 7, output: "denied" }, "failed",
      { metadata: { machine_name: "Build Agent", tool_name: "exec_command" } },
    ) },
    { id: "mcp", kind: "tool", tool: tool(
      "mcp", "mcp__linear__search_issues", { query: "renderer" }, { matches: ["NCX-1"] }, "cancelled",
    ) },
    { id: "browser", kind: "tool", tool: tool(
      "browser", "browser_execute", { action: "navigate" }, { url: "https://example.test" },
    ) },
    { id: "unknown", kind: "tool", tool: tool(
      "unknown", "renderHyperGraph", { depth: 3 }, { nodes: 12 }, "completed",
      { images: ["data:image/png;base64,AA=="] },
    ) },
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

  assert.deepEqual(renderer.root.findAllByType("strong").map((node) => node.children.join("")), [
    "Run command", "Start process", "Check process", "Stop process", "Open preview", "Open preview",
    "Account info", "Run command", "Search issues", "Execute", "Render hyper graph",
  ]);
  assert.deepEqual(renderer.root.findAllByProps({ className: "agent-terminal-tool-source" })
    .map((node) => node.children.join("")), [
    "Sandbox · /workspace/nanocodex-spin", "Sandbox", "Sandbox", "Sandbox", "Sandbox", "Sandbox", "Account",
    "Machine Build Agent", "MCP Linear", "Managed browser",
  ]);
  const headings = renderer.root.findAllByProps({ className: "agent-terminal-tool-heading" })
    .map((node) => node.children.flatMap((child) => child.children ?? []).join(" "));
  assert.match(headings[0], /printf hello.*Exit 0.*1 stdout line/);
  assert.match(headings[1], /npm start.*PID 42.*Running.*Port 8000 ready/);
  assert.match(headings[2], /proc.*Failed.*Exit 1.*2 stdout lines.*2 stderr lines/);
  assert.match(headings[3], /proc.*Killed/);
  assert.match(headings[4], /Port 8000.*Preview ready/);
  assert.match(headings[6], /Ready.*2 connectors.*1 machine.*1 Vault item/);
  assert.match(headings[10], /1 input field.*nodes.*12/);
  assert.deepEqual(renderer.root.findAllByProps({ className: "agent-terminal-tool-status" })
    .map((node) => node.children.join("")), [
    "Succeeded", "Succeeded", "Succeeded", "Succeeded", "Succeeded", "Succeeded", "Succeeded", "Failed", "Cancelled", "Succeeded", "Succeeded",
  ]);
  assert.ok(renderer.root.findAllByProps({ className: "agent-terminal-tool-meta" })[0]
    .children.some((node) => node.children?.join("") === "1.25 s"));
  const detailHeadings = renderer.root.findAllByType("h4").map((node) => node.children.join(""));
  assert.deepEqual(detailHeadings.slice(0, 3), ["Command", "Stdout", "Stderr"]);
  assert.equal(detailHeadings.filter((label) => label === "Stdout").length, 2);
  assert.equal(detailHeadings.filter((label) => label === "Stderr").length, 2);
  const links = renderer.root.findAllByType("a");
  assert.equal(links.length, 1);
  assert.equal(links[0].props.href, "https://preview.example.test/app");
  assert.equal(links[0].props.target, "_blank");
  assert.equal(links[0].props.rel, "noopener noreferrer");
  assert.deepEqual(renderer.root.findAllByType("details").filter((node) => node.props.open)
    .map((node) => node.props.className), [
    "agent-terminal-tool is-failed", "agent-terminal-tool is-cancelled",
  ]);
  assert.ok(renderer.root.findAllByType("code").some((node) => node.children.join("") === "user_build_agent_exec_command"));
  const images = renderer.root.findAllByType("img");
  assert.equal(images[0].props.src, "data:image/png;base64,AA==");
  assert.equal(images[0].props.alt, "Render hyper graph result 1");
  await act(async () => renderer.unmount());
});

test("subagent and host tools share concise what, where, state, duration, and outcome slots", async () => {
  const entries = [
    {
      id: "spawn", kind: "tool", tool: {
        callId: "spawn", name: "spawn_agent", status: "completed", durationNs: 12_000_000,
        arguments: "spawn renderer", input: JSON.stringify({
          role: "renderer specialist",
          task: "Inspect every tool result and report the relevant evidence without leaking schema noise.",
          output_schema: { type: "object", properties: { report: { type: "string" } } },
        }),
        output: JSON.stringify({ agent_id: 68, role: "renderer specialist", status: { state: "running" } }),
        children: [],
      },
    },
    {
      id: "wait", kind: "tool", tool: {
        callId: "wait", name: "wait_agent", status: "completed", durationNs: 2_000_000_000,
        arguments: "agents 68", input: JSON.stringify({ agent_ids: [68] }),
        output: JSON.stringify({
          agents: [{ agent_id: 68, role: "renderer specialist", status: { state: "completed" } }],
          timed_out: false,
        }),
        children: [],
      },
    },
    {
      id: "send", kind: "tool", tool: {
        callId: "send", name: "send_agent_message", status: "completed",
        arguments: "message agent", input: JSON.stringify({ agent_id: 68, message: "Please verify the preview link." }),
        output: JSON.stringify({ accepted: true }), children: [],
      },
    },
    {
      id: "interrupt", kind: "tool", tool: {
        callId: "interrupt", name: "interrupt_agent", status: "completed",
        arguments: "agent 68", input: JSON.stringify({ agent_id: 68 }),
        output: JSON.stringify({ status: { state: "interrupted" } }), children: [],
      },
    },
    {
      id: "close", kind: "tool", tool: {
        callId: "close", name: "close_agent", status: "completed",
        arguments: "agent 68", input: JSON.stringify({ agent_id: 68 }),
        output: JSON.stringify({ status: { state: "closed" } }), children: [],
      },
    },
    {
      id: "local", kind: "tool", tool: {
        callId: "local", name: "exec_command", status: "running",
        arguments: "pwd", input: JSON.stringify({ cmd: "pwd", workdir: "/repo" }), children: [],
      },
    },
    {
      id: "browser", kind: "tool", tool: {
        callId: "browser", name: "browser_capture_page", status: "completed",
        arguments: "page", input: JSON.stringify({ page: "main" }), output: JSON.stringify({ captured: true }), children: [],
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
  assert.deepEqual(renderer.root.findAllByType("strong").map((node) => node.children.join("")), [
    "Spawned renderer specialist",
    "Waiting on renderer specialist (68)",
    "Message agent 68",
    "Interrupt agent 68",
    "Close agent 68",
    "Run command",
    "Capture page",
  ]);
  assert.deepEqual(renderer.root.findAllByProps({ className: "agent-terminal-tool-source" })
    .map((node) => node.children.join("")), [
    "Subagent", "Subagent", "Subagent", "Subagent", "Subagent", "Local · /repo", "Web client",
  ]);
  const headings = renderer.root.findAllByProps({ className: "agent-terminal-tool-heading" })
    .map((node) => node.children.flatMap((child) => child.children ?? []).join(" "));
  assert.match(headings[0], /Inspect every tool result.*Agent 68.*Running/);
  assert.doesNotMatch(headings[0], /output_schema|properties/);
  assert.match(headings[1], /renderer specialist \(68\).*Completed/);
  assert.match(headings[2], /Please verify the preview link.*Accepted/);
  assert.match(headings[3], /Interrupted/);
  assert.match(headings[4], /Closed/);
  assert.ok(renderer.root.findAllByProps({ className: "agent-terminal-tool-meta" })[1]
    .children.some((node) => node.children?.join("") === "2.00 s"));
  assert.equal(renderer.root.findAllByProps({ role: "status" })[0].children.join(""), "Running");
  assert.ok(renderer.root.findAllByType("pre").some((node) => node.children.join("").includes("output_schema")));
  await act(async () => renderer.unmount());
});

test("machine aliases use result metadata and stay truthful while provenance is unavailable", async () => {
  const machineTool = (callId, status, metadata) => ({
    callId,
    name: "user_build_agent_exec_command",
    status,
    arguments: "pwd",
    input: JSON.stringify({ cmd: "pwd", workdir: "/repo" }),
    ...(status === "running" ? {} : { output: JSON.stringify({ exit_code: 1, output: "denied" }) }),
    ...(metadata === undefined ? {} : { metadata }),
    children: [],
  });
  const entries = [
    { id: "running", kind: "tool", tool: machineTool("running", "running") },
    { id: "failed", kind: "tool", tool: machineTool("failed", "failed") },
    {
      id: "identified", kind: "tool", tool: machineTool("identified", "failed", {
        machine_id: "build_agent",
        tool_name: "exec_command",
      }),
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

  assert.deepEqual(renderer.root.findAllByType("strong").map((node) => node.children.join("")), [
    "Machine tool", "Machine tool", "Run command",
  ]);
  assert.deepEqual(renderer.root.findAllByProps({ className: "agent-terminal-tool-source" })
    .map((node) => node.children.join("")), [
    "Machine · /repo", "Machine · /repo", "Machine build_agent · /repo",
  ]);
  assert.equal(renderer.root.findAllByProps({ className: "agent-terminal-tool-source" })
    .some((node) => node.children.join("") === "Machine build"), false);
  assert.equal(renderer.root.findAllByType("code")
    .filter((node) => node.children.join("") === "user_build_agent_exec_command").length, 3);
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
