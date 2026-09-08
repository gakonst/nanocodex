import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { Agent } from "../managed/index.mjs";
import { createManagedBrowserVoice } from "../managed/Voice.mjs";
import { managedBrowserVoiceTransport } from "../managed/internal.mjs";
import { Voice } from "../browser/index.mjs";

const AGENT_ID = "019d2f5d-7491-8000-8000-000000000001";
const UUID_V7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

test("Rust speech waits for a provider handoff before reading memory or admitting agent work", { timeout: 5000 }, async () => {
  const module = await WebAssembly.compile(await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url)));
  const requests = [];
  const agent = Agent.open(AGENT_ID, { baseUrl: "https://managed.example", fetch: async (input, init) => {
    const path = new URL(input).pathname;
    requests.push({ path, body: JSON.parse(init.body) });
    if (path.endsWith("/delegate")) return Response.json({ route: "started", turn_id: "first" });
    return Response.json({ context: { workspace: "/brain", history: [] } });
  } });
  const voice = await createManagedBrowserVoice(agent, "cove", { module });
  try {
    await voice.start();
    voice.callBody("v=offer");
    assert.equal(JSON.parse(voice.sidebandOpened()).playback_enabled, true);
    await voice.realtimeMessage(JSON.stringify({ type: "input_transcript.added", item: { text: "Hi, say hello briefly" } }));
    await voice.realtimeMessage(JSON.stringify({ type: "turn.done", turn: { role: "user", transcript: "Hi, say hello briefly" } }));
    const greeting = JSON.parse(await voice.realtimeMessage(JSON.stringify({ type: "output_transcript.added", item: { text: "Hello!" } })));
    assert.equal(greeting.transcripts.length, 1);
    await new Promise((resolve) => setTimeout(resolve, 300));
    assert.equal(requests.length, 1, "speech never schedules speculative memory requests or model work");
    await voice.realtimeMessage(JSON.stringify({ type: "turn.done", turn: { role: "user", transcript: "When is Elena's birthday?" } }));
    assert.equal(requests.length, 1);
    await voice.realtimeMessage(JSON.stringify({ type: "delegation.created", item: {
      type: "delegation", target: "client", id: "lookup", content: [{ type: "input_text", text: "Look up Elena's saved birthday" }],
    } }));
    assert.match(requests[1].path, /realtime\/delegate$/);
    assert.doesNotMatch(requests[1].body.input, /voice_bootstrap/);
    assert.match(requests[1].body.input, /Look up Elena's saved birthday/);
    await voice.stop();
  } finally { voice.free(); }
});

test("managed Rust queues late startup context once while SDP is already in flight", async () => {
  const wasm = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const module = await WebAssembly.compile(wasm);
  let admit;
  const admission = new Promise((resolve) => { admit = resolve; });
  const agent = Agent.open(AGENT_ID, {
    baseUrl: "https://managed.example",
    fetch: async () => {
      await admission;
      return Response.json({ context: { workspace: "/brain/" + "nested/".repeat(1000), history: Array.from({ length: 4 }, () => ({
        type: "message", role: "user", content: [{ type: "input_text", text: "The current project is Juniper. " + "Retained detail. ".repeat(900) }],
      })) } });
    },
  });
  const voice = await createManagedBrowserVoice(agent, "cove", { module });
  try {
    const starting = voice.start();
    const call = JSON.parse(voice.callBody("v=offer"));
    assert.equal(voice.parallelStartup, true);
    assert.doesNotMatch(JSON.parse(call.call_body).session.instructions, /current project is Juniper/);
    admit();
    await starting;
    const context = JSON.parse(voice.sidebandOpened());
    assert.match(context.frames.join(""), /current project is Juniper/);
    const texts = context.frames.map((frame) => JSON.parse(frame).content[0].text);
    assert.ok(texts.join("").length > 8192, "late admission preserves the full startup budget");
    assert.ok(texts.every((text) => new TextEncoder().encode(text).length <= 500));
    assert.equal(context.playback_enabled, true, "background context does not block conversational playback");
    assert.ok(context.frames.every((frame) => JSON.parse(frame).type === "session.context.append"));
    assert.deepEqual(JSON.parse(voice.sidebandOpened()).frames, context.frames, "lost control acknowledgements replay context");
    voice.framesSent(context.frames.length);
    assert.deepEqual(JSON.parse(voice.sidebandOpened()).frames, []);
  } finally {
    admit();
    voice.free();
  }
});

test("managed browser voice gives a UUIDv8 durable Agent a distinct UUIDv7 realtime session", async () => {
  const wasm = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  const module = await WebAssembly.compile(wasm);
  const requests = [];
  const agent = Agent.open(AGENT_ID, {
    baseUrl: "https://managed.example",
    fetch: async (input, init) => {
      const url = new URL(input);
      const body = init?.body === undefined ? undefined : JSON.parse(String(init.body));
      requests.push({ body, method: init?.method ?? "GET", path: url.pathname });
      if (url.pathname.endsWith("/realtime/start")) {
        return Response.json({
          context: {
            workspace: "/workspace",
            history: [{
              type: "message",
              role: "user",
              content: [{ type: "input_text", text: "continue the durable chat" }],
            }],
          },
        });
      }
      if (url.pathname.endsWith("/realtime/delegate")) {
        voice.agentEvent({ turnId: "voice-turn", event: { type: "assistant.message", payload: { text: "December 22." } } });
        return Response.json({ route: "started", turn_id: "voice-turn" });
      }
      if (url.pathname.endsWith("/turns/voice-turn/cancel")) {
        return Response.json({ turn_id: "voice-turn", state: "cancelling" });
      }
      if (url.pathname.endsWith("/realtime/stop")) {
        return Response.json({ stopped: true });
      }
      throw new Error(`unexpected managed voice request: ${url}`);
    },
  });
  const voice = await createManagedBrowserVoice(agent, "cove", { module });

  await voice.start();
  const call = JSON.parse(voice.callBody("v=managed-offer"));
  const provider = JSON.parse(call.call_body);
  assert.equal(call.managed_agent_id, AGENT_ID);
  assert.match(call.session_id, UUID_V7);
  assert.equal(call.realtime_session_id, call.session_id);
  assert.equal(call.thread_id, call.session_id);
  assert.equal(provider.session.model, "gpt-live-1-codex");
  assert.match(provider.session.instructions, /continue the durable chat/);
  const sideband = new URL(voice.sidebandUrl("rtc_managed"), "https://managed.example");
  assert.equal(sideband.searchParams.get("managed_agent_id"), AGENT_ID);
  assert.equal(sideband.searchParams.get("realtime_session_id"), call.session_id);
  assert.equal(sideband.searchParams.get("session_id"), call.session_id);
  assert.equal(sideband.searchParams.get("thread_id"), call.session_id);

  const context = { cursor: "9007199254740993", event: { type: "managed.voice.context", payload: {
    voice_session_id: call.session_id, result: { operation: "delete", key: { id: 5, version: 1 } },
  } } };
  const effects = (event) => JSON.parse(voice.agentEvent(event));
  assert.deepEqual(effects({ ...context, event: { ...context.event, payload: { ...context.event.payload, voice_session_id: "other-call" } } }).frames, []);
  const update = effects(context);
  const frame = JSON.parse(update.frames[0]);
  assert.equal(frame.type, "session.context.append");
  assert.equal(frame.channel, "commentary");
  assert.match(frame.content[0].text, /delete/);
  assert.equal(update.acknowledge_frames, true);
  assert.deepEqual(JSON.parse(voice.sidebandOpened()).frames, update.frames);
  voice.framesSent(1);
  assert.deepEqual(JSON.parse(voice.sidebandOpened()).frames, []);
  assert.deepEqual(effects(context).frames, [], "replay cannot restore obsolete facts");
  assert.deepEqual(effects({ ...context, cursor: "9007199254740992" }).frames, []);
  assert.equal(JSON.parse(voice.sidebandOpened()).playback_enabled, true);
  await voice.realtimeMessage(JSON.stringify({ type: "turn.done", turn: { role: "user", transcript: "When is Elena's birthday?" } }));
  assert.equal(requests.length, 1, "completed speech does not force agent work");
  const delegation = JSON.stringify({
    type: "delegation.created",
    item: {
      type: "delegation", target: "client", id: "delegation-1",
      content: [{ type: "input_text", text: "Look up Elena's birthday" }],
    },
  });
  const reply = JSON.parse(await voice.realtimeMessage(delegation));
  assert.match(reply.frames.join(""), /December 22/);
  assert.doesNotMatch(requests[1].body.input, /voice_bootstrap/);
  assert.match(requests[1].body.input, /When is Elena's birthday/);
  await voice.realtimeMessage(delegation);
  assert.equal(requests.length, 2, "a replayed delegation must not repeat admission");
  assert.equal(voice.agentEvent({ turnId: "typed-turn", event: { type: "run.started" } }), undefined);
  assert.equal(typeof voice.agentEvent({ turnId: "voice-turn", event: { type: "run.started" } }), "string");
  assert.equal(await voice.cancel(), true);
  await voice.stop();
  voice.free();

  assert.deepEqual(requests.map(({ method, path }) => [method, path]), [
    ["POST", `/v1/agents/${AGENT_ID}/realtime/start`],
    ["POST", `/v1/agents/${AGENT_ID}/realtime/delegate`],
    ["POST", `/v1/agents/${AGENT_ID}/turns/voice-turn/cancel`],
    ["POST", `/v1/agents/${AGENT_ID}/realtime/stop`],
  ]);
  assert.match(requests[1].body.input, /<realtime_delegation>/);
  assert.equal(requests[1].body.voice_session_id, call.session_id);
  assert.equal(typeof requests[1].body.operation_id, "string");
  assert.equal(typeof requests[0].body.operation_id, "string");
  assert.equal(typeof requests[3].body.operation_id, "string");
});

test("managed Agent voice uses its configured same-origin realtime routes", async () => {
  const requests = [];
  const agent = Agent.open(AGENT_ID, {
    baseUrl: "https://managed.example",
    fetch: async (input, init) => {
      requests.push({ request: new Request(input, init), init });
      return new Response("v=answer", {
        headers: { "x-nanocodex-realtime-location": "/v1/realtime/calls/rtc_managed" },
      });
    },
  });
  const transport = managedBrowserVoiceTransport(agent);
  const voiceSessionId = "019d2f5d-7491-7000-8000-000000000003";
  const providerBody = JSON.stringify({ sdp: "v=offer", session: { delegation: { type: "client" } } });
  const response = await transport.call(JSON.stringify({
    call_body: providerBody,
    managed_agent_id: AGENT_ID,
    realtime_session_id: voiceSessionId,
  }));
  assert.equal(await response.text(), "v=answer");
  assert.equal(new URL(requests[0].request.url).origin, "https://managed.example");
  assert.equal(new URL(requests[0].request.url).pathname, `/v1/agents/${AGENT_ID}/realtime/calls`);
  assert.equal(requests[0].init.body, providerBody);
  assert.equal(requests[0].request.headers.get("x-nanocodex-voice-session-id"), voiceSessionId);
  const sideband = transport.sidebandUrl("rtc_managed");
  assert.equal(sideband.origin, "wss://managed.example");
  assert.equal(sideband.pathname, `/v1/agents/${AGENT_ID}/realtime/sideband`);
  assert.equal(sideband.searchParams.get("call_id"), "rtc_managed");
  assert.equal(sideband.searchParams.get("voice_session_id"), voiceSessionId);
});

test("Voice.create refuses an ordinary managed Agent hosted on another browser origin", () => {
  const descriptor = Object.getOwnPropertyDescriptor(globalThis, "location");
  Object.defineProperty(globalThis, "location", {
    configurable: true,
    value: { origin: "https://consumer.example" },
  });
  try {
    const agent = Agent.open(AGENT_ID, { baseUrl: "https://managed.example" });
    assert.throws(
      () => Voice.create(agent),
      /same-origin managed Agent host; use Connect for cross-origin agents/,
    );
  } finally {
    if (descriptor) Object.defineProperty(globalThis, "location", descriptor);
    else delete globalThis.location;
  }
});
