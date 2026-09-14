import assert from "node:assert/strict";
import test from "node:test";
import { formatToolOutput, generatedOutputUrl, projectToolOutput } from "../agent/index.mjs";
import { applyAgentEvents, initialState } from "../agent/transcript.mjs";

const png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aP9sAAAAASUVORK5CYII=";

test("code-mode emitted content survives structured metadata and serialized output", () => {
  const emitted = JSON.stringify([
    { type: "input_text", text: "Script completed\nWall time 0.1 seconds\nOutput:" },
    { type: "input_text", text: "## Results\n**Generated** chart" },
    { type: "input_image", image_url: png },
  ]);
  const items = projectToolOutput({ success: true }, emitted, { image_url: png });
  assert.deepEqual(items, [
    { kind: "text", text: "## Results\n**Generated** chart" },
    { kind: "image", url: png, mimeType: "image/png" },
  ]);
});

test("MCP media and embedded resources retain playback and file content without binary diagnostics", () => {
  const content = { content: [
    { type: "image", mimeType: "image/png", data: png.split(",")[1] },
    { type: "audio", mimeType: "audio/wav", data: "UklGRg==" },
    { type: "resource_link", name: "Movie", uri: "https://example.test/movie.mp4", mimeType: "video/mp4" },
    { type: "resource", resource: { uri: "file:///report.csv", mimeType: "text/csv", text: "item,count\nA,2" } },
    { type: "resource", resource: { uri: "file:///report.pdf", mimeType: "application/pdf", blob: "JVBERi0=" } },
  ] };
  const items = projectToolOutput(content);
  assert.deepEqual(items.map(item => item.kind), ["image", "audio", "video", "file", "file"]);
  assert.equal(items[3].name, "report.csv");
  assert.match(items[3].url, /^data:text\/csv;charset=utf-8,item%2Ccount/);
  assert.equal(items[4].url, "data:application/pdf;base64,JVBERi0=");
  const detail = formatToolOutput(content);
  assert.doesNotMatch(detail, /iVBOR|UklGRg|JVBERi0/);
  assert.match(detail, /Embedded attachment/);
  assert.deepEqual(JSON.parse(formatToolOutput({ machines: [{ id: "machine-1", mount: "/mac" }] })).machines, [{ id: "machine-1", mount: "/mac" }]);
});

test("output URL policy rejects active or unresolved local destinations and bounds diagnostics", () => {
  for (const url of ["javascript:alert(1)", "file:///private/file.png", "/workspace/image.png", "sandbox:/mnt/data/image.png", "https://user:secret@example.test/file"]) {
    assert.equal(generatedOutputUrl(url, "image"), undefined);
  }
  assert.equal(generatedOutputUrl("data:text/html,<script>alert(1)</script>", "image"), undefined);
  assert.equal(generatedOutputUrl("data:image/png;base64,not base64!", "image"), undefined);
  const unavailable = projectToolOutput({ type: "resource_link", name: "Report", uri: "sandbox:/mnt/data/report.pdf" });
  assert.equal(unavailable[0].kind, "text");
  assert.match(unavailable[0].text, /Report/);
  const cyclic = {}; cyclic.output = cyclic;
  assert.deepEqual(projectToolOutput(cyclic), []);
  assert.ok(formatToolOutput("a".repeat(100_000)).length < 66_000);
});

test("history starting with an emitted tool result still projects the generated image", () => {
  const event = { request_id: "session", seq: 12, type: "tool.result", payload: {
    call_id: "exec-1", status: "completed", turn_id: "turn-1",
    structured_result: { success: true },
    result: JSON.stringify([{ type: "input_image", image_url: png }]),
  } };
  const state = applyAgentEvents(initialState(), [event]);
  assert.equal(state.entries.length, 1);
  assert.equal(state.entries[0].tool.generatedOutput[0].url, png);
  assert.equal(state.entries[0].turnId, "turn-1");
  const replayed = applyAgentEvents(state, [event]);
  assert.equal(replayed.entries.length, 1);
  const corrected = applyAgentEvents(state, [{ ...event, seq: 13, payload: { ...event.payload, status: "failed", result: null, structured_result: null } }]);
  assert.equal(corrected.entries[0].tool.generatedOutput, undefined);
  assert.equal(corrected.entries[0].tool.images, undefined);
  const textOnly = applyAgentEvents(initialState(), [{ ...event, payload: { ...event.payload, structured_result: null, result: { content: [{ type: "text", text: "Only emitted text" }] } } }]);
  assert.equal(textOnly.entries[0].tool.generatedOutput[0].text, "Only emitted text");
});

test("malformed Unicode and binary content inside text cannot break result projection", () => {
  const items = projectToolOutput({ type: "resource", resource: { mimeType: "text/csv", text: "name\n\ud800", uri: "file:///unicode.csv" } });
  assert.equal(items[0].kind, "file");
  assert.match(items[0].url, /%EF%BF%BD/);
  assert.doesNotMatch(formatToolOutput({ type: "input_text", text: `Preview: ![image](${png})` }), /iVBOR/);
  assert.doesNotMatch(formatToolOutput({ type: "input_image", data: "encoded-image-data" }), /encoded-image-data/);
});

test("the same emitted resource in both result forms does not create a raw JSON duplicate", () => {
  const resource = { type: "resource", resource: { uri: "file:///report.csv", mimeType: "text/csv", text: "a,b\n1,2" } };
  const emitted = [{ type: "input_text", text: JSON.stringify(resource) }];
  const items = projectToolOutput(emitted, JSON.stringify(emitted));
  assert.equal(items.length, 1);
  assert.equal(items[0].kind, "file");
  assert.match(projectToolOutput({ type: "input_text", text: '{"count":2}' })[0].text, /```json/);
});

test("a typed poll result keeps generated media on the original command with progress and exit status", () => {
  const event = (seq, type, payload) => ({ request_id: "session", seq, type, payload: { turn_id: "turn", ...payload } });
  const state = applyAgentEvents(initialState(), [
    event(1, "tool.call", { call_id: "command", tool: "exec_command", arguments: { cmd: "render" }, managed_event_created_at: 1_000 }),
    event(2, "tool.result", { call_id: "command", status: "completed", structured_result: { session_id: 42, output: "Rendering\n" } }),
    event(3, "tool.call", { call_id: "poll", tool: "write_stdin", arguments: { session_id: 42 } }),
    event(4, "tool.result", {
      call_id: "poll", status: "completed", managed_event_created_at: 4_000,
      structured_result: { exit_code: 7, output: "Partial result\n" },
      result: [{ type: "input_image", image_url: png }],
    }),
  ]);
  assert.equal(state.entries.length, 1, "the poll must not create a second generated-output row");
  const tool = state.entries[0].tool;
  assert.equal(tool.callId, "command");
  assert.equal(tool.status, "failed");
  assert.equal(tool.durationNs, 3_000_000_000);
  assert.equal(JSON.parse(tool.output).output, "Rendering\nPartial result\n");
  assert.equal(JSON.parse(tool.output).exit_code, 7);
  assert.ok(tool.generatedOutput.some(item => item.kind === "image" && item.url === png));
});
