import assert from "node:assert/strict";
import { test } from "node:test";
import { createBeforeCompaction } from "../runtime/before-compaction.mjs";
import { createNodeHost } from "../node/host.mjs";
import { createBrowserHost } from "../browser/host.mjs";

const request = (boundaryId = "boundary-fixture") => ({
  boundaryId,
  sessionId: "session-fixture",
  rootSessionId: "root-fixture",
  messages: [{ role: "user", text: "Remember the synthetic project decision." },
    { role: "assistant", text: "The decision is recorded." }],
  truncated: false,
});
const deferred = () => Promise.withResolvers();

test("beforeCompaction validates receipt shape and its UTF-8 byte limit", async t => {
  const invalid = [undefined, null, true, "receipt", {}, { receiptId: 1 },
    { receiptId: "" }, { receiptId: " \n\t" }, { receiptId: "x".repeat(257) },
    { receiptId: "é".repeat(129) }, { receiptId: "🦊".repeat(65) }];
  for (const [index, receipt] of invalid.entries()) {
    await t.test(`invalid receipt ${index}`, async () => {
      const hook = createBeforeCompaction(async () => receipt);
      await assert.rejects(hook.preserve(request()), {
        name: "TypeError", message: /durable receiptId.*UTF-8 bytes/,
      });
      hook.dispose();
    });
  }
  for (const receiptId of ["x", "x".repeat(256), "é".repeat(128), "🦊".repeat(64)]) {
    const hook = createBeforeCompaction(async () => ({ receiptId }));
    assert.deepEqual(await hook.preserve(request()), { receiptId });
    hook.dispose();
  }
});

for (const synchronous of [false, true]) {
  test(`beforeCompaction propagates ${synchronous ? "synchronous throws" : "rejected promises"} and releases the boundary`, async () => {
    const failure = new Error("synthetic durable write failed");
    let attempts = 0;
    const hook = createBeforeCompaction(() => {
      if (++attempts > 1) return Promise.resolve({ receiptId: "retry-succeeded" });
      if (synchronous) throw failure;
      return Promise.reject(failure);
    });
    await assert.rejects(hook.preserve(request()), error => error === failure);
    assert.deepEqual(await hook.preserve(request()), { receiptId: "retry-succeeded" });
    hook.dispose();
  });
}

test("beforeCompaction cancellation aborts only its boundary and ignores a late receipt", async () => {
  const started = [deferred(), deferred()];
  const commits = [deferred(), deferred()];
  let calls = 0;
  const hook = createBeforeCompaction(input => {
    const index = calls++;
    started[index].resolve(input);
    return commits[index].promise;
  });
  const first = hook.preserve(request("first"));
  const cancelled = assert.rejects(first, /preservation cancelled/);
  const second = hook.preserve(request("second"));
  const [one, two] = await Promise.all(started.map(value => value.promise));
  await assert.rejects(hook.preserve(request("first")), /already active/);
  hook.cancel("unknown");
  hook.cancel("first");
  await cancelled;
  assert.equal(one.signal.aborted, true);
  assert.equal(two.signal.aborted, false);
  commits[0].resolve({ receiptId: "too-late" });
  commits[1].resolve({ receiptId: "second-committed" });
  assert.deepEqual(await second, { receiptId: "second-committed" });
  hook.dispose();
});

test("beforeCompaction enforces the default 30-second deadline even if the callback ignores abort", async t => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const entered = deferred();
  const hook = createBeforeCompaction(input => { entered.resolve(input); return new Promise(() => {}); });
  const pending = hook.preserve(request());
  const rejected = assert.rejects(pending, /preservation timed out/);
  const input = await entered.promise;
  t.mock.timers.tick(29_999);
  assert.equal(input.signal.aborted, false);
  t.mock.timers.tick(1);
  await rejected;
  assert.equal(input.signal.aborted, true);
  assert.match(input.signal.reason.message, /timed out/);
  hook.dispose();
});

test("beforeCompaction clears completed deadlines and disposal aborts all pending requests", async t => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const inputs = [];
  const hook = createBeforeCompaction(input => {
    inputs.push(input);
    return input.boundaryId === "complete" ? Promise.resolve({ receiptId: "committed" }) : new Promise(() => {});
  });
  await hook.preserve(request("complete"));
  t.mock.timers.tick(30_000);
  assert.equal(inputs[0].signal.aborted, false, "a completed receipt has no remaining deadline");
  const first = assert.rejects(hook.preserve(request("one")), /cancelled/);
  const second = assert.rejects(hook.preserve(request("two")), /cancelled/);
  await Promise.resolve();
  hook.dispose();
  hook.dispose();
  await Promise.all([first, second]);
  assert.ok(inputs.slice(1).every(input => input.signal.aborted));
  await assert.rejects(hook.preserve(request("later")), /disposed/);
  assert.equal(inputs.length, 3);
});

for (const [name, createHost] of [["Node", createNodeHost], ["browser/Cloudflare", createBrowserHost]]) {
  test(`${name} host connects the beforeCompaction callback, cancellation, and disposal`, async t => {
    const inputs = [];
    const entered = [deferred(), deferred()];
    const host = createHost({ beforeCompaction(input) {
      inputs.push(input);
      entered[inputs.length - 1].resolve();
      return new Promise(() => {});
    } });
    t.after(() => host.dispose());
    const cancelled = assert.rejects(host.beforeCompaction(request("cancel")), /cancelled/);
    await entered[0].promise;
    host.cancelBeforeCompaction("cancel");
    await cancelled;
    assert.equal(inputs[0].signal.aborted, true);
    const disposed = assert.rejects(host.beforeCompaction(request("dispose")), /cancelled/);
    await entered[1].promise;
    await host.dispose();
    await disposed;
    assert.equal(inputs[1].signal.aborted, true);
    await assert.rejects(host.beforeCompaction(request()), /disposed/);
  });
}

for (const interrupt of ["cancel", "dispose"]) {
  test(`beforeCompaction ${interrupt} before callback dispatch prevents host effects`, async () => {
    let calls = 0;
    const hook = createBeforeCompaction(async () => {
      calls++;
      return { receiptId: "must-not-commit" };
    });
    const pending = hook.preserve(request());
    const rejected = assert.rejects(pending, /preservation cancelled/);
    hook[interrupt]("boundary-fixture");
    await rejected;
    assert.equal(calls, 0, "an already interrupted boundary cannot invoke the host callback");
    hook.dispose();
  });
}

test("beforeCompaction cancellation after fulfillment still prevents receipt delivery", async () => {
  let input;
  const hook = createBeforeCompaction(value => {
    input = value;
    // The first microtask runs before Promise.race settles. The nested one
    // interrupts after settlement but before preserve's await continuation.
    queueMicrotask(() => queueMicrotask(() => hook.cancel(value.boundaryId)));
    return { receiptId: "committed-during-cancellation" };
  });
  await assert.rejects(hook.preserve(request()), /preservation cancelled/);
  assert.equal(input.signal.aborted, true);
  hook.dispose();
});
