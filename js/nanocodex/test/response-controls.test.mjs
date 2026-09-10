import { test } from "node:test";
import assert from "node:assert/strict";
import { responseControlsSocket } from "../runtime/response-controls.mjs";
import { multiplex } from "../browser/Transport.mjs";

test("schema/cache controls affect provider creates and preserve continuation lineage", () => {
  const sent = [];
  const socket = { readyState: 1, send(data) { assert.equal(this, socket); sent.push(JSON.parse(data)); } };
  const wrapped = responseControlsSocket(socket, { promptCache: "explicit", outputSchema: { type: "object" } });
  wrapped.send(JSON.stringify({ type: "response.create", previous_response_id: "parent", input: [
    { role: "developer", content: [{ type: "input_text", text: "stable" }] },
    { role: "user", content: [{ type: "input_text", text: "changes" }] },
  ] }));
  assert.equal(sent[0].previous_response_id, "parent");
  assert.deepEqual(sent[0].prompt_cache_options, { mode: "explicit", ttl: "30m" });
  assert.deepEqual(sent[0].input[0].content[0].prompt_cache_breakpoint, { mode: "explicit" });
  assert.equal(sent[0].input[1].content[0].prompt_cache_breakpoint, undefined);
  assert.equal(sent[0].text.format.strict, true);
  wrapped.send(JSON.stringify({ type: "response.create", input: [{ role: "user", content: "followup" }] }));
  assert.equal(sent[1].input[0].prompt_cache_breakpoint, undefined);
  wrapped.send(JSON.stringify({ type: "response.steer", input: [] }));
  assert.deepEqual(sent[2], { type: "response.steer", input: [] });
});

class Socket extends EventTarget {
  readyState = 1; bufferedAmount = 0; sent = [];
  send(data) { this.sent.push(JSON.parse(data)); }
  close() { this.readyState = 3; this.dispatchEvent(new Event("close")); }
  message(body) { this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(body) })); }
}
test("multiplexed lanes isolate interleaved events, scoped failures, and socket lifetime", () => {
  const socket = new Socket(); const pool = multiplex(socket);
  const a = pool.lane("a"), b = pool.lane("b"); const seenA = [], seenB = [];
  a.addEventListener("message", e => seenA.push(JSON.parse(e.data)));
  b.addEventListener("message", e => seenB.push(JSON.parse(e.data)));
  a.send(JSON.stringify({ type: "response.create", input: "A", stream: true }));
  b.send(JSON.stringify({ type: "response.create", input: "B", previous_response_id: "parent" }));
  assert.equal(socket.sent[0].stream_id, "a"); assert.equal(socket.sent[0].stream, undefined);
  assert.equal(socket.sent[1].previous_response_id, "parent");
  socket.message({ type: "response.completed", stream_id: "b" });
  socket.message({ type: "error", stream_id: "a" });
  assert.equal(seenA.length, 1); assert.equal(seenB.length, 1);
  socket.message({ type: "error", error: { code: "connection_failed" } });
  assert.equal(seenA.length, 2); assert.equal(seenB.length, 2);
  a.close(); assert.equal(a.readyState, 3); assert.equal(socket.readyState, 1); assert.equal(b.readyState, 1);
  assert.throws(() => b.send(JSON.stringify({ type: "response.create", stream_id: "a" })));
  pool.close(); assert.equal(b.readyState, 3); assert.equal(socket.readyState, 3);
});
test("lane limit counts all IDs used over the connection lifetime", () => {
  const pool = multiplex(new Socket());
  for (let n = 0; n < 32; n++) pool.lane(String(n)).close();
  assert.throws(() => pool.lane("33"), /32/);
  assert.equal(pool.lane("0").readyState, 1);
  assert.throws(() => pool.lane("invalid space")); pool.close();
});

test("cache controls compose with a lane whose send property is immutable", () => {
  const socket = new Socket(); const pool = multiplex(socket);
  const controlled = responseControlsSocket(pool.lane("cache"), { promptCache: "implicit" });
  controlled.send(JSON.stringify({ type: "response.create", input: [] }));
  assert.equal(socket.sent[0].stream_id, "cache");
  assert.equal(socket.sent[0].prompt_cache_options.mode, "implicit");
  pool.close();
});
