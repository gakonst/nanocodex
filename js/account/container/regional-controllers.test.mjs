import assert from "node:assert/strict";
import { registerHooks } from "node:module";
import test from "node:test";

// Exercise the shared controller against the container runtime boundary. Node
// cannot instantiate the Workers Container base class outside workerd.
const hook = registerHooks({
  resolve(specifier, context, nextResolve) {
    if (specifier === "@cloudflare/containers") return {
      shortCircuit: true,
      url: `data:text/javascript,${encodeURIComponent("export class Container { constructor(ctx) { this.ctx = ctx; } fetch(request) { return this.ctx.dispatch(request); } }")}`,
    };
    return nextResolve(specifier, context);
  },
});
const controllers = await import("../worker/chatGptEgress.ts");
hook.deregister();

const Controller = controllers.ChatGptEgress;
test(`${Controller.name} preserves voice RPC, SDP headers, and sanitized timing`, async (t) => {
  const records = [];
  t.mock.method(console, "info", (value) => records.push(value));
  const requests = [];
  const controller = new Controller({ container: { running: true }, dispatch: async (request) => {
    requests.push(request);
    return new Response("v=0\r\nanswer", { status: 201, headers: {
      location: "/calls/rtc_fixture", "content-type": "application/sdp",
      "x-nanocodex-relay-timing": JSON.stringify({ fetch_ms: 12, socket_reused: true, upload_ms: -1, secret: "excluded" }),
    } });
  } }, {});
  const response = await controller.createRealtimeCall('{"sdp":"v=0"}', {
    "content-type": "application/json", "x-session-id": "11111111-1111-4111-8111-111111111111",
  }, "?intent=quicksilver&architecture=avas");
  assert.equal(response.status, 201);
  assert.equal(response.body, "v=0\r\nanswer");
  assert.equal(response.headers.location, "/calls/rtc_fixture");
  assert.equal(response.headers["content-type"], "application/sdp");
  assert.equal(response.headers["x-nanocodex-relay-timing"], undefined);
  assert.equal(requests[0].url, "https://chatgpt-egress.internal/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas");
  assert.equal(await requests[0].text(), '{"sdp":"v=0"}');
  assert.equal(records[0].type, "voice.relay");
  assert.equal(records[0].fetch_ms, 12);
  assert.equal(records[0].socket_reused, true);
  assert.equal(records[0].upload_ms, undefined);
  assert.equal(records[0].secret, undefined);
  assert.equal(records[1].type, "voice.relay.body");
  assert.equal(records[1].transport, "rpc");
});
