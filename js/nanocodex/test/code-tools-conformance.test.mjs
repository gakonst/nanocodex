import assert from "node:assert/strict";
import test from "node:test";
import asyncVariant from "@jitl/quickjs-wasmfile-release-asyncify";
import { newQuickJSAsyncWASMModuleFromVariant } from "quickjs-emscripten-core";
import { createCodeTools } from "nanocodex-tools/runtime/code-tools";
import { createCodeRuntime } from "../runtime/code-runtime.mjs";
import { createQuickJsEvaluator } from "../runtime/quickjs-evaluator.mjs";
import { createWorkerEvaluator } from "../runtime/worker-evaluator.mjs";
import { NodeWebWorker } from "./support/node-web-worker.mjs";

const quickJs = await newQuickJSAsyncWASMModuleFromVariant(asyncVariant);
const workerUrl = new URL("../runtime/code-evaluator.worker.mjs", import.meta.url);
const evaluators = [
  ["native", undefined],
  ["QuickJS", createQuickJsEvaluator(quickJs)],
  ["Worker", createWorkerEvaluator({ createWorker: () => new NodeWebWorker(workerUrl, { type: "module" }) })],
];

for (const [name, evaluate] of evaluators) {
  test(`${name}: missing third tool rejects locally and keeps catalog exact`, async () => {
    const dispatched = [];
    const runtime = createCodeRuntime(Object.fromEntries(["first", "second"].map((name) => [name, {
      handler: () => { dispatched.push(name); return name; },
    }])), evaluate ? { evaluate } : {});
    try {
      const result = JSON.parse(await runtime.executeCode(`
        await tools.first({});
        await tools.second({});
        const failures = [];
        for (const name of ["missing_third", "constructor", "toString", "__proto__", "hasOwnProperty"]) {
          let promise;
          try { promise = tools[name]({}); }
          catch (_) { throw new Error("missing tool threw synchronously"); }
          if (!(promise instanceof Promise)) throw new Error("missing tool did not return Promise");
          try { await promise; throw new Error("unexpected success"); }
          catch (error) { failures.push({ code: error.code, tool: error.tool, message: error.message }); }
        }
        text({ failures, keys: Object.keys(tools), catalog: ALL_TOOLS.map(({ name }) => name),
          own: Object.hasOwn(tools, "missing_third"), contains: "missing_third" in tools,
          nullPrototype: Object.getPrototypeOf(tools) === null, frozen: Object.isFrozen(tools),
          then: typeof tools.then, symbol: typeof tools[Symbol.iterator], awaited: await tools === tools });
      `));
      assert.equal(result.success, true, JSON.stringify(result));
      const output = JSON.parse(result.output.at(-1).text);
      assert.deepEqual(dispatched, ["first", "second"]);
      assert.deepEqual(result.nested_calls.map(({ name }) => name), dispatched);
      assert.deepEqual(output.keys, ["first", "second"]);
      assert.deepEqual(output.catalog, output.keys);
      assert.deepEqual({ ...output, failures: undefined, keys: undefined, catalog: undefined }, {
        failures: undefined, keys: undefined, catalog: undefined,
        own: false, contains: false, nullPrototype: true, frozen: true,
        then: "undefined", symbol: "undefined", awaited: true,
      });
      for (const failure of output.failures) {
        assert.equal(failure.code, "TOOL_NOT_AVAILABLE");
        assert.match(failure.message, new RegExp(failure.tool));
        assert.match(failure.message, /ALL_TOOLS/);
        assert.match(failure.message, /direct tool entry/);
      }
    } finally { runtime.reset(); }
  });

  test(`${name}: input serialization failures are asynchronous`, async () => {
    let dispatched = false;
    const runtime = createCodeRuntime({ first: { handler: () => { dispatched = true; } } }, evaluate ? { evaluate } : {});
    try {
      const result = JSON.parse(await runtime.executeCode(`
        const input = { toJSON() { throw new Error("serialization sentinel"); } };
        let promise;
        try { promise = tools.first(input); }
        catch (_) { throw new Error("synchronous serialization failure"); }
        if (!(promise instanceof Promise)) throw new Error("expected Promise");
        try { await promise; throw new Error("unexpected success"); }
        catch (error) { text({ rejected: true, message: error.message }); }
      `));
      assert.equal(result.success, true, JSON.stringify(result));
      assert.equal(JSON.parse(result.output.at(-1).text).rejected, true);
      assert.equal(dispatched, false);
    } finally { runtime.reset(); }
  });
}

test("factory preserves explicitly registered prototype names and then", async () => {
  const calls = [];
  const tools = createCodeTools(["constructor", "__proto__", "then"], (name) => { calls.push(name); return name; });
  for (const name of Object.keys(tools)) assert.equal(await tools[name](), name);
  assert.deepEqual(calls, ["constructor", "__proto__", "then"]);
});

for (const [name, evaluate] of evaluators) {
  test(`${name}: registered tool raw rejections preserve typeof and value`, async () => {
    const failures = ["upstream tool failure", null, false, 42,
      { message: "raw object", code: "RAW", details: { retryable: false } },
      ["raw", "array"], undefined];
    const runtime = createCodeRuntime({ fail: { handler({ index }) { throw failures[index]; } } },
      evaluate ? { evaluate } : {});
    try {
      const result = JSON.parse(await runtime.executeCode(`
        const failures = [];
        for (let index = 0; index < 7; index++) {
          failures.push(await tools.fail({ index }).catch((error) => ({
            type: typeof error, value: error, isError: error instanceof Error,
          })));
        }
        text(failures);
      `));
      assert.equal(result.success, true, JSON.stringify(result));
      assert.deepEqual(JSON.parse(result.output.at(-1).text), failures.map((value) => ({
        type: typeof value, ...(value !== undefined ? { value } : {}), isError: false,
      })));
    } finally { runtime.reset(); }
  });
}

for (const [name, evaluate] of evaluators) {
  test(`${name}: discarded missing and known failures stay cell-owned`, async () => {
    const runtime = createCodeRuntime({ fail: { handler() { throw new Error("discarded failure"); } } },
      evaluate ? { evaluate } : {});
    try {
      const result = JSON.parse(await runtime.executeCode(`
        void tools.missing();
        void tools.fail({});
        await new Promise((resolve) => setTimeout(resolve, 30));
        text("cell survived");
      `));
      assert.equal(result.success, true, JSON.stringify(result));
      assert.equal(result.output.at(-1).text, "cell survived");
    } finally { runtime.reset(); }
  });
}
