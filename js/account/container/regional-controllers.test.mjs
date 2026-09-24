import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import test from "node:test";
import ts from "typescript";

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
test(`${Controller.name} returns the exact text WebSocket upgrade`, async (t) => {
  t.mock.method(console, "info", () => {});
  const upgrade = { status: 101, webSocket: {} };
  const controller = new Controller({ container: { running: true }, dispatch: async (request) => {
    assert.equal(request.headers.has("x-nanocodex-egress-request-id"), false);
    assert.match(request.headers.get("x-nanocodex-relay-id"), /^[0-9a-f-]{36}$/);
    return upgrade;
  } }, {});
  const response = await controller.fetch(new Request("https://chatgpt-egress.internal/backend-api/codex/responses", {
    headers: { upgrade: "websocket", "x-nanocodex-egress-request-id": "11111111-1111-4111-8111-111111111111" },
  }));
  assert.equal(response, upgrade);
  assert.equal(response.webSocket, upgrade.webSocket);
});

test("regional container configs have matching account/egress bindings, migration, and exports", () => {
  const regionalNames = ["Wnam", "Enam", "Weur", "Eeur", "Apac", "Sam", "Oc"];
  const readConfig = (relative) => {
    const file = new URL(relative, import.meta.url);
    const parsed = ts.parseConfigFileTextToJson(file.pathname, readFileSync(file, "utf8"));
    assert.equal(parsed.error, undefined);
    return parsed.config;
  };
  const account = readConfig("../wrangler.jsonc");
  const broker = readConfig("../../egress/wrangler.broker.jsonc");
  const entry = readFileSync(new URL("../worker/entry.ts", import.meta.url), "utf8");
  const migration = account.migrations.find((m) => m.tag === "v3-regional-egress");
  assert.deepEqual(migration.new_sqlite_classes, regionalNames.map((region) => `ChatGptEgress${region}`));
  const regional = account.containers.filter((c) => c.class_name !== "ChatGptEgress");
  for (const name of regionalNames) {
    const class_name = `ChatGptEgress${name}`;
    const bindingName = `CHATGPT_EGRESS_${name.toUpperCase()}`;
    const config = regional.find((c) => c.class_name === class_name);
    assert.deepEqual(config.constraints.regions, name === "Oc" ? ["OC", "APAC"] : [name.toUpperCase()]);
    assert.deepEqual(account.durable_objects.bindings.find((b) => b.name === bindingName), { name: bindingName, class_name });
    assert.deepEqual(broker.durable_objects.bindings.find((b) => b.name === bindingName), { name: bindingName, class_name, script_name: "nanocodex" });
    assert.match(entry, new RegExp(`\\b${class_name}\\b`));
    assert.equal(Object.getPrototypeOf(controllers[class_name]), controllers.ChatGptEgress);
  }
});
