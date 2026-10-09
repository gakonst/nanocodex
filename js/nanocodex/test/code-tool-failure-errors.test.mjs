import assert from "node:assert/strict";
import test from "node:test";

import asyncVariant from "@jitl/quickjs-wasmfile-release-asyncify";
import { newQuickJSAsyncWASMModuleFromVariant } from "quickjs-emscripten-core";

import { createCodeRuntime, toolResult } from "../runtime/code-runtime.mjs";
import { createQuickJsEvaluator } from "../runtime/quickjs-evaluator.mjs";

const quickJs = await newQuickJSAsyncWASMModuleFromVariant(asyncVariant);

// Hosts report handler failures as failed tool results (for example Claude's
// managed host and Hand routing). Guest code must catch real Error objects.
const failures = {
  unavailable: { handler: () => toolResult("Selected Hand route unavailable", null, { success: false }) },
  forbidden: { handler: () => toolResult("Thread inspection failed", null, { success: false,
    value: { message: "Thread inspection failed", code: "admin_threads_failed" } }) },
  mcp: { handler: () => toolResult('{"isError":true}', { isError: true, content: [] }, { success: false,
    value: { isError: true, content: [] } }) },
};

for (const [label, options] of [["host", {}], ["QuickJS", { evaluate: createQuickJsEvaluator(quickJs) }]]) {
  test(`${label} Code Mode rejects failed tool results with Error objects`, async () => {
    const runtime = createCodeRuntime(failures, options);
    const result = JSON.parse(await runtime.executeCode(`
      const seen = [];
      for (const name of ["unavailable", "forbidden", "mcp"]) {
        try { await tools[name]({}); seen.push({ name, resolved: true }); }
        catch (error) { seen.push({ name, isError: error instanceof Error, message: error.message,
          code: error.code ?? null, text: String(error), mcp: error.isError ?? null }); }
      }
      text(seen);
    `, "failures", "exec-failures"));
    assert.equal(result.success, true, JSON.stringify(result.output));
    const seen = JSON.parse(result.output.at(-1).text);
    assert.deepEqual(seen.map(({ name, isError }) => [name, isError]), [["unavailable", true], ["forbidden", true], ["mcp", true]]);
    assert.equal(seen[0].message, "Selected Hand route unavailable");
    assert.equal(seen[0].text, "Error: Selected Hand route unavailable");
    assert.equal(seen[1].message, "Thread inspection failed");
    assert.equal(seen[1].code, "admin_threads_failed");
    assert.equal(seen[2].mcp, true, "structured failure fields stay readable");
    assert.ok(result.nested_calls.every((call) => call.success === false));
    const uncaught = JSON.parse(await runtime.executeCode("await tools.unavailable({});", "failures", "exec-uncaught"));
    assert.equal(uncaught.success, false);
    assert.match(uncaught.output, /Selected Hand route unavailable/);
  });
}

