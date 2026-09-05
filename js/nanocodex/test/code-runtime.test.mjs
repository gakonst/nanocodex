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
