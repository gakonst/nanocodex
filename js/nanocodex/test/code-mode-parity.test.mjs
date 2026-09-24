import assert from "node:assert/strict";
import { test } from "node:test";

import { createCodeRuntime } from "../runtime/code-runtime.mjs";

// Contract reference: openai/codex 506a328dab110591d3c1449a15217596e7e9cd61,
// code-mode-protocol/src/description.rs and code-mode-runtime/src/runtime/{value,callbacks}.rs.
const run = async (source) => JSON.parse(await createCodeRuntime().executeCode(source));
for (const value of ["", " \t\n"]) {
  test(`notify rejects whitespace ${JSON.stringify(value)}`, async () => {
    const result = await run(`notify(${JSON.stringify(value)});`);
    assert.equal(result.success, false);
    assert.match(result.output, /notify expects non-empty text/);
  });
}

test("notify preserves nonempty serialized content", async () => {
  const updates = [];
  const result = JSON.parse(await createCodeRuntime().executeCode('notify({ progress: 1 });', 'default', 'exec', update => updates.push(update)));
  assert.equal(result.success, true);
  assert.deepEqual(result.notifications, []);
  assert.deepEqual(updates, [{ type: 'notification', call_id: "exec", text: '{"progress":1}' }]);
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

for (const options of [{ yield_time_ms: -1 }, { max_output_tokens: 0.5 }, { unknown: null }]) {
  test(`exec retains invalid pragma rejection ${JSON.stringify(options)}`, async () => {
    let calls = 0;
    const runtime = createCodeRuntime({ probe: { handler() { calls++; } } });
    const result = JSON.parse(await runtime.executeCodeObserved(`// @exec: ${JSON.stringify(options)}\nawait tools.probe({});`));
    assert.equal(result.success, false);
    assert.equal(calls, 0);
  });
}
