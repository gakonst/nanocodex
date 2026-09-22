import assert from "node:assert/strict";
import test from "node:test";

import asyncVariant from "@jitl/quickjs-wasmfile-release-asyncify";
import { newQuickJSAsyncWASMModuleFromVariant } from "quickjs-emscripten-core";

import { createCodeRuntime } from "../runtime/code-runtime.mjs";
import { createQuickJsEvaluator } from "../runtime/quickjs-evaluator.mjs";

const quickJs = await newQuickJSAsyncWASMModuleFromVariant(asyncVariant);

test("QuickJS evaluator runs async Code Mode tools without host eval", async () => {
  const runtime = createCodeRuntime({
    add: {
      description: "Add two values.",
      parameters: { type: "object" },
      async handler({ left, right }) {
        await Promise.resolve();
        return left + right;
      },
    },
  }, { evaluate: createQuickJsEvaluator(quickJs) });

  const first = JSON.parse(await runtime.executeCode(`
    const sum = await tools.add({ left: 20, right: 22 });
    store("sum", sum);
    text({ sum, available: ALL_TOOLS.map((tool) => tool.name) });
  `, "quickjs", "exec-1"));
  assert.equal(first.success, true);
  assert.match(JSON.stringify(first.output), /sum/);
  assert.match(JSON.stringify(first.output), /42/);
  assert.deepEqual(first.nested_calls.map((call) => call.name), ["add"]);

  const second = JSON.parse(await runtime.executeCode(`text(load("sum"));`, "quickjs", "exec-2"));
  assert.equal(second.success, true);
  assert.match(JSON.stringify(second.output), /42/);
});

test("QuickJS keeps Promise.all tool calls concurrent through map and reduce", async () => {
  let active = 0;
  let peak = 0;
  const runtime = createCodeRuntime({
    exec_command: {
      description: "Run one bounded command.",
      supportsParallelToolCalls: true,
      parameters: { type: "object" },
      outputSchema: {
        type: "object",
        properties: { output: { type: "string" } },
        required: ["output"],
        additionalProperties: false,
      },
      async handler({ value, delay }) {
        active += 1;
        peak = Math.max(peak, active);
        await new Promise((resolve) => setTimeout(resolve, delay));
        active -= 1;
        return { exit_code: 0, output: String(value) };
      },
    },
  }, { evaluate: createQuickJsEvaluator(quickJs) });

  const execution = JSON.parse(await runtime.executeCode(`
    const commands = [
      { value: 20, delay: 60 },
      { value: 21, delay: 10 },
      { value: 1, delay: 30 },
    ].map((input) => tools.exec_command(input));
    const total = (await Promise.all(commands))
      .map(({ output }) => Number(output))
      .reduce((sum, value) => sum + value, 0);
    text({ total });
  `, "quickjs", "exec-concurrent"));

  assert.equal(execution.success, true);
  assert.equal(peak, 3);
  assert.deepEqual(
    JSON.parse(runtime.toolDefinitions())[0].output_schema.required,
    ["output"],
  );
  assert.deepEqual(
    execution.nested_calls.map((call) => call.call_id),
    [
      "exec-concurrent/code-1",
      "exec-concurrent/code-2",
      "exec-concurrent/code-3",
    ],
  );
  assert.equal(execution.output.at(-1).text, '{"total":42}');
});

test("QuickJS evaluator reports guest failures as Code Mode failures", async () => {
  const runtime = createCodeRuntime({}, { evaluate: createQuickJsEvaluator(quickJs) });
  const result = JSON.parse(await runtime.executeCode(`throw new Error("guest exploded")`));
  assert.equal(result.success, false);
  assert.match(result.output, /guest exploded/);
});

test("Code Mode cancellation aborts active nested tools", async () => {
  let started;
  let nestedSignal;
  const toolStarted = new Promise((resolve) => { started = resolve; });
  const runtime = createCodeRuntime({
    blocked: {
      async handler(_input, context) {
        nestedSignal = context.signal;
        started();
        await new Promise((resolve, reject) => {
          context.signal.addEventListener("abort", () => reject(new Error("nested tool cancelled")), {
            once: true,
          });
        });
      },
    },
  }, { evaluate: createQuickJsEvaluator(quickJs) });
  const execution = runtime.executeCode("await tools.blocked({});", "cancel", "exec-cancel");
  await toolStarted;
  runtime.cancel();
  const result = JSON.parse(await execution);
  assert.equal(result.success, false);
  assert.match(result.output, /Code Mode execution was cancelled/);
  assert.equal(nestedSignal.aborted, true);
});

test("cancelling a never-settling QuickJS cell leaves its serialized evaluator reusable", async () => {
  const runtime = createCodeRuntime({}, { evaluate: createQuickJsEvaluator(quickJs) });
  const pending = runtime.executeCode("await new Promise(() => {});", "wedged", "exec-wedged");
  await new Promise((resolve) => setImmediate(resolve));
  runtime.cancel("wedged");
  const cancelled = JSON.parse(await pending);
  assert.equal(cancelled.success, false);
  assert.match(cancelled.output, /Code Mode execution was cancelled/);

  const recovered = JSON.parse(await runtime.executeCode(
    'text("RECOVERED");',
    "wedged",
    "exec-recovered",
  ));
  assert.equal(recovered.success, true);
  assert.match(JSON.stringify(recovered.output), /RECOVERED/);
});

test("QuickJS preserves structured tool error messages, codes, and details", async () => {
  const failure = Object.assign(new Error("desktop capture failed"), {
    code: "SCREEN_CAPTURE_FAILED",
    details: { displayId: 7, retryable: false },
  });
  const runtime = createCodeRuntime({
    screenshot: { handler() { throw failure; } },
  }, { evaluate: createQuickJsEvaluator(quickJs) });

  const caught = JSON.parse(await runtime.executeCode(`
    try {
      await tools.screenshot({});
    } catch (error) {
      text({ isError: error instanceof Error, message: error.message,
        code: error.code, details: error.details, stack: error.stack });
    }
  `));
  assert.equal(caught.success, true);
  assert.deepEqual(JSON.parse(caught.output.at(-1).text), {
    isError: true,
    message: failure.message,
    code: failure.code,
    details: failure.details,
    stack: failure.stack,
  });

  const uncaught = JSON.parse(await runtime.executeCode("await tools.screenshot({});"));
  assert.equal(uncaught.success, false);
  assert.match(uncaught.output, /desktop capture failed/);
  assert.ok(uncaught.output.includes(failure.stack));
  assert.doesNotMatch(uncaught.output, /\[object Object\]/);
});

for (const failure of [
  "legacy tool failure",
  { message: "structured tool failure", code: 0, details: null },
  { code: "UNAVAILABLE", details: { reason: "no display" } },
]) {
  test(`QuickJS decodes tool failure ${JSON.stringify(failure)}`, async () => {
    const runtime = createCodeRuntime({
      fail: { handler() { throw failure; } },
    }, { evaluate: createQuickJsEvaluator(quickJs) });
    const result = JSON.parse(await runtime.executeCode(`
      try { await tools.fail({}); }
      catch (error) { text({ message: error.message, code: error.code, details: error.details }); }
    `));
    assert.equal(result.success, true);
    assert.deepEqual(JSON.parse(result.output.at(-1).text), typeof failure === "string"
      ? { message: failure }
      : { ...failure, message: failure.message ?? JSON.stringify(failure) });
  });
}


test("QuickJS safely serializes BigInt and circular tool error metadata", async () => {
  const shared = { displayId: 7n };
  const details = { first: shared, second: shared };
  details.self = details;
  const failure = Object.assign(new Error("desktop capture failed with metadata"), {
    code: 42n,
    details,
  });
  const runtime = createCodeRuntime({
    fail: { handler() { throw failure; } },
  }, { evaluate: createQuickJsEvaluator(quickJs) });

  const result = JSON.parse(await runtime.executeCode(`
    try { await tools.fail({}); }
    catch (error) { text({ message: error.message, code: error.code, details: error.details }); }
  `));
  assert.equal(result.success, true);
  assert.deepEqual(JSON.parse(result.output.at(-1).text), {
    message: failure.message,
    code: "42",
    details: {
      first: { displayId: "7" },
      second: { displayId: "7" },
      self: "[Circular]",
    },
  });
  const uncaught = JSON.parse(await runtime.executeCode("await tools.fail({});"));
  assert.equal(uncaught.success, false);
  assert.ok(uncaught.output.includes(failure.stack));
});
