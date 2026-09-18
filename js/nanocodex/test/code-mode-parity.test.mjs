import assert from "node:assert/strict";
import { test } from "node:test";

import { createCodeRuntime } from "../runtime/code-runtime.mjs";

// Contract reference: openai/codex 36430b36881cf5c289cb48e671cfc9e8b542ae7b,
// code-mode-protocol/src/description.rs and code-mode-runtime/src/runtime/{value,callbacks}.rs.
const imageUrl = "data:image/png;base64,AAAA";
const run = async (source) => JSON.parse(await createCodeRuntime().executeCode(source));
const images = (result) => result.output.filter((item) => item.type === "input_image");

for (const [label, expression, expected] of [
  ["default", JSON.stringify(imageUrl), "high"],
  ["embedded detail", JSON.stringify({ image_url: imageUrl, detail: "LOW" }), "low"],
  ["explicit override", `${JSON.stringify({ image_url: imageUrl, detail: "low" })}, "ORIGINAL"`, "original"],
  ["MCP metadata", JSON.stringify({ type: "image", mimeType: "image/png", data: "AAAA", _meta: { "codex/imageDetail": "original" } }), "original"],
  ["invalid MCP metadata ignored", JSON.stringify({ type: "image", mimeType: "image/png", data: "AAAA", _meta: { "codex/imageDetail": "unexpected" } }), "high"],
  ["MCP existing data URL", JSON.stringify({ type: "image", data: imageUrl }), "high"],
  ["MCP snake-case MIME", JSON.stringify({ type: "image", mime_type: "image/png", data: "AAAA" }), "high"],
  ["image_url before MCP fields", JSON.stringify({ type: "image", image_url: imageUrl, detail: "low", data: "invalid", _meta: { "codex/imageDetail": "original" } }), "low"],
]) {
  test(`Codex image semantics: ${label}`, async () => {
    const result = await run(`image(${expression});`);
    assert.equal(result.success, true);
    assert.deepEqual(images(result), [{ type: "input_image", image_url: imageUrl, detail: expected }]);
  });
}

test("generatedImage preserves embedded detail and emits even an empty hint", async () => {
  const result = await run(`generatedImage(${JSON.stringify({ image_url: imageUrl, detail: "original", output_hint: "" })});`);
  assert.equal(result.success, true);
  assert.equal(images(result)[0].detail, "original");
  assert.deepEqual(result.output.at(-1), { type: "input_text", text: "" });
});

for (const outputHint of [null, 1, false, {}]) {
  test(`generatedImage rejects invalid hint ${JSON.stringify(outputHint)}`, async () => {
    const result = await run(`generatedImage(${JSON.stringify({ image_url: imageUrl, output_hint: outputHint })});`);
    assert.equal(result.success, false);
    assert.match(result.output, /output_hint must be a string when provided/);
  });
}

for (const value of ["", " \t\n"]) {
  test(`notify rejects whitespace ${JSON.stringify(value)}`, async () => {
    const result = await run(`notify(${JSON.stringify(value)});`);
    assert.equal(result.success, false);
    assert.match(result.output, /notify expects non-empty text/);
  });
}

test("notify preserves nonempty serialized content", async () => {
  const result = JSON.parse(await createCodeRuntime().executeCodeObserved('notify({ progress: 1 });'));
  assert.equal(result.success, true);
  assert.deepEqual(result.notifications, [{ call_id: "exec", text: '{"progress":1}' }]);
});

for (const source of ["", " \t\r\n", "// @exec: {}\n \t", "// @exec: {}\r\n\r\n"]) {
  test(`exec rejects empty source ${JSON.stringify(source)}`, async () => {
    const result = JSON.parse(await createCodeRuntime().executeCodeObserved(source));
    assert.equal(result.success, false);
    assert.match(result.output, /non-empty|followed by JavaScript source/);
  });
}

for (const options of [{ yield_time_ms: null }, { max_output_tokens: null }, { yield_time_ms: null, max_output_tokens: null }]) {
  test(`exec accepts null optional pragma fields ${JSON.stringify(options)}`, async () => {
    const result = JSON.parse(await createCodeRuntime().executeCodeObserved(`// @exec: ${JSON.stringify(options)}\r\ntext("ok");`));
    assert.equal(result.success, true);
    assert.match(JSON.stringify(result.output), /ok/);
  });
}

for (const options of [{ yield_time_ms: -1 }, { max_output_tokens: 0.5 }, { yield_time_ms: Number.MAX_SAFE_INTEGER + 1 }, { unknown: null }]) {
  test(`exec retains invalid pragma rejection ${JSON.stringify(options)}`, async () => {
    let calls = 0;
    const runtime = createCodeRuntime({ probe: { handler() { calls++; } } });
    const result = JSON.parse(await runtime.executeCodeObserved(`// @exec: ${JSON.stringify(options)}\nawait tools.probe({});`));
    assert.equal(result.success, false);
    assert.equal(calls, 0);
  });
}
