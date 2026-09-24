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

test("request observations omit content and cannot affect a sent request", async () => {
  const frames = [], observations = [];
  const socket = { send(data) { frames.push(data); return "sent"; } };
  const wrapped = responseControlsSocket(socket, { promptCacheKey: "private-cache-key" }, shape => {
    assert.equal(frames.length, observations.length + 1, "send precedes observation");
    observations.push(shape);
    assert.ok(Object.isFrozen(shape));
    return Promise.reject(new Error("observer unavailable"));
  });
  const request = { type: "response.create", model: "gpt-6-astra",
    reasoning: { effort: "low", context: "all_turns", private_field: "private-reasoning" },
    text: { verbosity: "low" }, service_tier: "default", tool_choice: "auto",
    parallel_tool_calls: false, store: false, stream: true, generate: true,
    previous_response_id: "private-response-id", include: ["reasoning.encrypted_content"],
    input: [{ role: "user", content: "private-input" }],
    tools: [{ name: "private-tool-name", parameters: { private_schema: "private" } }],
    metadata: { authorization: "private-secret" }, instructions: "private-instructions" };
  assert.equal(wrapped.send(JSON.stringify(request)), "sent");
  assert.deepEqual(JSON.parse(frames[0]), { ...request, prompt_cache_key: "private-cache-key" });
  assert.deepEqual(observations[0], { model: "gpt-6-astra", reasoning_effort: "low",
    reasoning_context: "all_turns", service_tier: "default", text_verbosity: "low", tool_choice: "auto",
    encoded_characters: frames[0].length, input_items: 1, tools_count: 1,
    cache_key_present: true, previous_response_present: true, encrypted_reasoning_included: true,
    parallel_tool_calls: false, store: false, stream: true, generate: true });
  assert.doesNotMatch(JSON.stringify(observations), /private|authorization|instructions|parameters|metadata/);
  await new Promise(resolve => setImmediate(resolve));
});

test("request observation alone preserves opaque frames, caps records and sanitizes enum values", () => {
  const frames = [], observations = [];
  const socket = { send(data) { frames.push(data); } };
  assert.equal(responseControlsSocket(socket), socket);
  const observed = responseControlsSocket(socket, {}, shape => observations.push(shape));
  observed.send("opaque-not-json");
  observed.send(new Uint8Array([1, 2, 3]));
  observed.send(JSON.stringify({ type: "response.steer", input: "private" }));
  const encoded = JSON.stringify({ type: "response.create", model: "private-model",
    reasoning: { effort: "private-effort", context: "private-context" },
    service_tier: "private-tier", text: { verbosity: "private-verbosity" },
    tool_choice: { function: { name: "private-function" } },
    input: "private", tools: "private", store: "private" });
  for (let i = 0; i < 40; i++) observed.send(encoded);
  assert.equal(frames.length, 43);
  assert.equal(frames[0], "opaque-not-json");
  assert.deepEqual(frames[1], new Uint8Array([1, 2, 3]));
  assert.ok(frames.slice(3).every(frame => frame === encoded));
  assert.equal(observations.length, 32);
  assert.doesNotMatch(JSON.stringify(observations), /private|function/);
  assert.equal(observations[0].model, "other_or_absent");
  assert.equal(observations[0].store, undefined);
  assert.throws(() => responseControlsSocket(socket, {}, true), /observer must be a function/);
});

test("failed sends produce no request observation and synchronous observer failure stays passive", () => {
  let count = 0;
  const broken = responseControlsSocket({ send() { throw Error("send failed"); } }, {}, () => count++);
  assert.throws(() => broken.send('{"type":"response.create"}'), /send failed/);
  assert.equal(count, 0);
  const live = responseControlsSocket({ send() { count++; return 42; } }, {}, () => { throw Error("observer failed"); });
  assert.equal(live.send('{"type":"response.create"}'), 42);
  assert.equal(count, 1);
});
