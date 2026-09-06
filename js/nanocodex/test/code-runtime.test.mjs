import assert from "node:assert/strict";
import { test } from "node:test";

import { createCodeRuntime } from "../runtime/code-runtime.mjs";

test("nested Code Mode admits at most 128 explicitly parallel-safe calls", async () => {
  let active = 0;
  let maximum = 0;
  const releases = [];
  const saturated = deferred();
  const runtime = createCodeRuntime({
    probe: {
      supportsParallelToolCalls: true,
      async handler() {
        active += 1;
        maximum = Math.max(maximum, active);
        if (active === 128) saturated.resolve();
        await new Promise((resolve) => releases.push(resolve));
        active -= 1;
        return "done";
      },
    },
  });

  const execution = runtime.executeCode(
    "await Promise.all(Array.from({ length: 129 }, () => tools.probe({})));",
    "bounded",
    "exec-bounded",
  );
  await withDeadline(saturated.promise, 1_000, "nested calls did not saturate");
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(active, 128);
  assert.equal(maximum, 128);

  for (const release of releases.slice(0, 128)) release();
  while (releases.length < 129) await new Promise((resolve) => setImmediate(resolve));
  releases[128]();
  const completed = JSON.parse(await execution);
  assert.equal(completed.success, true);
  assert.equal(completed.nested_calls.length, 129);
  assert.equal(maximum, 128);
});

test("an unsafe nested call excludes safe siblings through a fair per-cell gate", async () => {
  const firstStarted = deferred();
  const releaseFirst = deferred();
  const unsafeStarted = deferred();
  const releaseUnsafe = deferred();
  const secondStarted = deferred();
  const order = [];
  const runtime = createCodeRuntime({
    safe: {
      supportsParallelToolCalls: true,
      async handler({ id }) {
        order.push(`start:${id}`);
        if (id === "first") {
          firstStarted.resolve();
          await releaseFirst.promise;
        } else {
          secondStarted.resolve();
        }
        order.push(`end:${id}`);
        return id;
      },
    },
    unsafe: {
      async handler() {
        order.push("start:unsafe");
        unsafeStarted.resolve();
        await releaseUnsafe.promise;
        order.push("end:unsafe");
        return "unsafe";
      },
    },
  });
  const execution = runtime.executeCode(`
    await Promise.all([
      tools.safe({ id: "first" }),
      tools.unsafe({}),
      tools.safe({ id: "second" }),
    ]);
  `, "exclusive", "exec-exclusive");

  await firstStarted.promise;
  assert.equal(unsafeStarted.settled, false);
  assert.equal(secondStarted.settled, false);
  releaseFirst.resolve();
  await unsafeStarted.promise;
  assert.equal(secondStarted.settled, false);
  releaseUnsafe.resolve();
  await secondStarted.promise;

  const completed = JSON.parse(await execution);
  assert.equal(completed.success, true);
  assert.deepEqual(order, [
    "start:first",
    "end:first",
    "start:unsafe",
    "end:unsafe",
    "start:second",
    "end:second",
  ]);
});

test("nested lifecycle updates are observed at start and completion boundaries", async () => {
  const started = deferred();
  const release = deferred();
  const updates = [];
  const runtime = createCodeRuntime({
    blocked: {
      async handler() {
        started.resolve();
        await release.promise;
        return { answer: 42 };
      },
    },
  });
  const execution = runtime.executeCode(
    "await tools.blocked({ value: 21 });",
    "observed",
    "exec-observed",
    (update) => updates.push(structuredClone(update)),
  );

  await started.promise;
  assert.equal(updates.length, 1);
  assert.equal(updates[0].type, "nested_call_started");
  assert.match(updates[0].call_id, /^exec-observed\/code-\d+$/);
  assert.equal(updates[0].name, "blocked");
  assert.deepEqual(updates[0].input, { value: 21 });
  release.resolve();
  const completed = JSON.parse(await execution);
  assert.equal(completed.success, true);
  assert.deepEqual(updates.map(({ type }) => type), [
    "nested_call_started",
    "nested_call_completed",
  ]);
  assert.equal(updates[1].call.success, true);
  assert.equal(updates[1].call.call_id, updates[0].call_id);
  assert.deepEqual(updates[1].call.structured_result, { answer: 42 });
});

for (const [label, source] of [
  ["normal completion", "void tools.blocked({});"],
  ["exit", "void tools.blocked({}); exit();"],
]) test(`a cell owns discarded nested tool promises through ${label}`, async () => {
  const started = deferred();
  let nestedSignal;
  const runtime = createCodeRuntime({
    blocked: {
      async handler(_input, context) {
        nestedSignal = context.signal;
        started.resolve();
        await new Promise((resolve) => context.signal.addEventListener("abort", resolve, {
          once: true,
        }));
        return "cancelled";
      },
    },
  });

  const execution = runtime.executeCode(
    source,
    "discarded",
    "exec-discarded",
  );
  await started.promise;
  const beforeCancel = await Promise.race([
    execution.then(() => "settled"),
    new Promise((resolve) => setTimeout(() => resolve("pending"), 10)),
  ]);
  assert.equal(beforeCancel, "pending");

  runtime.cancel("discarded");
  const completed = JSON.parse(await withDeadline(
    execution,
    1_000,
    "discarded nested tool did not cancel",
  ));
  assert.equal(nestedSignal.aborted, true);
  assert.equal(completed.success, false);
  assert.match(completed.output, /Code Mode execution was cancelled/);
});

function deferred() {
  let resolve;
  const state = {
    settled: false,
    promise: new Promise((accept) => { resolve = accept; }),
    resolve(value) {
      if (state.settled) return;
      state.settled = true;
      resolve(value);
    },
  };
  return state;
}

async function withDeadline(promise, milliseconds, message) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_resolve, reject) => {
        timer = setTimeout(() => reject(new Error(message)), milliseconds);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function observed(runtime, promise, sessionId, callId) {
  const updates = [];
  for (;;) {
    const update = await runtime.nextCodeUpdate(sessionId, callId);
    if (update === null) break;
    updates.push(JSON.parse(update));
  }
  return { ...JSON.parse(await promise), updates };
}

function outputText(output) {
  return typeof output === "string" ? output : output.filter((item) => item.type === "input_text").map((item) => item.text).join("\n");
}

test("yielded cells keep nested call identities, drain output, and fence sessions", async () => {
  const release = deferred();
  const runtime = createCodeRuntime({
    "example.echo": { async handler() { await release.promise; return "second-chunk"; } },
  });
  const initial = await observed(runtime, runtime.executeCodeObserved(
    '// @exec: {"yield_time_ms": 0}\ntext("first-chunk"); text(await tools.example_echo({}));',
    "owner", "exec-1",
  ), "owner", "exec-1");
  assert.equal(initial.success, true);
  const cellId = outputText(initial.output).match(/Script running with cell ID ([^\s]+)/)[1];
  assert.match(outputText(initial.output), /first-chunk/);
  assert.equal(initial.nested_calls.length, 0);
  assert.equal(initial.updates[0].type, "nested_call_started");

  const fenced = await observed(runtime, runtime.waitCodeObserved(JSON.stringify({ cell_id: cellId }), "other", "wait-foreign"), "other", "wait-foreign");
  assert.equal(fenced.success, false);
  assert.match(fenced.output, /not found/);

  const waiting = runtime.waitCodeObserved(JSON.stringify({ cell_id: cellId }), "owner", "wait-1");
  const collected = observed(runtime, waiting, "owner", "wait-1");
  await Promise.resolve();
  const busy = await observed(runtime, runtime.waitCodeObserved(JSON.stringify({ cell_id: cellId }), "owner", "wait-busy"), "owner", "wait-busy");
  assert.equal(busy.success, false);
  assert.match(busy.output, /active observer/);
  release.resolve();
  const completed = await collected;
  assert.equal(completed.success, true);
  assert.match(outputText(completed.output), /Script completed/);
  assert.match(outputText(completed.output), /second-chunk/);
  assert.doesNotMatch(outputText(completed.output), /first-chunk/);
  assert.equal(completed.nested_calls.length, 1);
  assert.equal(completed.nested_calls[0].call_id, initial.updates[0].call_id);
  assert.equal(completed.updates[0].type, "nested_call_completed");
  const closed = await observed(runtime, runtime.waitCodeObserved(JSON.stringify({ cell_id: cellId }), "owner", "wait-closed"), "owner", "wait-closed");
  assert.equal(closed.success, false);
  runtime.reset();
});

test("terminate cancels nested work and never restarts a missing cell", async () => {
  let calls = 0;
  let nestedSignal;
  const runtime = createCodeRuntime({
    blocked: { async handler(_input, { signal }) {
      calls++;
      nestedSignal = signal;
      await new Promise((resolve) => signal.addEventListener("abort", resolve, { once: true }));
      return "cancelled";
    } },
  });
  const started = await observed(runtime, runtime.executeCodeObserved(
    '// @exec: {"yield_time_ms": 0}\nawait tools.blocked({});', "owner", "exec-terminate",
  ), "owner", "exec-terminate");
  const cellId = outputText(started.output).match(/cell ID ([^\s]+)/)[1];
  const stopped = await observed(runtime, runtime.waitCodeObserved(JSON.stringify({ cell_id: cellId, terminate: true }), "owner", "terminate"), "owner", "terminate");
  assert.equal(stopped.success, true);
  assert.match(outputText(stopped.output), /Script terminated/);
  assert.equal(nestedSignal.aborted, true);
  const missing = await observed(runtime, runtime.waitCodeObserved(JSON.stringify({ cell_id: cellId }), "owner", "after"), "owner", "after");
  assert.equal(missing.success, false);
  assert.equal(calls, 1);
  runtime.reset();
});

for (const evaluator of ["native", "quickjs", "worker"]) test(`${evaluator} supports MCP media, notifications, explicit yields, and bounded Unicode output`, async () => {
  let evaluate;
  if (evaluator === "quickjs") {
    const { default: variant } = await import("@jitl/quickjs-wasmfile-release-asyncify");
    const { newQuickJSAsyncWASMModuleFromVariant } = await import("quickjs-emscripten-core");
    const { createQuickJsEvaluator } = await import("../runtime/quickjs-evaluator.mjs");
    evaluate = createQuickJsEvaluator(await newQuickJSAsyncWASMModuleFromVariant(variant));
  } else if (evaluator === "worker") {
    const { NodeWebWorker } = await import("./support/node-web-worker.mjs");
    const { createWorkerEvaluator } = await import("../runtime/worker-evaluator.mjs");
    evaluate = createWorkerEvaluator({ createWorker: () => new NodeWebWorker(new URL("../runtime/code-evaluator.worker.mjs", import.meta.url)) });
  }
  const runtime = createCodeRuntime({}, { evaluate });
  const first = await observed(runtime, runtime.executeCodeObserved(`
    text("before");
    notify("progress");
    yield_control();
    await new Promise((resolve) => setTimeout(resolve, 20));
    image({ type: "image", data: "AAAA", mimeType: "image/png", _meta: { "codex/imageDetail": "original" } });
    audio({ type: "audio", data: "AAAA", mimeType: "audio/wav" });
    text("世界".repeat(200));
  `, "helpers", "exec-helpers"), "helpers", "exec-helpers");
  assert.deepEqual(first.notifications, [{ call_id: "exec-helpers", text: "progress" }]);
  const cellId = outputText(first.output).match(/cell ID ([^\s]+)/)[1];
  let last;
  for (let index = 0; index < 10; index++) {
    const callId = `wait-helpers-${index}`;
    last = await observed(runtime, runtime.waitCodeObserved(JSON.stringify({ cell_id: cellId, max_tokens: 10 }), "helpers", callId), "helpers", callId);
    if (!outputText(last.output).includes("Script running")) break;
  }
  assert.equal(last.success, true);
  assert.deepEqual(last.notifications, []);
  assert.equal(last.output.find((item) => item.type === "input_image").detail, "original");
  assert.equal(last.output.find((item) => item.type === "input_audio").audio_url, "data:audio/wav;base64,AAAA");
  assert.match(outputText(last.output), /output truncated/);
  assert.doesNotMatch(outputText(last.output), /�/);
  runtime.reset();
});

test("turn cancellation preserves untouched cells from earlier turns", async () => {
  const releases = [deferred(), deferred()];
  const signals = [];
  const runtime = createCodeRuntime({ blocked: {
    supportsParallelToolCalls: true,
    async handler({ index }, { signal }) {
      signals[index] = signal;
      signal.addEventListener("abort", () => releases[index].resolve(), { once: true });
      await releases[index].promise;
      return index;
    },
  } });
  runtime.beginTurn("owner");
  const first = await observed(runtime, runtime.executeCodeObserved(
    '// @exec: {"yield_time_ms":0}\ntext(await tools.blocked({index:0}));', "owner", "old",
  ), "owner", "old");
  const oldId = outputText(first.output).match(/cell ID ([^\s]+)/)[1];
  runtime.beginTurn("owner");
  await observed(runtime, runtime.executeCodeObserved(
    '// @exec: {"yield_time_ms":0}\ntext(await tools.blocked({index:1}));', "owner", "new",
  ), "owner", "new");
  runtime.cancelTurn("owner");
  assert.equal(signals[0].aborted, false);
  assert.equal(signals[1].aborted, true);
  releases[0].resolve();
  const completed = await observed(runtime, runtime.waitCodeObserved(JSON.stringify({ cell_id: oldId }), "owner", "resume-old"), "owner", "resume-old");
  assert.equal(completed.success, true);
  assert.match(outputText(completed.output), /Script completed/);
  runtime.releaseSession("owner");
});

for (const pragma of ['{"yield_time_ms":-1}', '{"max_output_tokens":1.5}', '{"unknown":1}', '[]']) {
  test(`invalid cell pragma is rejected before invoking tools: ${pragma}`, async () => {
    let calls = 0;
    const runtime = createCodeRuntime({ touch: { handler() { calls++; } } });
    const result = await observed(runtime, runtime.executeCodeObserved(`// @exec: ${pragma}\nawait tools.touch({});`, "invalid", "exec-invalid"), "invalid", "exec-invalid");
    assert.equal(result.success, false);
    assert.equal(calls, 0);
    runtime.reset();
  });
}
