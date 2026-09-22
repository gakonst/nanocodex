import { test } from "node:test";
import assert from "node:assert/strict";
import { responseControlsBody, responseControlsSocket } from "../runtime/response-controls.mjs";
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

test("shared cache keys preserve session lineage without adding unsupported cache options", () => {
  const socket = new Socket();
  const controlled = responseControlsSocket(socket, { promptCacheKey: "owner-team-key" });
  for (const session of ["first-session", "second-session"]) {
    const request = { type: "response.create", prompt_cache_key: session, previous_response_id: `${session}-parent`, input: [
      { role: "developer", content: [{ type: "input_text", text: "stable instructions" }] },
      { role: "user", content: "hello" },
    ] };
    controlled.send(JSON.stringify(request));
    assert.deepEqual(socket.sent.at(-1), { ...request, prompt_cache_key: "owner-team-key" });
  }
  for (const promptCacheKey of ["", "x".repeat(65), 42]) {
    assert.throws(() => responseControlsSocket(socket, { promptCacheKey }), /invalid prompt cache key/);
  }
});

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

test("startup developer context preserves cache keys, stable prefix, and continuation lineage", () => {
  const socket = new Socket();
  const controlled = responseControlsSocket(socket, { promptCacheKey: "owner-team-key", promptCache: "explicit" });
  const stable = { role: "developer", content: [{ type: "input_text", text: "Baseline and static host instructions" }] };
  const startup = { role: "developer", content: [{ type: "input_text", text: '<startup_context><time>2026-09-16T19:00:00Z</time></startup_context>' }] };
  const first = { type: "response.create", input: [stable, startup, { role: "user", content: "first" }] };
  controlled.send(JSON.stringify(first));
  assert.deepEqual(socket.sent[0].input[0], stable);
  assert.equal(socket.sent[0].input[1].content[0].text, startup.content[0].text);
  assert.deepEqual(socket.sent[0].input[1].content[0].prompt_cache_breakpoint, { mode: "explicit" });
  controlled.send(JSON.stringify({ type: "response.create", previous_response_id: "first-response", input: [{ role: "user", content: "next" }] }));
  assert.equal(socket.sent[1].previous_response_id, "first-response");
  assert.deepEqual(socket.sent[1].input, [{ role: "user", content: "next" }]);
  assert.equal(socket.sent[0].prompt_cache_key, socket.sent[1].prompt_cache_key);
  // A full replay has the identical cacheable prefix, including the frozen timestamp.
  controlled.send(JSON.stringify({ ...first, input: [...first.input, { role: "user", content: "next" }] }));
  assert.deepEqual(socket.sent[2].input.slice(0, 2), socket.sent[0].input.slice(0, 2));
});


test("HTTPS and WebSocket requests apply identical response controls", () => {
  const controls = { promptCacheKey: "owner-team-key", promptCache: "explicit", outputSchema: { type: "object" } };
  const input = [{ role: "developer", content: [{ type: "input_text", text: "stable" }] },
    { role: "user", content: [{ type: "input_text", text: "question" }] }];
  const request = { model: "model", stream: true, input, text: { verbosity: "low" } };
  let sent;
  responseControlsSocket({ send(data) { sent = JSON.parse(data); } }, controls)
    .send(JSON.stringify({ ...request, type: "response.create" }));
  delete sent.type;
  const actual = JSON.parse(responseControlsBody(JSON.stringify(request), controls));
  assert.deepEqual(actual, sent);
  assert.equal(actual.prompt_cache_key, "owner-team-key");
  assert.deepEqual(actual.input[0].content[0].prompt_cache_breakpoint, { mode: "explicit" });
  assert.equal(actual.input[1].content[0].prompt_cache_breakpoint, undefined);
  assert.equal(actual.text.verbosity, "low");
  assert.equal(request.input[0].content[0].prompt_cache_breakpoint, undefined);
  for (const invalid of [{ promptCacheKey: "" }, { promptCache: "other" }, { outputSchema: [] }]) {
    assert.throws(() => responseControlsBody(JSON.stringify(request), invalid), TypeError);
  }
});

test("empty response controls do not parse or re-encode request bodies", () => {
  const socket = { send() {} };
  for (const controls of [undefined, {}, { promptCacheKey: undefined, outputSchema: undefined, promptCache: undefined }]) {
    assert.equal(responseControlsSocket(socket, controls), socket);
    // Deliberately not JSON: any parsing would throw rather than pass through.
    assert.equal(responseControlsBody("opaque encoded request", controls), "opaque encoded request");
  }
});
