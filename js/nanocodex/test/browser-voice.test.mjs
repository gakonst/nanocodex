import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

import { agentActions } from "../actions/index.mjs";
import { Actions, Voice } from "../browser/index.mjs";
import {
  BrowserVoiceSession,
  capturePreferredMicrophone,
  MICROPHONE_CAPTURE_TIMEOUT_MS,
  REALTIME_CALL_TIMEOUT_MS,
  SpeakerPlayback,
  SIDEBAND_OPEN_TIMEOUT_MS,
  VoiceError,
} from "../browser/VoiceSession.mjs";
import { createAgentClient, defineRuntime } from "../internal.mjs";
import { Agent as ManagedAgent } from "../managed/index.mjs";
import { initializeBrowserEngine } from "../browser/engine.mjs";

test("browser voice exposes Codex's ChatGPT V3 catalog and default", () => {
  assert.deepEqual(Voice.voices, [
    "juniper", "maple", "spruce", "ember", "vale", "breeze", "arbor", "sol", "cove",
  ]);
  assert.equal(Voice.defaultVoice, "cove");
  assert.equal(Voice.VoiceError, VoiceError);
  assert.throws(() => Voice.create({}), /Nanocodex Agent/);
});

test("starts the call while local ICE gathering is still in progress", async () => {
  const fixture = installBrowserVoiceFixture({ boundary: "ice" });
  const calls = [];
  const session = new BrowserVoiceSession({
    core: fakeVoiceCore(calls), voice: "cove",
    captureMicrophone: async () => fakeMicrophone(calls),
    onStatus() {}, onTranscript() {}, onTerminated() {},
  });
  try {
    await session.start();
    assert.equal(fixture.peer.iceGatheringState, "gathering");
    assert.equal(JSON.parse(fixture.request.call_body).sdp, "v=offer");
  } finally {
    await session.close();
    fixture.restore();
  }
});

test("managed SDP and sideband negotiation overlap admission without admitting early speech", async () => {
  const fixture = installBrowserVoiceFixture({ boundary: "sideband" });
  const calls = [];
  let admit;
  const admission = new Promise((resolve) => { admit = resolve; });
  const core = fakeVoiceCore(calls, {
    parallelStartup: true,
    async start() { await admission; calls.push(["admitted"]); },
  });
  const session = new BrowserVoiceSession({
    core, voice: "cove", captureMicrophone: async () => fakeMicrophone(calls),
    onStatus() {}, onTranscript() {}, onTerminated() {},
  });
  let ready = false;
  const starting = session.start().then(() => { ready = true; });
  try {
    await waitFor(() => fixture.request !== undefined);
    assert.equal(calls.some(([kind]) => kind === "admitted"), false);
    await waitFor(() => fixture.sideband !== undefined);
    fixture.sideband.readyState = WebSocket.OPEN;
    fixture.sideband.emit("open", {});
    await waitFor(() => calls.some(([kind]) => kind === "completeCall"));
    fixture.sideband.message({ type: "turn.delta" });
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(ready, false);
    assert.equal(calls.some(([kind]) => kind === "realtimeMessage"), false);
    admit();
    await starting;
    await waitFor(() => calls.some(([kind]) => kind === "realtimeMessage"));
    assert.equal(fixture.sidebandUrls.length, 1);
    assert.ok(fixture.sideband.sent.includes('{"type":"rust.frame"}'));
    assert.ok(calls.some(([kind]) => kind === "framesSent"));
  } finally {
    admit();
    await session.close();
    fixture.restore();
  }
});

test("failed parallel admission closes negotiated media without publishing ready", async () => {
  const fixture = installBrowserVoiceFixture();
  const calls = [];
  let deny;
  const admission = new Promise((_, reject) => { deny = reject; });
  const core = fakeVoiceCore(calls, {
    parallelStartup: true, start: () => admission,
  });
  const session = new BrowserVoiceSession({
    core, voice: "cove", captureMicrophone: async () => fakeMicrophone(calls),
    onStatus() {}, onTranscript() {}, onTerminated() {},
  });
  const starting = session.start();
  const rejected = assert.rejects(starting, /admission denied/);
  try {
    await waitFor(() => fixture.sideband?.readyState === WebSocket.OPEN);
    deny(new Error("admission denied"));
    await rejected;
    assert.equal(fixture.peer.signalingState, "closed");
    assert.equal(fixture.sideband.readyState, WebSocket.CLOSED);
    assert.equal(calls.some(([kind]) => kind === "sidebandOpened"), false);
  } finally {
    await session.close();
    fixture.restore();
  }
});

test("a sideband lost during admission cannot publish a ready session", async () => {
  const fixture = installBrowserVoiceFixture();
  const calls = [];
  let admit;
  const admission = new Promise((resolve) => { admit = resolve; });
  const session = new BrowserVoiceSession({
    core: fakeVoiceCore(calls, {
      parallelStartup: true, start: () => admission,
    }),
    voice: "cove", captureMicrophone: async () => fakeMicrophone(calls),
    onStatus() {}, onTranscript() {}, onTerminated() {},
  });
  const rejected = assert.rejects(session.start(), /closed during admission/);
  try {
    await waitFor(() => fixture.sideband?.readyState === WebSocket.OPEN);
    fixture.sideband.close();
    admit();
    await rejected;
    assert.equal(fixture.peer.signalingState, "closed");
    assert.equal(calls.some(([kind]) => kind === "sidebandOpened"), false);
  } finally {
    admit();
    await session.close();
    fixture.restore();
  }
});

test("the public managed voice forwards memory updates and durable admission failures", async () => {
  await initializeBrowserEngine({ module: await WebAssembly.compile(
    await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url)),
  ) });
  const fixture = installBrowserVoiceFixture();
  let events;
  let voiceSessionId;
  let delegated = false;
  const agent = ManagedAgent.open("019d2f5d-7491-8000-8000-000000000001", {
    baseUrl: "https://example.test",
    fetch: async (input, init) => {
      const path = new URL(input).pathname;
      if (path.endsWith("/realtime/start")) {
        voiceSessionId = JSON.parse(init.body).voice_session_id;
        return Response.json({ context: { workspace: "/brain", history: [] } });
      }
      if (path.endsWith("/realtime/calls")) return globalThis.fetch(input, init);
      if (path.endsWith("/realtime/delegate")) {
        delegated = true;
        return Response.json({ route: "started", turn_id: "failed-voice-turn" });
      }
      if (path.endsWith("/realtime/stop")) return Response.json({ stopped: true });
      if (path.endsWith("/events")) return new Response(new ReadableStream({
        start(controller) { events = controller; },
      }), { headers: { "content-type": "text/event-stream" } });
      throw new Error(`unexpected voice request: ${path}`);
    },
  });
  const voice = Voice.create(agent, { captureMicrophone: async () => fakeMicrophone([]) });
  try {
    await voice.start({ voice: "maple", pace: "fast", updates: "results", instructions: "Speak Greek.", acknowledgements: false });
    const provider = fixture.request.session;
    assert.equal(provider.model, "gpt-live-1-codex");
    assert.equal(provider.audio.output.voice, "maple");
    assert.deepEqual(provider.delegation, { type: "client", ack_filler: false });
    assert.match(provider.instructions, /Speak briskly/);
    assert.match(provider.instructions, /Speak Greek\./);
    await voice.speak("Read this aloud.");
    await voice.appendText("Selected README.md", { role: "developer" });
    await voice.appendContext("The editor selection changed.");
    const frames = fixture.sideband.sent.map((frame) => JSON.parse(frame));
    assert.ok(frames.some((frame) => frame.channel === "speakable" && frame.content[0].text === "Read this aloud."));
    assert.ok(frames.some((frame) => frame.type === "session.context.append" && frame.content[0].text === "Selected README.md" && !("channel" in frame)));
    await assert.rejects(voice.speak(" "), /voice text/);
    assert.equal(voice.getSnapshot().status, "active");
    await waitFor(() => events !== undefined);
    const cursor = "9007199254740993";
    const event = { cursor, created_at: 1, turn_id: null, type: "event", event: {
      type: "managed.voice.context", payload: {
        voice_session_id: voiceSessionId, result: { operation: "delete", key: { id: 5, version: 1 } },
      },
    } };
    events.enqueue(new TextEncoder().encode(`id: ${cursor}\nevent: event\ndata: ${JSON.stringify(event)}\n\n`));
    await waitFor(() => fixture.sideband.sent.some((frame) => frame.includes("delete")));
    assert.equal(fixture.sidebandUrls.length, 1);
    fixture.sideband.message({ type: "delegation.created", item: {
      type: "delegation", target: "client", id: "failed-handoff", content: [{ type: "input_text", text: "Look up the saved note" }],
    } });
    await waitFor(() => delegated);
    events.enqueue(new TextEncoder().encode('id: 9007199254740994\nevent: turn_failed\ndata: {"type":"turn_failed","id":"failed-voice-turn","turn_id":"failed-voice-turn","error":"private backend error","cursor":"9007199254740994","created_at":2}\n\n'));
    await waitFor(() => fixture.sideband.sent.some((encoded) => {
      const frame = JSON.parse(encoded);
      return frame.delegation_item_id === "failed-handoff" && frame.content[0].text === "I couldn't complete that request. Please try again.";
    }));
  } finally {
    await voice.destroy();
    fixture.restore();
  }
});

test("both transcript rows stream while delegation admission is blocked", async () => {
  const fixture = installBrowserVoiceFixture();
  const calls = [];
  let releaseAdmission;
  const admission = new Promise((resolve) => { releaseAdmission = resolve; });
  let fences = 0;
  const core = fakeVoiceCore(calls, {
    async sidebandClosed() {
      return JSON.stringify({ frames: [], transcripts: [], reconnect_after_ms: 0 });
    },
    async realtimeMessage(payload) {
      const event = JSON.parse(payload);
      calls.push(["realtimeMessage", event.type]);
      return JSON.stringify({ frames: [], transcripts: event.transcripts ?? [] });
    },
  });
  const { agent } = await testAgent(core, calls);
  const voice = Voice.create(agent, {
    captureMicrophone: async () => fakeMicrophone(calls),
    async beforeAgentTurn() { if (++fences === 2) await admission; },
  });
  try {
    await voice.start();
    fixture.sideband.message({ type: "delegation.created" });
    await waitFor(() => fences === 2);
    const update = (speaker, text, is_partial) => fixture.sideband.message({
      type: "transcript", transcripts: [{ speaker, text, id: 0, is_partial }],
    });
    update("user", "Check", true);
    update("assistant", "I will", true);
    update("user", "Check the build", true);
    await waitFor(() => voice.getSnapshot().transcripts[0]?.text === "Check the build");
    assert.equal(voice.getSnapshot().transcripts.length, 2);
    assert.equal(voice.getSnapshot().transcripts[1].text, "I will");
    assert.equal(voice.getSnapshot().transcripts[0].isPartial, true);
    const id = voice.getSnapshot().transcripts[0].id;
    update("user", "Check the build.", false);
    await waitFor(() => voice.getSnapshot().transcripts[0]?.isPartial === false);
    assert.equal(voice.getSnapshot().transcripts[0].id, id);
    assert.equal(voice.getSnapshot().transcripts.length, 2);
    const previousSideband = fixture.sideband;
    previousSideband.close();
    await waitFor(() => fixture.sideband !== previousSideband);
    await waitFor(() => calls.filter(([kind]) => kind === "sidebandOpened").length === 2);
    const stopping = voice.stop();
    releaseAdmission();
    await stopping;
    assert.equal(calls.filter(([kind, type]) => kind === "realtimeMessage" && type === "delegation.created").length, 1);
  } finally {
    releaseAdmission();
    await voice.destroy();
    agent.dispose();
    fixture.restore();
  }
});

test("the public resource is a thin binding over the Rust voice controller", async () => {
  const fixture = installBrowserVoiceFixture();
  try {
    const calls = [];
    const core = fakeVoiceCore(calls);
    const { agent, emitAgentEvent } = await testAgent(core, calls);
    const voice = Actions.voice.create(agent, {
      beforeAgentTurn: async () => { calls.push(["fence"]); },
      captureMicrophone: async () => {
        calls.push(["microphone"]);
        return fakeMicrophone(calls);
      },
    });

    await Actions.voice.start(voice, { voice: "juniper" });
    assert.equal(Actions.voice.getSnapshot(voice).status, "active");
    assert.deepEqual(calls.slice(0, 6), [
      ["microphone"],
      ["browserVoice", "juniper"],
      ["configure", { voice: "juniper" }],
      ["fence"],
      ["start"],
      ["callBody", "v=offer"],
    ]);
    assert.equal(calls.some(([kind]) => kind === "completeCall"), true);
    assert.equal(calls.some(([kind]) => kind === "sidebandUrl"), true);
    assert.equal(fixture.request.session_id, "agent-session");
    assert.deepEqual(JSON.parse(fixture.request.call_body), {
      sdp: "v=offer",
      session: { delegation: { type: "client" } },
    });

    fixture.sideband.message({ type: "delegation.created" });
    await waitFor(() => fixture.sideband.sent.includes('{"type":"rust.frame"}'));
    assert.equal(calls.filter(([kind]) => kind === "fence").length, 2);
    emitAgentEvent({ type: "assistant.message", payload: { text: "done" } });
    await waitFor(() => calls.some(([kind]) => kind === "agentEvent"));
    assert.deepEqual(JSON.parse(calls.find(([kind]) => kind === "agentEvent")[1]), {
      type: "event",
      target: { pane: "main", branchId: "agent-session" },
      event: { type: "assistant.message", payload: { text: "done" } },
    });

    const firstSideband = fixture.sideband;
    firstSideband.close();
    await waitFor(() => calls.some(([kind]) => kind === "sidebandClosed"));
    await new Promise((resolve) => setTimeout(resolve, 210));
    await waitFor(() => fixture.sideband !== firstSideband);
    assert.equal(
      calls.filter(([kind]) => kind === "sidebandOpened").length,
      2,
    );

    await Actions.voice.stop(voice);
    assert.equal(Actions.voice.getSnapshot(voice).status, "idle");
    assert.equal(calls.filter(([kind]) => kind === "fence").length, 3);
    assert.equal(calls.some(([kind]) => kind === "stop"), true);
    assert.equal(calls.some(([kind]) => kind === "free"), true);
    assert.equal(fixture.sideband.sent.includes('{"type":"session.close"}'), true);
    agent.dispose();
  } finally {
    fixture.restore();
  }
});

test("requests the microphone before waiting for the Rust controller", async () => {
  const fixture = installBrowserVoiceFixture();
  try {
    const order = [];
    let resolveCore;
    const core = new Promise((resolve) => { resolveCore = resolve; });
    const session = new BrowserVoiceSession({
      core,
      sessionId: "mobile-session",
      voice: "cove",
      captureMicrophone() {
        order.push("microphone");
        return Promise.resolve(fakeMicrophone(order));
      },
      onStatus() {},
      onTranscript() {},
      onTerminated() {},
    });

    const starting = session.start();
    assert.equal(order[0], "microphone");
    session.abort();
    resolveCore(fakeVoiceCore(order));
    await starting;
    assert.equal(order.some((entry) => Array.isArray(entry) && entry[0] === "track.stop"), true);
  } finally {
    fixture.restore();
  }
});

test("negotiates media during admission and opens the sideband while applying the answer", async () => {
  const fixture = installBrowserVoiceFixture();
  const calls = [];
  let admit, answer;
  const admission = new Promise((resolve) => { admit = resolve; });
  const appliedAnswer = new Promise((resolve) => { answer = resolve; });
  const session = new BrowserVoiceSession({
    core: fakeVoiceCore(calls), voice: "cove",
    captureMicrophone: async () => fakeMicrophone(calls),
    beforeAgentTurn: () => admission,
    onStatus() {}, onTranscript() {}, onTerminated() {},
  });
  try {
    const starting = session.start();
    await waitFor(() => fixture.peer?.localDescription !== undefined);
    assert.equal(fixture.request, undefined, "provider call must wait for admitted context");
    assert.equal(calls.some(([kind]) => kind === "start"), false);
    fixture.peer.setRemoteDescription = () => appliedAnswer;
    admit();
    await waitFor(() => calls.some(([kind]) => kind === "sidebandOpened"));
    answer();
    await starting;
    await session.close();
  } finally {
    admit(); answer(); session.abort(); fixture.restore();
  }
});

test("failed admission stops late microphone capture without creating a peer", async () => {
  const fixture = installBrowserVoiceFixture();
  const calls = [];
  let captured;
  const session = new BrowserVoiceSession({
    core: fakeVoiceCore(calls), voice: "cove",
    captureMicrophone: () => new Promise((resolve) => { captured = resolve; }),
    beforeAgentTurn: async () => { throw new Error("admission rejected"); },
    onStatus() {}, onTranscript() {}, onTerminated() {},
  });
  try {
    await assert.rejects(session.start(), /admission rejected/);
    captured(fakeMicrophone(calls));
    await waitFor(() => calls.some(([kind]) => kind === "track.stop"));
    assert.equal(fixture.peer, undefined);
    assert.equal(fixture.request, undefined);
  } finally { session.abort(); fixture.restore(); }
});

test("stop waits for an in-flight lifecycle start before stopping and freeing its controller", async () => {
  const fixture = installBrowserVoiceFixture();
  const calls = [];
  let started;
  const core = fakeVoiceCore(calls);
  core.start = () => {
    calls.push(["start"]);
    return new Promise((resolve) => { started = resolve; });
  };
  const session = new BrowserVoiceSession({
    core, voice: "cove", captureMicrophone: async () => fakeMicrophone(calls),
    onStatus() {}, onTranscript() {}, onTerminated() {},
  });
  try {
    const starting = session.start();
    await waitFor(() => fixture.peer?.localDescription !== undefined);
    const closing = session.close();
    assert.equal(calls.some(([kind]) => kind === "track.stop"), true);
    assert.equal(calls.some(([kind]) => kind === "free" || kind === "stop"), false);
    started();
    await Promise.all([starting, closing]);
    assert.deepEqual(calls.filter(([kind]) => ["start", "stop", "free"].includes(kind)), [["start"], ["stop"], ["free"]]);
    assert.equal(fixture.request, undefined);
  } finally { started?.(); session.abort(); fixture.restore(); }
});

test("explains browser and embed microphone denials", async () => {
  const previous = {
    document: Object.getOwnPropertyDescriptor(globalThis, "document"),
    navigator: Object.getOwnPropertyDescriptor(globalThis, "navigator"),
    window: Object.getOwnPropertyDescriptor(globalThis, "window"),
  };
  const denial = Object.assign(new Error("Permission denied"), { name: "NotAllowedError" });
  try {
    Object.defineProperty(globalThis, "navigator", {
      configurable: true,
      value: { mediaDevices: { getUserMedia: async () => { throw denial; } } },
    });
    globalThis.window = { top: {} };
    globalThis.document = { permissionsPolicy: { allowsFeature: () => false } };
    await assert.rejects(
      capturePreferredMicrophone(async () => undefined),
      (error) => error instanceof VoiceError
        && error.code === "microphone_permission_blocked"
        && /host iframe must allow="microphone"/.test(error.message),
    );

    const topWindow = {};
    topWindow.top = topWindow;
    globalThis.window = topWindow;
    globalThis.document = { permissionsPolicy: { allowsFeature: () => true } };
    await assert.rejects(
      capturePreferredMicrophone(async () => undefined),
      (error) => error instanceof VoiceError
        && error.code === "microphone_permission_blocked"
        && /Allow it in your browser settings, then retry/.test(error.message),
    );

    const missing = Object.assign(new Error("No device"), { name: "NotFoundError" });
    Object.defineProperty(globalThis, "navigator", {
      configurable: true,
      value: { mediaDevices: { getUserMedia: async () => { throw missing; } } },
    });
    await assert.rejects(
      capturePreferredMicrophone(async () => undefined),
      (error) => error instanceof VoiceError && error.code === "microphone_not_found",
    );
  } finally {
    restoreGlobal("document", previous.document);
    restoreGlobal("navigator", previous.navigator);
    restoreGlobal("window", previous.window);
  }
});

test("stops the initially acquired microphone when device selection fails", async () => {
  const previous = Object.getOwnPropertyDescriptor(globalThis, "navigator");
  const stopped = [];
  const makeMicrophone = () => ({
    getAudioTracks: () => [{ label: "Built-in microphone", getSettings: () => ({ deviceId: "current" }) }],
    getTracks: () => [{ stop: () => stopped.push(true) }],
  });
  try {
    const enumerateFailure = new Error("device enumeration failed");
    const first = makeMicrophone();
    Object.defineProperty(globalThis, "navigator", {
      configurable: true,
      value: { mediaDevices: {
        getUserMedia: async () => first,
        enumerateDevices: async () => { throw enumerateFailure; },
      } },
    });
    await assert.rejects(capturePreferredMicrophone(async () => undefined), (error) => error === enumerateFailure);
    assert.equal(stopped.length, 1);

    const selectionFailure = new Error("physical input selection failed");
    const second = makeMicrophone();
    Object.defineProperty(globalThis, "navigator", {
      configurable: true,
      value: { mediaDevices: {
        getUserMedia: async () => second,
        enumerateDevices: async () => [{ kind: "audioinput", label: "Built-in microphone", deviceId: "current" }],
      } },
    });
    await assert.rejects(
      capturePreferredMicrophone(async () => { throw selectionFailure; }),
      (error) => error === selectionFailure,
    );
    assert.equal(stopped.length, 2);
  } finally {
    restoreGlobal("navigator", previous);
  }
});

test("bounds stalled microphone acquisition with an actionable typed error", async (t) => {
  const fixture = installBrowserVoiceFixture();
  const calls = [];
  const timers = new Map();
  let nextTimer = 1;
  let resolveCapture;
  t.mock.method(globalThis.window, "setTimeout", (callback, delay) => {
    const handle = nextTimer;
    nextTimer += 1;
    timers.set(handle, { callback, delay });
    return handle;
  });
  t.mock.method(globalThis.window, "clearTimeout", (handle) => timers.delete(handle));
  try {
    const { agent } = await testAgent(fakeVoiceCore(calls), calls);
    const voice = Actions.voice.create(agent, {
      captureMicrophone: () => new Promise((resolve) => { resolveCapture = resolve; }),
    });

    const starting = voice.start();
    const timeout = [...timers.entries()].find(([, timer]) => (
      timer.delay === MICROPHONE_CAPTURE_TIMEOUT_MS
    ));
    assert.notEqual(timeout, undefined);
    timers.delete(timeout[0]);
    timeout[1].callback();

    await assert.rejects(starting, (error) => (
      error instanceof VoiceError
      && error.code === "microphone_capture_timeout"
      && /selected microphone or reconnect it/.test(error.message)
    ));
    const snapshot = voice.getSnapshot();
    assert.equal(snapshot.status, "error");
    assert.equal(snapshot.error?.code, "microphone_capture_timeout");
    assert.match(snapshot.statusText, /selected microphone or reconnect it/);
    assert.equal(fixture.request, undefined);

    resolveCapture(fakeMicrophone(calls));
    await waitFor(() => calls.some(([kind]) => kind === "track.stop"));
    assert.equal(timers.size, 0);
    agent.dispose();
  } finally {
    fixture.restore();
  }
});

test("bounds each browser voice startup boundary with typed cleanup errors", async (t) => {
  const boundaries = [
    ["fetch", REALTIME_CALL_TIMEOUT_MS, "realtime_call_timeout", "connection request", "request"],
    ["sideband", SIDEBAND_OPEN_TIMEOUT_MS, "sideband_open_timeout", "sideband", "sideband"],
  ];
  for (const [boundary, timeoutMs, code, message, resource] of boundaries) {
    const fixture = installBrowserVoiceFixture({ boundary });
    const calls = [];
    const timers = new Map();
    let nextTimer = 1;
    t.mock.method(globalThis.window, "setTimeout", (callback, delay) => {
      const handle = nextTimer;
      nextTimer += 1;
      timers.set(handle, { callback, delay });
      return handle;
    });
    t.mock.method(globalThis.window, "clearTimeout", (handle) => timers.delete(handle));
    const session = new BrowserVoiceSession({
      core: fakeVoiceCore(calls),
      sessionId: `timeout-${boundary}`,
      voice: "cove",
      captureMicrophone: async () => fakeMicrophone(calls),
      onStatus() {},
      onTranscript() {},
      onTerminated() {},
    });
    try {
      const starting = session.start();
      await waitFor(() => (
        resource === "peer" ? fixture.peer !== undefined
          : resource === "request" ? fixture.requestSignal !== undefined
            : fixture.sideband !== undefined
      ));
      const timeout = [...timers.entries()].find(([, timer]) => timer.delay === timeoutMs);
      assert.notEqual(timeout, undefined, boundary);
      timers.delete(timeout[0]);
      timeout[1].callback();
      await assert.rejects(starting, (error) => (
        error instanceof VoiceError && error.code === code && error.message.includes(message)
      ));
      if (boundary === "fetch") assert.equal(fixture.requestSignal.aborted, true);
      if (boundary === "sideband") assert.equal(fixture.sideband.readyState, fixture.sideband.constructor.CLOSED);
      await session.close();
    } finally {
      fixture.restore();
    }
  }
});

test("stop and cancel settle pending capture and stop a late microphone", async () => {
  for (const action of ["stop", "cancel"]) {
    const fixture = installBrowserVoiceFixture();
    const calls = [];
    let resolveCapture;
    try {
      const { agent } = await testAgent(fakeVoiceCore(calls), calls);
      const voice = Actions.voice.create(agent, {
        captureMicrophone: () => new Promise((resolve) => { resolveCapture = resolve; }),
      });
      const starting = voice.start();
      assert.equal(voice.getSnapshot().status, "connecting");

      const result = await voice[action]();
      assert.equal(result, action === "cancel" ? true : undefined);
      await starting;
      assert.equal(voice.getSnapshot().status, "idle");
      assert.equal(fixture.request, undefined);

      resolveCapture(fakeMicrophone(calls));
      await waitFor(() => calls.some(([kind]) => kind === "track.stop"));
      agent.dispose();
    } finally {
      fixture.restore();
    }
  }
});

test("a replacement resource does not wait on the previous pending microphone", async () => {
  const fixture = installBrowserVoiceFixture();
  const calls = [];
  let resolveFirstCapture;
  try {
    const { agent } = await testAgent(fakeVoiceCore(calls), calls);
    const first = Actions.voice.create(agent, {
      captureMicrophone: () => new Promise((resolve) => { resolveFirstCapture = resolve; }),
    });
    const second = Actions.voice.create(agent, {
      captureMicrophone: async () => fakeMicrophone(calls),
    });

    const firstStarting = first.start();
    await second.start();
    await firstStarting;
    assert.equal(first.getSnapshot().status, "idle");
    assert.equal(second.getSnapshot().status, "active");

    resolveFirstCapture(fakeMicrophone(calls));
    await waitFor(() => calls.some(([kind]) => kind === "track.stop"));
    await second.stop();
    agent.dispose();
  } finally {
    fixture.restore();
  }
});

test("refreshes an asynchronous sideband authorization before reconnecting", async () => {
  const fixture = installBrowserVoiceFixture();
  const calls = [];
  const statuses = [];
  let tickets = 0;
  try {
    const session = new BrowserVoiceSession({
      core: fakeVoiceCore(calls),
      sessionId: "connect-agent",
      voice: "cove",
      captureMicrophone: async () => fakeMicrophone(calls),
      call: async () => new Response("v=answer", {
        headers: { "x-nanocodex-realtime-location": "/v1/live/rtc_connect" },
      }),
      async sidebandUrl(callId) {
        tickets += 1;
        return `wss://connect.example/sideband?call_id=${callId}&ticket=${tickets}`;
      },
      onStatus(status) { statuses.push(status); },
      onTranscript() {},
      onTerminated() {},
    });
    await session.start();
    assert.match(fixture.sidebandUrls[0], /ticket=1$/);
    const first = fixture.sideband;
    statuses.length = 0;
    first.close();
    await new Promise((resolve) => setTimeout(resolve, 210));
    await waitFor(() => fixture.sideband !== first);
    assert.match(fixture.sidebandUrls[1], /ticket=2$/);
    await waitFor(() => statuses.includes("Voice active (cove)"));
    await session.close();
  } finally {
    fixture.restore();
  }
});

test("stop tears browser media down while startup boundaries are stalled", async () => {
  for (const boundary of ["ice", "fetch", "sideband"]) {
    const fixture = installBrowserVoiceFixture({ boundary });
    const calls = [];
    try {
      const session = new BrowserVoiceSession({
        core: fakeVoiceCore(calls),
        sessionId: `stalled-${boundary}`,
        voice: "cove",
        captureMicrophone: async () => fakeMicrophone(calls),
        onStatus() {},
        onTranscript() {},
        onTerminated() {},
      });
      const starting = session.start();
      await waitFor(() => (
        boundary === "ice" ? fixture.peer !== undefined
          : boundary === "fetch" ? fixture.requestSignal !== undefined
            : fixture.sideband !== undefined
      ));
      await Promise.race([
        session.close(),
        new Promise((_, reject) => setTimeout(() => reject(new Error(`${boundary} stop timed out`)), 100)),
      ]);
      if (boundary !== "fetch") await starting.catch(() => {});
      assert.equal(calls.some(([kind]) => kind === "track.stop"), true, boundary);
      if (boundary === "fetch") assert.equal(fixture.requestSignal.aborted, true);
    } finally {
      fixture.restore();
    }
  }
});

test("starts waiting on one stop coalesce into one replacement session", async () => {
  const fixture = installBrowserVoiceFixture();
  const calls = [];
  let releaseStop;
  let stopCount = 0;
  const core = fakeVoiceCore(calls, {
    async stop() {
      stopCount += 1;
      if (stopCount === 1) await new Promise((resolve) => { releaseStop = resolve; });
      return JSON.stringify({ frames: [], transcripts: [], schedule_flush: false });
    },
  });
  try {
    const { agent } = await testAgent(core, calls);
    const voice = Actions.voice.create(agent, {
      captureMicrophone: async () => {
        calls.push(["microphone"]);
        return fakeMicrophone(calls);
      },
    });
    await voice.start();
    const stopping = voice.stop();
    await waitFor(() => typeof releaseStop === "function");
    const first = voice.start();
    const second = voice.start();
    releaseStop();
    await Promise.all([stopping, first, second]);
    assert.equal(calls.filter(([kind]) => kind === "microphone").length, 2);
    await voice.stop();
    agent.dispose();
  } finally {
    fixture.restore();
  }
});

test("speaker playback resumes from the next user gesture when autoplay is blocked", async () => {
  let attempts = 0;
  let resume;
  const speaker = {
    autoplay: false,
    srcObject: null,
    pause() {},
    play() {
      attempts += 1;
      return attempts === 1 ? Promise.reject(new Error("blocked")) : Promise.resolve();
    },
  };
  const gestures = {
    addEventListener(_type, listener) { resume = listener; },
    removeEventListener(_type, listener) { if (resume === listener) resume = undefined; },
  };
  const playback = new SpeakerPlayback(speaker, () => {}, gestures);
  playback.setEnabled(false);
  playback.attach({});
  assert.equal(speaker.muted, true);
  playback.setEnabled(true);
  assert.equal(speaker.muted, false);
  await Promise.resolve();
  await Promise.resolve();
  resume();
  await Promise.resolve();
  assert.equal(attempts, 2);
  playback.close();
});

test("Rust playback permission follows successful frame delivery and reconnect replay", async () => {
  const fixture = installBrowserVoiceFixture();
  const previousAudio = Object.getOwnPropertyDescriptor(globalThis, "Audio");
  const previousDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  const speaker = { muted: false, play: async () => {}, pause() {} };
  globalThis.Audio = class { constructor() { return speaker; } };
  globalThis.document = new EventTarget();
  const calls = [];
  const frame = JSON.stringify({ type: "session.context.append", content: [{ type: "input_text", text: "Retrieved answer" }] });
  let pending = false;
  const core = fakeVoiceCore(calls, {
    async sidebandOpened() { return JSON.stringify({ frames: pending ? [frame] : [], acknowledge_frames: pending, playback_enabled: pending }); },
    async framesSent(count) { calls.push(["framesSent", count]); pending = false; },
    async agentEvent() { pending = true; return JSON.stringify({ frames: [frame], acknowledge_frames: true, playback_enabled: true }); },
  });
  const { agent, emitAgentEvent } = await testAgent(core, calls);
  const voice = Voice.create(agent, { captureMicrophone: async () => fakeMicrophone(calls) });
  try {
    await voice.start();
    fixture.peer.emit("track", { track: {}, streams: [{}] });
    assert.equal(speaker.muted, true);
    fixture.sideband.close();
    emitAgentEvent({ type: "assistant.message" });
    await waitFor(() => pending);
    assert.equal(speaker.muted, true, "unsent context must keep playback gated");
    await new Promise((resolve) => setTimeout(resolve, 220));
    await waitFor(() => fixture.sideband.sent.includes(frame));
    assert.equal(speaker.muted, false);
    assert.deepEqual(calls.filter(([name]) => name === "framesSent"), [["framesSent", 1]]);
  } finally {
    await voice.destroy(); agent.dispose(); fixture.restore();
    restoreGlobal("Audio", previousAudio); restoreGlobal("document", previousDocument);
  }
});

function fakeVoiceCore(calls, overrides = {}) {
  return {
    async configure(settings) { calls.push(["configure", JSON.parse(settings)]); },
    async start() { calls.push(["start"]); },
    async callBody(sdp) {
      calls.push(["callBody", sdp]);
      return JSON.stringify({
        session_id: "agent-session",
        call_body: JSON.stringify({ sdp, session: { delegation: { type: "client" } } }),
      });
    },
    async completeCall(body, location) {
      calls.push(["completeCall", body, location]);
      return JSON.stringify({ call_id: "rtc_test", sdp: body });
    },
    async sidebandUrl(callId) {
      calls.push(["sidebandUrl", callId]);
      return `/api/realtime/sideband?call_id=${callId}`;
    },
    async sidebandOpened() {
      calls.push(["sidebandOpened"]);
      return JSON.stringify({ frames: [], transcripts: [], schedule_flush: false });
    },
    async sidebandClosed(connectedMs) {
      calls.push(["sidebandClosed", connectedMs]);
      return JSON.stringify({
        frames: [],
        transcripts: [],
        reconnect_after_ms: 200,
        schedule_flush: false,
      });
    },
    async framesSent(count) { calls.push(["framesSent", count]); },
    async requiresAgentAdmission(payload) {
      calls.push(["requiresAgentAdmission", payload]);
      return JSON.parse(payload).type === "delegation.created";
    },
    async realtimeMessage(payload) {
      calls.push(["realtimeMessage", payload]);
      return JSON.stringify({
        frames: ['{"type":"rust.frame"}'],
        transcripts: [{ speaker: "user", text: "ship it" }],
        acknowledge_frames: true,
        schedule_flush: false,
      });
    },
    async agentEvent(envelope) {
      calls.push(["agentEvent", envelope]);
      return JSON.stringify({ frames: [], transcripts: [], schedule_flush: false });
    },
    async flush(finalChunk) {
      calls.push(["flush", finalChunk]);
      return JSON.stringify({ frames: [], transcripts: [], schedule_flush: false });
    },
    async stop() {
      calls.push(["stop"]);
      return JSON.stringify({
        frames: ['{"type":"session.close"}'],
        transcripts: [],
        status: "Voice stopped",
        schedule_flush: false,
      });
    },
    async cancel() { calls.push(["cancel"]); return true; },
    async preferredPhysicalInput() { return undefined; },
    free() { calls.push(["free"]); },
    ...overrides,
  };
}

async function testAgent(core, calls) {
  let listener;
  const raw = {
    sessionId: "agent-session",
    prompt() { throw new Error("the JS voice binding must not prompt the Agent"); },
    browserVoice(voice) { calls.push(["browserVoice", voice]); return core; },
    free() {},
  };
  const runtime = defineRuntime({
    create: async () => raw,
    subscribe(next) { listener = next; return () => { listener = undefined; }; },
    decorate: (agent) => agent.extend(agentActions()),
  });
  return {
    agent: await createAgentClient(runtime),
    emitAgentEvent(event) { listener?.(event); },
  };
}

function fakeMicrophone(calls) {
  return {
    getAudioTracks: () => [],
    getTracks: () => [{ stop: () => calls.push(["track.stop"]) }],
  };
}

function installBrowserVoiceFixture({ boundary } = {}) {
  const previous = {
    RTCPeerConnection: globalThis.RTCPeerConnection,
    WebSocket: globalThis.WebSocket,
    fetch: globalThis.fetch,
    location: globalThis.location,
    window: globalThis.window,
  };
  const fixture = {
    peer: undefined,
    request: undefined,
    requestSignal: undefined,
    sideband: undefined,
    sidebandUrls: [],
  };
  class FakePeer {
    connectionState = "connected";
    iceGatheringState = boundary === "ice" ? "gathering" : "complete";
    localDescription;
    signalingState = "stable";
    listeners = new Map();
    constructor() { fixture.peer = this; }
    addEventListener(type, listener) {
      const listeners = this.listeners.get(type) ?? new Set();
      listeners.add(listener); this.listeners.set(type, listeners);
    }
    removeEventListener(type, listener) { this.listeners.get(type)?.delete(listener); }
    emit(type, event) { for (const listener of this.listeners.get(type) ?? []) listener(event); }
    addTrack() {}
    close() { this.signalingState = "closed"; }
    createDataChannel() { return { close() {} }; }
    async createOffer() { return { type: "offer", sdp: "v=offer" }; }
    async setLocalDescription(description) { this.localDescription = description; }
    async setRemoteDescription() {}
  }
  class FakeWebSocket {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSED = 3;
    readyState = FakeWebSocket.CONNECTING;
    listeners = new Map();
    sent = [];
    constructor(url) {
      fixture.sidebandUrls.push(String(url));
      fixture.sideband = this;
      if (boundary === "sideband") return;
      queueMicrotask(() => {
        this.readyState = FakeWebSocket.OPEN;
        this.emit("open", {});
      });
    }
    addEventListener(type, listener) {
      const listeners = this.listeners.get(type) ?? new Set();
      listeners.add(listener);
      this.listeners.set(type, listeners);
    }
    removeEventListener(type, listener) { this.listeners.get(type)?.delete(listener); }
    emit(type, event) { for (const listener of this.listeners.get(type) ?? []) listener(event); }
    message(value) { this.emit("message", { data: JSON.stringify(value) }); }
    send(value) { this.sent.push(value); }
    close() {
      this.readyState = FakeWebSocket.CLOSED;
      this.emit("close", {});
    }
  }
  globalThis.location = new URL("https://example.test/agent");
  globalThis.window = { clearTimeout, location: globalThis.location, setTimeout };
  globalThis.RTCPeerConnection = FakePeer;
  globalThis.WebSocket = FakeWebSocket;
  globalThis.fetch = async (_url, init) => {
    fixture.request = JSON.parse(init.body);
    fixture.requestSignal = init.signal;
    if (boundary === "fetch") return new Promise(() => {});
    return new Response("v=answer", {
      headers: { "x-nanocodex-realtime-location": "/v1/live/rtc_test" },
    });
  };
  return {
    get request() { return fixture.request; },
    get requestSignal() { return fixture.requestSignal; },
    get peer() { return fixture.peer; },
    get sideband() { return fixture.sideband; },
    get sidebandUrls() { return fixture.sidebandUrls; },
    restore() { Object.assign(globalThis, previous); },
  };
}

async function waitFor(predicate) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
  throw new Error("condition was not reached");
}

function restoreGlobal(name, descriptor) {
  if (descriptor) Object.defineProperty(globalThis, name, descriptor);
  else delete globalThis[name];
}
