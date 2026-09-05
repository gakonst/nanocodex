import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { Agent } from "../managed/index.mjs";
import { createManagedBrowserVoice } from "../managed/Voice.mjs";
import { managedBrowserVoiceTransport } from "../managed/internal.mjs";
import { Voice } from "../browser/index.mjs";

const AGENT_ID = "019d2f5d-7491-8000-8000-000000000001";
const UUID_V7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

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

  const delegation = JSON.stringify({
    type: "delegation.created",
    item: {
      type: "delegation",
      target: "client",
      id: "delegation-1",
      content: [{ type: "input_text", text: "ship it" }],
    },
  });
  await voice.realtimeMessage(delegation);
  await voice.realtimeMessage(delegation);
  assert.equal(requests[1].body.operation_id, requests[2].body.operation_id);
  assert.equal(voice.agentEvent({ turnId: "typed-turn", event: { type: "run.started" } }), undefined);
  assert.equal(typeof voice.agentEvent({ turnId: "voice-turn", event: { type: "run.started" } }), "string");
  assert.equal(await voice.cancel(), true);
  await voice.stop();
  voice.free();

  assert.deepEqual(requests.map(({ method, path }) => [method, path]), [
    ["POST", `/v1/agents/${AGENT_ID}/realtime/start`],
    ["POST", `/v1/agents/${AGENT_ID}/realtime/delegate`],
    ["POST", `/v1/agents/${AGENT_ID}/realtime/delegate`],
    ["POST", `/v1/agents/${AGENT_ID}/turns/voice-turn/cancel`],
    ["POST", `/v1/agents/${AGENT_ID}/realtime/stop`],
  ]);
  assert.match(requests[1].body.input, /<realtime_delegation>/);
  assert.equal(requests[1].body.voice_session_id, call.session_id);
  assert.equal(typeof requests[1].body.operation_id, "string");
  assert.equal(typeof requests[0].body.operation_id, "string");
  assert.equal(typeof requests[4].body.operation_id, "string");
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
