import assert from "node:assert/strict";
import { test } from "node:test";

import { createCodeRuntime } from "../runtime/code-runtime.mjs";

test("nested Code Mode dispatches all calls beyond the former concurrency cap", async () => {
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
        if (active === 129) saturated.resolve();
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
  assert.equal(active, 129);
  assert.equal(maximum, 129);

  for (const release of releases) release();
  const completed = JSON.parse(await execution);
  assert.equal(completed.success, true);
  assert.equal(completed.nested_calls.length, 129);
  assert.equal(maximum, 129);
});

test("parallel-safety metadata does not prevent nested sibling dispatch", async () => {
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

  await Promise.all([firstStarted.promise, unsafeStarted.promise, secondStarted.promise]);
  assert.equal(order.includes("end:first"), false);
  assert.equal(order.includes("end:unsafe"), false);
  releaseFirst.resolve();
  releaseUnsafe.resolve();

  const completed = JSON.parse(await execution);
  assert.equal(completed.success, true);
  assert.deepEqual(order, [
    "start:first",
    "start:unsafe",
    "start:second",
    "end:second",
    "end:first",
    "end:unsafe",
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
]) test(`root ${label} cancels discarded nested tool promises`, async () => {
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
  assert.equal(beforeCancel, "settled");

  const completed = JSON.parse(await withDeadline(
    execution,
    1_000,
    "discarded nested tool did not cancel",
  ));
  assert.equal(nestedSignal.aborted, true);
  assert.equal(completed.success, true);
  assert.match(completed.output, /Script completed/);
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
  assert.deepEqual(first.notifications, []);
  assert.deepEqual(first.updates.filter(update => update.type === "notification"), [{ type: "notification", call_id: "exec-helpers", text: "progress" }]);
  assert.deepEqual(first.cell, { origin_call_id: "exec-helpers", running: true });
  const cellId = outputText(first.output).match(/cell ID ([^\s]+)/)[1];
  let last;
  for (let index = 0; index < 10; index++) {
    const callId = `wait-helpers-${index}`;
    last = await observed(runtime, runtime.waitCodeObserved(JSON.stringify({ cell_id: cellId, max_tokens: 20 }), "helpers", callId), "helpers", callId);
    if (!outputText(last.output).includes("Script running")) break;
  }
  assert.equal(last.success, true);
  assert.deepEqual(last.notifications, []);
  assert.deepEqual(last.cell, { origin_call_id: "exec-helpers", running: false });
  assert.equal(last.output.find((item) => item.type === "input_image").detail, "original");
  assert.equal(last.output.find((item) => item.type === "input_audio").audio_url, "data:audio/wav;base64,AAAA");
  assert.match(outputText(last.output), /tokens truncated|omitted/);
  assert.doesNotMatch(outputText(last.output), /�/);
  const silent = await observed(runtime, runtime.executeCodeObserved(
    '// @exec: {"max_output_tokens":0}\ntext("must-not-appear");', "helpers", "exec-silent",
  ), "helpers", "exec-silent");
  assert.equal(silent.success, true);
  assert.match(outputText(silent.output), /Warning: truncated output.*\nTotal output lines: 1\n\n…4 tokens truncated…/);
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

for (const value of [
  "data:image/png;base64,undefined",
  { image_url: "data:image/png;base64,[object Object]" },
]) test(`Codex helper defers data URI decoding to history preparation: ${JSON.stringify(value)}`, async () => {
  const runtime = createCodeRuntime({});
  const result = JSON.parse(await runtime.executeCode(
    `image(${JSON.stringify(value)});`, "invalid-image", "exec-invalid-image",
  ));
  assert.equal(result.success, true);
  assert.equal(result.output.some((item) => item.type === "input_image"), true);
  runtime.reset();
});

for (const evaluator of ["quickjs", "worker"]) test(`${evaluator} preserves Codex data URI acceptance at the helper boundary`, async () => {
  let evaluate;
  if (evaluator === "quickjs") {
    const { default: variant } = await import("@jitl/quickjs-wasmfile-release-asyncify");
    const { newQuickJSAsyncWASMModuleFromVariant } = await import("quickjs-emscripten-core");
    const { createQuickJsEvaluator } = await import("../runtime/quickjs-evaluator.mjs");
    evaluate = createQuickJsEvaluator(await newQuickJSAsyncWASMModuleFromVariant(variant));
  } else {
    const { NodeWebWorker } = await import("./support/node-web-worker.mjs");
    const { createWorkerEvaluator } = await import("../runtime/worker-evaluator.mjs");
    evaluate = createWorkerEvaluator({ createWorker: () => new NodeWebWorker(new URL("../runtime/code-evaluator.worker.mjs", import.meta.url)) });
  }
  const runtime = createCodeRuntime({}, { evaluate });
  const result = JSON.parse(await runtime.executeCode(
    'image("data:image/png;base64,not base64");', "bad-image", "exec-bad-image",
  ));
  assert.equal(result.success, true);
  assert.equal(result.output.some((item) => item.type === "input_image"), true);
  runtime.reset();
});

test("direct and yielded nested calls retain the originating turn identity", async () => {
  const release = deferred();
  const contexts = [];
  const runtime = createCodeRuntime({
    probe: { supportsParallelToolCalls: true, async handler(input, context) {
      contexts.push({ callId: context.callId, turnId: context.turnId });
      if (input.block) await release.promise;
      return "ok";
    } },
  });
  await runtime.executeTool("probe", "{}", "owner", "direct", "fixture", "owner:7");
  const initial = await observed(runtime, runtime.executeCodeObserved(
    '// @exec: {"yield_time_ms": 0}\nawait tools.probe({block:true}); await tools.probe({});',
    "owner", "exec-turn", "fixture", "owner:7",
  ), "owner", "exec-turn");
  const cellId = outputText(initial.output).match(/Script running with cell ID ([^\s]+)/)[1];
  await runtime.executeTool("probe", "{}", "owner", "next-turn", "fixture", "owner:8");
  release.resolve();
  const completed = await observed(runtime, runtime.waitCodeObserved(JSON.stringify({ cell_id: cellId }), "owner", "wait-turn"), "owner", "wait-turn");
  assert.equal(completed.success, true);
  assert.deepEqual(contexts.map(({turnId}) => turnId), ["owner:7", "owner:7", "owner:8", "owner:7"]);
  assert.equal(new Set(contexts.map(({callId}) => callId)).size, 4);
  runtime.reset();
});


test("global host bridge forwards turn identity to direct and Code Mode hosts", async () => {
  const { installHostBridge, bindHostSession, releaseHostSession } = await import("../internal.mjs");
  const contexts = [];
  const runtime = createCodeRuntime({ probe: { handler(_input, context) { contexts.push(context.turnId); return "ok"; } } });
  const host = { executeTool: runtime.executeTool, executeCode: runtime.executeCodeObserved };
  installHostBridge();
  bindHostSession(host, "bridge-turn");
  try {
    const direct = JSON.parse(await globalThis.nanocodexHost.executeTool("probe", "{}", "bridge-turn", "direct", "fixture", "bridge-turn:7"));
    assert.equal(direct.success, true);
    const nested = await observed(runtime, globalThis.nanocodexHost.executeCode("await tools.probe({});", "bridge-turn", "exec", "fixture", "bridge-turn:7"), "bridge-turn", "exec");
    assert.equal(nested.success, true);
    assert.deepEqual(contexts, ["bridge-turn:7", "bridge-turn:7"]);
  } finally {
    releaseHostSession(host, "bridge-turn");
    runtime.reset();
  }
});

// A wrong/ambiguous host ABI turn cannot be emitted by a conforming Rust owner.
// Cover that fail-closed boundary narrowly through the actual SDK host and tool
// router, not private identity helper calls. Events match the real owned trace.
test("owned host ABI keeps overlapping projected turns separate and rejects wrong or ambiguous turn lookup", async () => {
  const { createNodeHost } = await import("../node/host.mjs");
  const contexts = [];
  let effects = 0;
  const host = createNodeHost({ toolMode: "direct", tools: { probe: {
    async handler() { effects += 1; return { effect: effects }; },
  } }, codeEffectJournal: {
    async begin(context) { contexts.push(context); return { status: "execute" }; },
    async complete() {},
  } });
  const sessionId = "synthetic-overlap";
  function event(type, payload) { host.emitEvent(JSON.stringify({ type, request_id: sessionId, payload })); }
  function accepted(turn, operation) { event("input.accepted", { session_id: sessionId, turn_id: turn, kind: "prompt", request_id: operation }); }
  function call(turn, callId) { event("tool.call", { turn_id: turn, call_id: callId, model_call_index: 1 }); }
  try {
    accepted("projected-original", "original"); call("projected-original", "call_0");
    accepted("projected-queued", "queued"); call("projected-queued", "call_0");
    assert.equal(JSON.parse(await host.executeTool("probe", "{}", sessionId, "call_0", "fixture", "projected-original")).success, true);
    assert.equal(JSON.parse(await host.executeTool("probe", "{}", sessionId, "call_0", "fixture", "projected-queued")).success, true);
    assert.deepEqual(contexts.map(context => context.operationId), ["original", "queued"]);
    await assert.rejects(host.executeTool("probe", "{}", sessionId, "call_0", "fixture", "wrong-turn"), { code: "host_interrupted" });
    accepted("projected-a", "a"); call("projected-a", "ambiguous");
    accepted("projected-b", "b"); call("projected-b", "ambiguous");
    await assert.rejects(host.executeTool("probe", "{}", sessionId, "ambiguous", "fixture", sessionId + ":9"), { code: "host_interrupted" });
    assert.equal(contexts.length, 2, "wrong or ambiguous lookup never reaches journal admission");
    assert.equal(effects, 2, "wrong or ambiguous lookup never dispatches another effect");
  } finally { await host.dispose(); }
});

test("owned host routes multiplexed child calls by trusted accepted turn, not parent event correlation ID", async () => {
  const { createNodeHost } = await import("../node/host.mjs");
  const contexts = [];
  let effects = 0;
  const host = createNodeHost({ toolMode: "direct", tools: { probe: {
    async handler() { effects += 1; return "JOURNAL_CHILD_OK"; },
  } }, codeEffectJournal: {
    async begin(context) { contexts.push(context); return { status: "execute" }; },
    async complete() {},
  } });
  const parent = "synthetic-parent", child = "synthetic-child", turn = "child-projected";
  const emit = (type, payload) => host.emitEvent(JSON.stringify({type,request_id:parent,payload}));
  try {
    emit("input.accepted", {session_id:child,turn_id:turn,item_id:turn+":prompt",kind:"prompt",request_id:null});
    emit("tool.call", {turn_id:turn,call_id:"call_child",model_call_index:1});
    const result = JSON.parse(await host.executeTool("probe", "{}", child, "call_child", "fixture", child+":1"));
    assert.equal(result.success,true);
    assert.equal(effects,1);
    assert.equal(contexts[0].sessionId,child);
    assert.equal(contexts[0].operationId,"non-durable:"+turn+":prompt");
    assert.equal(contexts[0].modelCallIndex,1);
  } finally { await host.dispose(); }
});

test("owned host rejects conflicting accepted owners and invalid metadata without stale call fallback", async () => {
  const { createNodeHost } = await import("../node/host.mjs");
  let admissions = 0, effects = 0;
  const host = createNodeHost({ toolMode: "direct", tools: { probe: {
    async handler() { effects += 1; return effects; },
  } }, codeEffectJournal: {
    async begin() { admissions += 1; return { status: "execute" }; },
    async complete() {},
  } });
  const emit = (type, payload) => host.emitEvent(JSON.stringify({ type, request_id: "parent-correlation", payload }));
  const accepted = (session, turn) => emit("input.accepted", { session_id: session, turn_id: turn, item_id: turn + ":prompt", kind: "prompt", request_id: null });
  const call = (turn, fields = {}) => emit("tool.call", { turn_id: turn, call_id: "reused-call", model_call_index: 1, ...fields });
  try {
    accepted("child-a", "invalid-index"); call("invalid-index");
    call("invalid-index", { model_call_index: 0 });
    await assert.rejects(host.executeTool("probe", "{}", "child-a", "reused-call", "fixture", "invalid-index"), { code: "host_interrupted" });
    accepted("child-a", "wrong-session"); call("wrong-session");
    call("wrong-session", { session_id: "child-b" });
    await assert.rejects(host.executeTool("probe", "{}", "child-a", "reused-call", "fixture", "wrong-session"), { code: "host_interrupted" });
    accepted("child-a", "colliding-turn"); call("colliding-turn");
    accepted("child-b", "colliding-turn"); call("colliding-turn");
    for (const session of ["child-a", "child-b"])
      await assert.rejects(host.executeTool("probe", "{}", session, "reused-call", "fixture", "colliding-turn"), { code: "host_interrupted" });
    assert.equal(admissions, 0); assert.equal(effects, 0);
  } finally { await host.dispose(); }
});

test("owned host waits for exact delayed call metadata and never reuses consumed metadata", async () => {
  const { createNodeHost } = await import("../node/host.mjs");
  const contexts = []; let effects = 0;
  const host = createNodeHost({ toolMode: "direct", tools: { probe: {
    async handler() { effects += 1; return "OK"; },
  } }, codeEffectJournal: {
    async begin(context) { contexts.push(context); return { status: "execute" }; },
    async complete() {},
  } });
  const session = "delayed-child", turn = "delayed-turn";
  const emit = (type, payload) => host.emitEvent(JSON.stringify({ type, request_id: "correlation-parent", payload }));
  const invoke = () => host.executeTool("probe", "{}", session, "reused-call", "fixture", session + ":1");
  try {
    emit("input.accepted", { session_id: session, turn_id: turn, item_id: turn + ":prompt", kind: "prompt", request_id: null });
    for (const index of [1, 2]) {
      const pending = invoke();
      await new Promise(resolve => setTimeout(resolve, 10));
      assert.equal(effects, index - 1); assert.equal(contexts.length, index - 1);
      emit("tool.call", { turn_id: turn, call_id: "reused-call", model_call_index: index });
      assert.equal(JSON.parse(await pending).success, true);
    }
    assert.deepEqual(contexts.map(context => context.modelCallIndex), [1, 2]);
    const pending = invoke();
    const rejected = assert.rejects(pending, { code: "host_interrupted" });
    host.releaseSession(session);
    await rejected;
    assert.equal(effects, 2);
  } finally { await host.dispose(); }
});

test("owned host missing metadata has a bounded deadline and no effect admission", async () => {
  const { createNodeHost } = await import("../node/host.mjs");
  let admissions = 0;
  const host = createNodeHost({ toolMode: "direct", tools: { probe: { async handler() { throw Error("must not dispatch"); } } },
    codeEffectJournal: { async begin() { admissions += 1; return { status: "execute" }; }, async complete() {} } });
  try {
    host.emitEvent(JSON.stringify({ type: "input.accepted", request_id: "deadline", payload: { session_id: "deadline", turn_id: "projected-deadline", kind: "prompt", request_id: "op" } }));
    await withDeadline(assert.rejects(host.executeTool("probe", "{}", "deadline", "missing-call", "fixture", "deadline:1"), { code: "host_interrupted" }), 2000, "identity wait must not hang");
    assert.equal(admissions, 0);
  } finally { await host.dispose(); }
});

test("parallel nested calls coalesce asynchronous identity admission before dispatch", async () => {
  const ready = deferred(), release = deferred();
  const contexts = []; let identities = 0, effects = 0;
  const runtime = createCodeRuntime({ probe: { supportsParallelToolCalls: true,
    async handler() { effects += 1; return effects; },
  } }, { effectIdentity: async () => { identities += 1; ready.resolve(); await release.promise; return { operationId: "parallel-op", modelCallIndex: 7 }; },
    effectJournal: { async begin(context) { contexts.push(context); return { status: "execute" }; }, async complete() {} } });
  try {
    const pending = runtime.executeCode("await Promise.all([tools.probe({}), tools.probe({})]);", "parallel-identity", "outer");
    await withDeadline(ready.promise, 1000, "identity admission not reached");
    await new Promise(resolve => setTimeout(resolve, 10));
    assert.equal(identities, 1); assert.equal(effects, 0); assert.equal(contexts.length, 0);
    release.resolve(); assert.equal(JSON.parse(await pending).success, true);
    assert.equal(effects, 2); assert.equal(identities, 1);
    assert.deepEqual(contexts.map(context => [context.operationId, context.modelCallIndex, context.callId]), [["parallel-op", 7, "outer/code-1"], ["parallel-op", 7, "outer/code-2"]]);
  } finally { runtime.reset(); }
});

test("cell completion stops guest timers before a delayed durable store acknowledgement", async () => {
  const committing = deferred();
  const release = deferred();
  const committed = [];
  const notifications = [];
  const runtime = createCodeRuntime({}, { effectJournal: {
    async begin() { throw new Error("no effects expected"); },
    async complete() { throw new Error("no effects expected"); },
    async beginCell() { return { status: "execute", entries: committed }; },
    async completeCell(_context, writes, receipt) {
      assert.equal(receipt.success, true);
      committing.resolve();
      await release.promise;
      committed.splice(0, committed.length, ...writes);
    },
  } });
  const execution = runtime.executeCode('store("value", 1); setTimeout(() => { store("value", 2); notify("late guest callback"); }, 0);',
    "delayed-store", "first", update => notifications.push(update));
  await withDeadline(committing.promise, 1000, "store commit not reached");
  await new Promise(resolve => setTimeout(resolve, 20));
  release.resolve();
  assert.equal(JSON.parse(await execution).success, true);
  assert.deepEqual(committed, [["value", 1]]);
  assert.deepEqual(notifications, []);
  const read = JSON.parse(await runtime.executeCode('text(load("value"));', "delayed-store", "read"));
  assert.ok(JSON.stringify(read.output).includes('"text":"1"'));
});
