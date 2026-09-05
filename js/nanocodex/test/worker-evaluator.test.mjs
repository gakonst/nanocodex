import assert from "node:assert/strict";
import { test } from "node:test";

import { createCodeRuntime } from "../runtime/code-runtime.mjs";
import { createWorkerEvaluator } from "../runtime/worker-evaluator.mjs";
import { NodeWebWorker } from "./support/node-web-worker.mjs";

const PROTOCOL = "nanocodex.code-evaluator.v1";
const WORKER_URL = new URL("../runtime/code-evaluator.worker.mjs", import.meta.url);

test("real module evaluator preserves Code Mode globals and terminal store commits", async () => {
  const logs = [];
  const runtime = createCodeRuntime({
    double: {
      description: "Double one value.",
      parameters: { type: "object" },
      handler: ({ value }) => value * 2,
    },
  }, {
    console: { log: (...values) => logs.push(values) },
    evaluate: createWorkerEvaluator({
      createWorker: () => new NodeWebWorker(WORKER_URL, {
        name: "nanocodex-code-evaluator",
        type: "module",
      }),
    }),
  });

  const completed = JSON.parse(await runtime.executeCode(`
    if (!ALL_TOOLS.some((tool) => tool.name === "double")) throw new Error("missing tools");
    const answer = await tools.double({ value: 21 });
    store("answer", answer);
    console.log("answer", answer);
    image("data:image/png;base64,a", "high");
    generatedImage({ image_url: "data:image/png;base64,b", output_hint: "generated" });
    text(load("answer"));
    exit();
    text("unreachable");
  `, "real-worker", "exec-real"));
  assert.equal(completed.success, true);
  assert.deepEqual(completed.nested_calls[0].structured_result, 42);
  assert.deepEqual(logs, [["answer", "42"]]);
  assert.equal(completed.output.some((item) =>
    item.type === "input_image" && item.image_url === "data:image/png;base64,a"), true);
  assert.equal(completed.output.some((item) =>
    item.type === "input_image" && item.image_url === "data:image/png;base64,b"), true);
  assert.equal(JSON.stringify(completed.output).includes("unreachable"), false);

  const failed = JSON.parse(await runtime.executeCode(
    'store("ordinary-failure", "retained"); throw new Error("expected failure");',
    "real-worker",
    "exec-failed",
  ));
  assert.equal(failed.success, false);
  assert.match(failed.output, /expected failure/);

  const followOn = JSON.parse(await runtime.executeCode(
    'text({ answer: load("answer"), failure: load("ordinary-failure") });',
    "real-worker",
    "exec-follow-on",
  ));
  assert.equal(followOn.success, true);
  assert.match(JSON.stringify(followOn.output), /retained/);
  runtime.reset();
});

test("evaluator cancellation aborts nested work before termination and recreates cleanly", async () => {
  FakeWorker.reset();
  const order = [];
  const nestedStarted = deferred();
  const runtime = createCodeRuntime({
    blocked: {
      async handler(_input, context) {
        nestedStarted.resolve();
        await new Promise((_resolve, reject) => {
          context.signal.addEventListener("abort", () => {
            order.push("nested-aborted");
            reject(context.signal.reason);
          }, { once: true });
        });
      },
    },
  }, {
    evaluate: createWorkerEvaluator({ createWorker: () => new FakeWorker() }),
  });

  const pending = runtime.executeCode(
    "await tools.blocked({});",
    "cancelled-session",
    "exec-cancelled",
  );
  const first = await FakeWorker.next();
  const evaluation = first.incoming[0];
  first.emit({
    protocol: PROTOCOL,
    evaluationId: evaluation.evaluationId,
    type: "tool.call",
    id: 1,
    name: "blocked",
    input: {},
  });
  await nestedStarted.promise;
  const lateHandler = first.onmessage;
  first.onTerminate = () => order.push("evaluator-terminated");

  runtime.cancel("cancelled-session");
  const cancelled = JSON.parse(await pending);
  assert.equal(cancelled.success, false);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(order, ["nested-aborted", "evaluator-terminated"]);
  assert.equal(first.terminated, true);

  lateHandler({ data: {
    protocol: PROTOCOL,
    evaluationId: evaluation.evaluationId,
    type: "completed",
    storedWrites: [["leaked", true]],
  } });

  const recoveredPromise = runtime.executeCode(
    'text("RECOVERED");',
    "cancelled-session",
    "exec-recovered",
  );
  const second = await FakeWorker.next();
  assert.notEqual(second, first);
  const recovery = second.incoming[0];
  assert.deepEqual(recovery.storedEntries, []);
  second.emit({
    protocol: PROTOCOL,
    evaluationId: recovery.evaluationId,
    type: "output",
    kind: "text",
    value: "RECOVERED",
  });
  second.emit({
    protocol: PROTOCOL,
    evaluationId: recovery.evaluationId,
    type: "completed",
    storedWrites: [["healthy", 42]],
  });
  const recovered = JSON.parse(await recoveredPromise);
  assert.equal(recovered.success, true);
  assert.match(JSON.stringify(recovered.output), /RECOVERED/);

  const storedPromise = runtime.executeCode(
    'text(load("healthy"));',
    "cancelled-session",
    "exec-stored",
  );
  const third = await FakeWorker.next();
  const stored = third.incoming[0];
  assert.deepEqual(stored.storedEntries, [["healthy", 42]]);
  third.emit({
    protocol: PROTOCOL,
    evaluationId: stored.evaluationId,
    type: "completed",
    storedWrites: [],
  });
  assert.equal(JSON.parse(await storedPromise).success, true);
});

test("session-scoped cancellation terminates only that cell's evaluator", async () => {
  FakeWorker.reset();
  const runtime = createCodeRuntime({}, {
    evaluate: createWorkerEvaluator({ createWorker: () => new FakeWorker() }),
  });
  const firstPending = runtime.executeCode("await new Promise(() => {});", "session-a", "exec-a");
  const first = await FakeWorker.next();
  const secondPending = runtime.executeCode("await new Promise(() => {});", "session-b", "exec-b");
  const second = await FakeWorker.next();

  runtime.cancel("session-a");
  assert.equal(JSON.parse(await firstPending).success, false);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(first.terminated, true);
  assert.equal(second.terminated, false);

  runtime.cancel("session-b");
  assert.equal(JSON.parse(await secondPending).success, false);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(second.terminated, true);
});

class FakeWorker {
  static instances = [];
  static waiters = [];

  static reset() {
    this.instances = [];
    this.waiters = [];
  }

  static next() {
    if (this.instances.length) return Promise.resolve(this.instances.shift());
    return new Promise((resolve) => this.waiters.push(resolve));
  }

  constructor() {
    this.incoming = [];
    this.onmessage = null;
    this.onerror = null;
    this.onmessageerror = null;
    this.terminated = false;
    const waiter = FakeWorker.waiters.shift();
    if (waiter) waiter(this);
    else FakeWorker.instances.push(this);
  }

  postMessage(message) {
    this.incoming.push(structuredClone(message));
  }

  emit(data) {
    this.onmessage?.({ data: structuredClone(data) });
  }

  terminate() {
    if (this.terminated) return;
    this.terminated = true;
    this.onTerminate?.();
  }
}

function deferred() {
  let resolve;
  const promise = new Promise((accept) => { resolve = accept; });
  return { promise, resolve };
}
