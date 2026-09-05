import assert from "node:assert/strict";
import { test } from "node:test";

import {
  createWorkerAgent,
  installWorkerAgentRuntime,
  prepareWorkerAgent,
  prewarmWorkerRuntime,
  WORKER_EVENT_BATCH_MAX_BYTES,
  WORKER_EVENT_BATCH_MAX_EVENTS,
  WORKER_HEARTBEAT_INTERVAL_MS,
  WORKER_HEARTBEAT_TIMEOUT_MS,
} from "../browser/WorkerAgent.mjs";
import { createAgentConfig } from "../browser/config.mjs";
import { agentActions } from "../actions/index.mjs";
import { createAgentClient, createBrowserVoice, defineRuntime } from "../internal.mjs";
import * as Transport from "../browser/Transport.mjs";

test("Worker Agent preserves synchronous prompt handles, independent results, and ordered events", async () => {
  const fixture = createFixture();
  const worker = new LoopbackWorker(fixture.createAgent);
  const agent = await createWorkerAgent({
    sessionId: "root",
    harness: false,
    transport: Transport.openAi({ apiKey: "test-key" }),
  }, { worker });
  assert.equal(agent.agentId, "root");
  assert.equal(agent.sessionId, "root");
  const events = [];
  const watch = agent.events.watch();
  watch.onEvent((event) => events.push(event.seq));
  const iterator = watch[Symbol.asyncIterator]();

  const turn = agent.turn.prompt({ input: "ship", id: "operation-1" });
  assert.equal(typeof turn.result, "function");
  const pending = turn.result();
  await turn.steer({ input: "carefully" });
  fixture.emit("root", 1);
  fixture.emit("root", 2);
  await tick();
  assert.deepEqual(events, [1, 2]);
  assert.equal((await iterator.next()).value.seq, 1);
  assert.equal((await iterator.next()).value.seq, 2);
  assert.deepEqual(fixture.log.slice(0, 2), [
    ["prompt", "root", "ship", "operation-1"],
    ["steer", "root", "carefully"],
  ]);

  fixture.complete("root", "done");
  const result = await pending;
  assert.equal(result.finalMessage, "done");
  assert.deepEqual(fixture.resultStats, { snapshots: 0, usages: 0, released: 0 });
  const completion = worker.outgoing.find((message) => message.value?.resultId);
  assert.deepEqual(Object.keys(completion.value).sort(), ["finalMessage", "resultId"]);
  assert.equal(JSON.stringify(completion.value).length < 128, true);
  assert.throws(() => structuredClone(result), /could not be cloned/i);

  const [snapshot, sameSnapshot] = await Promise.all([result.snapshot(), result.snapshot()]);
  const [usage, sameUsage] = await Promise.all([result.usage(), result.usage()]);
  assert.equal(snapshot.workspace, "/workspace/root");
  assert.equal(usage.total_tokens, 3);
  assert.strictEqual(sameSnapshot, snapshot);
  assert.strictEqual(sameUsage, usage);
  assert.equal(Object.isFrozen(snapshot), true);
  assert.equal(Object.isFrozen(usage), true);
  assert.deepEqual(fixture.resultStats, { snapshots: 1, usages: 1, released: 0 });
  assert.equal(worker.incoming.filter((message) => message.method === "result.snapshot").length, 1);
  assert.equal(worker.incoming.filter((message) => message.method === "result.usage").length, 1);
  result.dispose();
  await assert.rejects(result.snapshot(), /disposed/);
  await assert.rejects(result.usage(), /disposed/);
  turn.dispose();
  watch.off();
  agent.dispose();
  assert.equal(worker.terminated, 1);
});

test("Worker prompt acknowledgement waits for durable turn admission", async () => {
  const fixture = createFixture({ holdAcceptance: true });
  const worker = new LoopbackWorker(fixture.createAgent);
  const agent = await createWorkerAgent({ sessionId: "root", harness: false }, { worker });
  const turn = agent.turn.prompt({ input: "admit durably", id: "operation-7" });
  let settled = false;
  const accepted = turn.accepted().then((requestId) => {
    settled = true;
    return requestId;
  });
  const pendingResult = turn.result();

  await tick();
  assert.equal(settled, false);
  assert.equal(worker.incoming.some((message) => message.method === "turn.result"), false);

  fixture.accept("root", "operation-7");
  assert.equal(await accepted, "operation-7");
  await tick();
  assert.equal(worker.incoming.some((message) => message.method === "turn.result"), true);

  fixture.complete("root", "admitted");
  const result = await pendingResult;
  assert.equal(result.finalMessage, "admitted");
  result.dispose();
  turn.dispose();
  agent.dispose();
  assert.equal(worker.terminated, 1);
});

test("Worker Agent retains and proxies the Rust browser voice handle", async () => {
  const fixture = createFixture();
  const worker = new LoopbackWorker(fixture.createAgent);
  const agent = await createWorkerAgent({ sessionId: "root", harness: false }, { worker });
  const voice = await createBrowserVoice(agent, "cove");

  await voice.start();
  const call = JSON.parse(await voice.callBody("v=offer"));
  assert.equal(JSON.parse(call.call_body).session.audio.output.voice, "cove");
  assert.deepEqual(
    JSON.parse(await voice.completeCall("v=answer", "/v1/live/rtc_test")),
    { call_id: "rtc_test", sdp: "v=answer" },
  );
  assert.equal(await voice.sidebandUrl("rtc_test"), "/sideband/rtc_test");
  assert.equal(JSON.parse(await voice.sidebandOpened()).frames.length, 0);
  assert.equal(JSON.parse(await voice.sidebandClosed(1_000)).reconnect_after_ms, 200);
  await voice.framesSent(1);
  assert.equal(await voice.requiresAgentAdmission('{"type":"delegation.created"}'), true);
  assert.equal(JSON.parse(await voice.realtimeMessage('{"type":"turn.done"}')).frames.length, 0);
  assert.equal(await voice.cancel(), true);
  assert.equal(await voice.preferredPhysicalInput("BlackHole", '["Built-in Microphone"]'), 0);
  assert.equal(JSON.parse(await voice.stop()).frames[0], '{"type":"session.close"}');
  agent.dispose();
  assert.equal(worker.terminated, 0);
  voice.free();
  await tick();

  assert.deepEqual(fixture.log.filter(([kind]) => kind.startsWith("voice-")), [
    ["voice-create", "root", "cove"],
    ["voice-start", "root"],
    ["voice-call", "root", "v=offer"],
    ["voice-complete", "root", "v=answer", "/v1/live/rtc_test"],
    ["voice-sideband", "root", "rtc_test"],
    ["voice-sideband-opened", "root"],
    ["voice-sideband-closed", "root", 1000],
    ["voice-frames-sent", "root", 1],
    ["voice-admission", "root", '{"type":"delegation.created"}'],
    ["voice-message", "root", '{"type":"turn.done"}'],
    ["voice-cancel", "root"],
    ["voice-input", "root", "BlackHole", '["Built-in Microphone"]'],
    ["voice-stop", "root"],
    ["voice-free", "root"],
  ]);
  assert.equal(worker.terminated, 1);
});

test("Worker turn admission preserves stable error codes and recovery identity", async () => {
  const fixture = createFixture({
    acceptanceError: Object.assign(new Error("durable operation blocked"), {
      code: "retryable", blockedBy: "older-operation",
    }),
  });
  const worker = new LoopbackWorker(fixture.createAgent);
  const agent = await createWorkerAgent({ sessionId: "root", harness: false }, { worker });
  const turn = agent.turn.prompt({ input: "conflicting input", id: "operation-7" });

  await assert.rejects(
    turn.accepted(),
    (error) => error instanceof Error
      && error.message === "durable operation blocked"
      && error.code === "retryable"
      && error.blockedBy === "older-operation",
  );
  await assert.rejects(turn.result(), (error) => error?.code === "retryable"
    && error.blockedBy === "older-operation");
  assert.equal(fixture.log.filter(([kind]) => kind === "turn-dispose").length, 1);

  turn.dispose();
  agent.dispose();
  assert.equal(worker.terminated, 1);
});

test("a prompt-created Turn keeps its Worker alive after Agent disposal", async () => {
  const fixture = createFixture();
  const worker = new LoopbackWorker(fixture.createAgent);
  const agent = await createWorkerAgent({ sessionId: "root", harness: false }, { worker });
  const turn = agent.turn.prompt({ input: "finish independently" });

  agent.dispose();
  assert.equal(worker.terminated, 0);
  const pendingResult = turn.result();
  await tick();
  assert.equal(fixture.disposedAgents.has("root"), true);

  fixture.complete("root", "still completed");
  const result = await pendingResult;
  assert.equal(result.finalMessage, "still completed");
  assert.equal(worker.terminated, 0);
  assert.equal(fixture.log.filter(([kind]) => kind === "turn-dispose").length, 1);
  assert.equal(worker.incoming.filter((message) => message.method === "turn.dispose").length, 0);

  turn.dispose();
  turn.dispose();
  result.dispose();
  result.dispose();
  await tick();
  assert.equal(worker.terminated, 1);
  assert.equal(fixture.resultStats.released, 1);
  assert.equal(worker.incoming.filter((message) => message.method === "result.dispose").length, 1);
});

test("disposing a result-started Turn before prompt acceptance preserves its result", async () => {
  const fixture = createFixture();
  const worker = new LoopbackWorker(fixture.createAgent);
  const agent = await createWorkerAgent({ sessionId: "root", harness: false }, { worker });
  const turn = agent.turn.prompt({ input: "accept before disposal" });
  const pendingResult = turn.result();

  turn.dispose();
  turn.dispose();
  agent.dispose();
  assert.equal(worker.terminated, 0);
  assert.equal(worker.incoming.filter((message) => message.method === "turn.dispose").length, 0);

  await tick();
  fixture.complete("root", "accepted result");
  const result = await pendingResult;
  assert.equal(result.finalMessage, "accepted result");
  assert.equal(fixture.log.filter(([kind]) => kind === "turn-dispose").length, 1);
  assert.equal(worker.incoming.filter((message) => message.method === "turn.dispose").length, 0);
  assert.equal(worker.terminated, 0);

  result.dispose();
  result.dispose();
  await tick();
  assert.equal(worker.terminated, 1);
  assert.equal(fixture.resultStats.released, 1);
  assert.equal(worker.incoming.filter((message) => message.method === "result.dispose").length, 1);
});

test("disposing an unawaited Turn releases its Worker lease exactly once", async () => {
  const fixture = createFixture();
  const worker = new LoopbackWorker(fixture.createAgent);
  const agent = await createWorkerAgent({ sessionId: "root", harness: false }, { worker });
  const turn = agent.turn.prompt({ input: "release without a result" });
  await tick();

  agent.dispose();
  assert.equal(worker.terminated, 0);
  turn.dispose();
  turn.dispose();
  agent.dispose();
  await tick();

  assert.equal(worker.terminated, 1);
  assert.equal(worker.incoming.filter((message) => message.method === "turn.dispose").length, 1);
  assert.equal(fixture.log.filter(([kind]) => kind === "turn-dispose").length, 1);
  assert.equal(fixture.resultStats.released, 0);
});

test("completed results survive Turn and Agent disposal until their own async work settles", async () => {
  const fixture = createFixture({ holdSnapshot: true });
  const worker = new LoopbackWorker(fixture.createAgent);
  const agent = await createWorkerAgent({ sessionId: "root", harness: false }, { worker });
  const turn = agent.turn.prompt({ input: "retain the checkpoint" });
  const pendingResult = turn.result();
  await tick();
  fixture.complete("root", "retained");
  const result = await pendingResult;
  turn.dispose();
  await agent.session.shutdown();
  assert.equal(worker.terminated, 0);

  const pendingSnapshot = result.snapshot();
  await tick();
  result.dispose();
  assert.equal(worker.terminated, 0);
  fixture.releaseSnapshot();
  assert.equal((await pendingSnapshot).workspace, "/workspace/root");
  await tick();
  assert.equal(worker.terminated, 1);
  assert.equal(fixture.resultStats.released, 1);
});

test("historical result identity rejects clones, disposed handles, and another Worker", async () => {
  const leftFixture = createFixture();
  const rightFixture = createFixture();
  const leftWorker = new LoopbackWorker(leftFixture.createAgent);
  const rightWorker = new LoopbackWorker(rightFixture.createAgent);
  const left = await createWorkerAgent({ sessionId: "left", harness: false }, { worker: leftWorker });
  const right = await createWorkerAgent({ sessionId: "right", harness: false }, { worker: rightWorker });
  const turn = left.turn.prompt({ input: "checkpoint" });
  const pending = turn.result();
  await tick();
  leftFixture.complete("left", "done");
  const result = await pending;
  turn.dispose();

  await assert.rejects(right.session.fork({ at: result }), /same Worker Agent/);
  await assert.rejects(
    right.session.fork({ at: Object.freeze({ finalMessage: "forged" }) }),
    /completed Nanocodex turn result/,
  );
  result.dispose();
  await assert.rejects(left.session.fork({ at: result }), /disposed/);
  left.dispose();
  right.dispose();
  assert.equal(leftWorker.terminated, 1);
  assert.equal(rightWorker.terminated, 1);
});

test("malformed on-demand result JSON rejects once without poisoning Worker cleanup", async () => {
  const fixture = createFixture({ invalidSnapshot: true });
  const worker = new LoopbackWorker(fixture.createAgent);
  const agent = await createWorkerAgent({ sessionId: "root", harness: false }, { worker });
  const turn = agent.turn.prompt({ input: "invalid snapshot" });
  const pending = turn.result();
  await tick();
  fixture.complete("root", "done");
  const result = await pending;

  await assert.rejects(result.snapshot(), SyntaxError);
  await assert.rejects(result.snapshot(), SyntaxError);
  assert.equal(fixture.resultStats.snapshots, 1);
  assert.equal((await result.usage()).total_tokens, 3);
  result.dispose();
  turn.dispose();
  agent.dispose();
  assert.equal(worker.terminated, 1);
  assert.equal(fixture.resultStats.released, 1);
});

test("Worker event forwarding follows first/last demand with filtering, order, and immutable fan-out", async () => {
  const fixture = createFixture();
  const worker = new LoopbackWorker(fixture.createAgent);
  const root = await createWorkerAgent({ sessionId: "root", harness: false }, { worker });
  const child = await root.session.spawn();

  fixture.emit("root", 0);
  await tick();
  assert.equal(fixture.watcherStats.created, 0);
  assert.equal(worker.outgoing.some((message) => message.type === "event.batch"), false);

  const rootEvents = [];
  const allEvents = [];
  const childEvents = [];
  const rootWatch = root.events.watch();
  rootWatch.onEvent((event) => {
    assert.equal(Object.isFrozen(event), true);
    assert.equal(Object.isFrozen(event.payload), true);
    assert.equal(Object.isFrozen(event.payload.nested), true);
    assert.throws(() => { event.payload.nested.value = "changed"; }, TypeError);
  });
  rootWatch.onEvent((event) => rootEvents.push([event.seq, event.payload.nested.value]));
  const allWatch = root.events.watch({ includeAllSessions: true });
  allWatch.onEvent((event) => allEvents.push([event.request_id, event.seq]));
  const childWatch = child.events.watch();
  childWatch.onEvent((event) => childEvents.push(event.seq));
  await tick();

  assert.equal(fixture.watcherStats.created, 1);
  assert.equal(fixture.watcherStats.active, 1);
  assert.deepEqual(fixture.watcherStats.options, [{ includeAllSessions: true }]);
  fixture.emit("root", 1, { nested: { value: "root" } });
  fixture.emit("root-spawn", 2, { nested: { value: "child" } });
  fixture.emit("root", 3, { nested: { value: "root-again" } });
  await tick();

  assert.deepEqual(rootEvents, [[1, "root"], [3, "root-again"]]);
  assert.deepEqual(allEvents, [["root", 1], ["root-spawn", 2], ["root", 3]]);
  assert.deepEqual(childEvents, [2]);

  rootWatch.off();
  allWatch.off();
  await tick();
  assert.equal(fixture.watcherStats.active, 1);
  childWatch.off();
  await tick();
  assert.equal(fixture.watcherStats.active, 0);
  assert.equal(fixture.watcherStats.released, 1);
  const forwardedMessages = worker.outgoing.filter((message) => message.type.startsWith("event.")).length;
  fixture.emit("root", 4, { nested: { value: "unsubscribed" } });
  await tick();
  assert.equal(worker.outgoing.filter((message) => message.type.startsWith("event.")).length, forwardedMessages);

  const resumed = root.events.watch();
  resumed.onEvent(() => {});
  await tick();
  assert.equal(fixture.watcherStats.created, 2);
  assert.equal(fixture.watcherStats.active, 1);
  resumed.off();
  await tick();
  assert.equal(fixture.watcherStats.active, 0);

  child.dispose();
  root.dispose();
  assert.equal(worker.terminated, 1);
});

test("Worker batches 4,096 ordered events under hard count and encoded-byte message bounds", async () => {
  const fixture = createFixture();
  const worker = new LoopbackWorker(fixture.createAgent);
  const agent = await createWorkerAgent({ sessionId: "root", harness: false }, { worker });
  const received = [];
  const watch = agent.events.watch();
  watch.onEvent((event) => received.push(event));
  await tick();

  const blob = "x".repeat(2_048);
  for (let seq = 0; seq < 4_096; seq += 1) fixture.emit("root", seq, { blob });
  await tick();

  const batches = worker.outgoing.filter((message) => message.type === "event.batch");
  assert.equal(received.length, 4_096);
  assert.deepEqual(received.map((event) => event.seq), Array.from({ length: 4_096 }, (_, index) => index));
  assert.equal(batches.length > 1, true);
  assert.equal(batches.some((message) => message.events.length < WORKER_EVENT_BATCH_MAX_EVENTS), true);
  assert.equal(batches.reduce((count, message) => count + message.events.length, 0), 4_096);
  for (const message of batches) {
    assert.equal(message.events.length <= WORKER_EVENT_BATCH_MAX_EVENTS, true);
    assert.equal(message.encodedBytes <= WORKER_EVENT_BATCH_MAX_BYTES, true);
    assert.equal(
      message.encodedBytes,
      message.events.reduce((bytes, entry) => bytes + entry.encodedBytes, 0),
    );
  }

  const oversized = "y".repeat(WORKER_EVENT_BATCH_MAX_BYTES + 1_024);
  fixture.emit("root", 4_096, { blob: oversized });
  await tick();
  const chunks = worker.outgoing.filter((message) => message.type === "event.chunk");
  assert.equal(chunks.length > 1, true);
  assert.equal(chunks.every((message) => message.chunk.byteLength <= WORKER_EVENT_BATCH_MAX_BYTES), true);
  assert.equal(received.at(-1).seq, 4_096);
  assert.equal(received.at(-1).payload.blob, oversized);
  assert.equal(Object.isFrozen(received.at(-1).payload), true);

  watch.off();
  await tick();
  const eventMessageCount = worker.outgoing.filter((message) => message.type.startsWith("event.")).length;
  fixture.emit("root", 4_097, { blob: "not-forwarded" });
  await tick();
  assert.equal(
    worker.outgoing.filter((message) => message.type.startsWith("event.")).length,
    eventMessageCount,
  );
  agent.dispose();
});

test("turn cancellation followed by graceful shutdown releases Worker event demand", async () => {
  const fixture = createFixture();
  const worker = new LoopbackWorker(fixture.createAgent);
  const agent = await createWorkerAgent({ sessionId: "root", harness: false }, { worker });
  const watch = agent.events.watch();
  watch.onEvent(() => {});
  await tick();
  assert.equal(fixture.watcherStats.active, 1);

  const turn = agent.turn.prompt({ input: "cancel me" });
  await turn.cancel();
  turn.dispose();
  await agent.session.shutdown();
  await tick();

  assert.equal(fixture.log.some(([kind]) => kind === "cancel"), true);
  assert.equal(fixture.log.some(([kind]) => kind === "shutdown"), true);
  assert.equal(fixture.watcherStats.active, 0);
  assert.equal(worker.terminated, 1);
});

test("session, branching, realtime, and graceful lifecycle remain DefaultAgent-shaped", async () => {
  const fixture = createFixture();
  const worker = new LoopbackWorker(fixture.createAgent);
  const root = await createWorkerAgent({ sessionId: "root", harness: false }, { worker });

  await root.session.setModel("gpt-6-astra");
  await root.session.setThinking("high");
  await root.session.setFastMode(true);
  await root.session.compact();
  assert.equal((await root.session.context()).workspace, "/workspace/root");
  assert.equal((await root.session.appendDeveloperMessage("voice started")).workspace, "/workspace/root");
  await root.session.realtime.start();
  await root.session.realtime.end();
  assert.equal(
    await root.session.realtime.delegation("fix <x>", [{ role: "user", text: "yes & now" }]),
    "canonical:fix <x>:user: yes & now",
  );
  assert.equal(await root.session.realtime.tailDelegation([]), undefined);
  assert.equal(
    fixture.log.some(([kind]) => kind === "realtime-delegation"),
    true,
  );
  assert.equal(
    fixture.log.some(([kind, sessionId, model]) => (
      kind === "model" && sessionId === "root" && model === "gpt-6-astra"
    )),
    true,
  );

  const first = root.turn.prompt({ input: "first" });
  const firstResult = first.result();
  await tick();
  fixture.complete("root", "first done");
  const completed = await firstResult;
  first.dispose();
  const fork = await root.session.fork({ at: completed });
  const spawn = await root.session.spawn();
  assert.equal(fork.sessionId, "root-fork");
  assert.equal(spawn.sessionId, "root-spawn");
  assert.equal(fixture.log.some(([kind]) => kind === "fork-at"), true);

  const childEvents = [];
  const childWatch = spawn.events.watch();
  childWatch.onEvent((event) => childEvents.push(event.seq));
  fork.dispose();
  await root.session.shutdown();
  fixture.emit("root-spawn", 7);
  await tick();
  assert.deepEqual(childEvents, [7]);
  await spawn.session.compact();
  childWatch.off();
  spawn.dispose();
  completed.dispose();
  assert.equal(worker.terminated, 1);
  assert.throws(() => root.turn.prompt({ input: "late" }), /disposed/);
});

test("Worker failures reject every pending operation and stale messages stay isolated", async () => {
  const fixture = createFixture();
  const worker = new LoopbackWorker(fixture.createAgent);
  const failures = [];
  const agent = await createWorkerAgent(
    { sessionId: "root", harness: false },
    { worker, onFailure: (error) => failures.push(error) },
  );
  const turn = agent.turn.prompt({ input: "never completes" });
  const pending = turn.result();
  const staleHandler = worker.onmessage;
  await tick();

  worker.crash("worker exploded");
  await assert.rejects(pending, /worker exploded/);
  assert.equal(failures.length, 1);
  assert.match(failures[0].message, /worker exploded/);
  assert.throws(() => agent.session.compact(), /disposed/);
  staleHandler({ data: { protocol: "nanocodex.worker-agent.v1", channel: "stale", type: "resolve", id: "rpc-1" } });
  assert.equal(worker.terminated, 1);
  agent.dispose();
});

test("a duplicate Worker session neither boots nor fences and leaves the first usable", async () => {
  const fixture = createFixture();
  const firstWorker = new LoopbackWorker(fixture.createAgent);
  const first = await createWorkerAgent(
    { sessionId: "reserved-worker-session", harness: false },
    { worker: firstWorker },
  );
  let duplicateWorkers = 0;

  await assert.rejects(
    createWorkerAgent(
      { sessionId: "reserved-worker-session", harness: false },
      { worker: () => { duplicateWorkers += 1; return new SilentWorker(); } },
    ),
    /session ID is already active/,
  );
  assert.equal(duplicateWorkers, 0, "duplicate detection precedes Worker construction and boot");
  assert.equal(await first.session.compact(), undefined);

  first.dispose();
  const replacementFixture = createFixture();
  const replacementWorker = new LoopbackWorker(replacementFixture.createAgent);
  const replacement = await createWorkerAgent(
    { sessionId: "reserved-worker-session", harness: false },
    { worker: replacementWorker },
  );
  replacement.dispose();
});

test("post-boot Worker client construction awaits Agent rollback acknowledgement", async () => {
  const worker = new MismatchedSessionWorker();
  let settled = false;
  const creation = createWorkerAgent(
    { sessionId: "reserved-session", harness: false },
    { worker },
  ).finally(() => { settled = true; });

  await waitFor(() => worker.rollback !== undefined);
  assert.equal(settled, false);
  assert.equal(worker.terminated, 0);
  worker.acknowledgeRollback();
  await assert.rejects(creation, /changed reserved session ID/);
  assert.equal(worker.terminated, 1);

  const fixture = createFixture();
  const replacementWorker = new LoopbackWorker(fixture.createAgent);
  const replacement = await createWorkerAgent(
    { sessionId: "reserved-session", harness: false },
    { worker: replacementWorker },
  );
  replacement.dispose();
});

test("a Worker crash queued after ready fails creation and releases its raw lease", async () => {
  const worker = new ReadyThenCrashWorker();

  await assert.rejects(
    createWorkerAgent(
      { sessionId: "ready-then-crashed", harness: false },
      { worker },
    ),
    /crashed after ready/,
  );
  assert.equal(worker.terminated, 1);
  assert.equal(worker.onerror, null);

  const fixture = createFixture();
  const replacementWorker = new LoopbackWorker(fixture.createAgent);
  const replacement = await createWorkerAgent(
    { sessionId: "ready-then-crashed", harness: false },
    { worker: replacementWorker },
  );
  replacement.dispose();
});

test("Worker connection construction releases its reservation after every cleanup failure", async () => {
  const failures = [
    new Error("onerror installation failed"),
    new Error("onmessage cleanup failed"),
    new Error("onerror cleanup failed"),
    new Error("onmessageerror cleanup failed"),
    new Error("termination failed"),
  ];
  await assert.rejects(
    createWorkerAgent(
      { sessionId: "throwing-worker-construction", harness: false },
      { worker: new ThrowingConstructionWorker(failures) },
    ),
    (error) => {
      assert.equal(error instanceof AggregateError, true);
      assert.deepEqual(error.errors, failures);
      return true;
    },
  );

  const fixture = createFixture();
  const replacement = await createWorkerAgent(
    { sessionId: "throwing-worker-construction", harness: false },
    { worker: new LoopbackWorker(fixture.createAgent) },
  );
  replacement.dispose();
});

test("a throwing prewarmed Worker terminator cannot strand creation or its reservation", { timeout: 2_000 }, async (t) => {
  const reported = [];
  t.mock.method(globalThis.console, "error", (error) => reported.push(error));
  const terminationFailure = new Error("prewarm termination failed");
  const worker = new ThrowingPrewarmTerminatorWorker(terminationFailure);
  const options = { sessionId: "throwing-prewarm-session", harness: false };
  const preparation = prepareWorkerAgent(options, { worker });
  void preparation.catch(() => {});
  const creation = createWorkerAgent(options);

  worker.fail("prewarm startup failed");
  await assert.rejects(creation, /prewarm startup failed/);
  assert.deepEqual(reported, [terminationFailure]);

  const fixture = createFixture();
  const replacement = await createWorkerAgent(options, {
    worker: new LoopbackWorker(fixture.createAgent),
  });
  replacement.dispose();
});

test("Worker failure cleanup survives a throwing terminator", async (t) => {
  const reported = [];
  t.mock.method(globalThis.console, "error", (error) => reported.push(error));
  const fixture = createFixture({ holdCompaction: true });
  const worker = new LoopbackWorker(fixture.createAgent);
  const terminate = worker.terminate.bind(worker);
  const terminationFailure = new Error("terminate failed");
  worker.terminate = () => {
    terminate();
    throw terminationFailure;
  };
  const failures = [];
  const agent = await createWorkerAgent(
    { sessionId: "root", harness: false },
    { worker, onFailure: (error) => failures.push(error) },
  );
  const pending = [agent.session.compact(), agent.session.compact()];
  for (const operation of pending) void operation.catch(() => {});
  await tick();

  worker.crash("worker exploded");
  const settled = await Promise.allSettled(pending);

  assert.equal(failures.length, 1);
  assert.match(failures[0].message, /worker exploded/);
  assert.equal(
    settled.every(({ status, reason }) => status === "rejected" && reason === failures[0]),
    true,
  );
  assert.deepEqual(reported, [terminationFailure]);
  assert.equal(worker.terminated, 1);
  assert.equal(worker.onmessage, null);
  assert.equal(worker.onerror, null);
  assert.equal(worker.onmessageerror, null);
  assert.throws(() => agent.session.compact(), /disposed/);
  agent.dispose();
});

test("normal Worker Agent disposal does not report a runtime failure", async () => {
  const fixture = createFixture();
  const worker = new LoopbackWorker(fixture.createAgent);
  const failures = [];
  const agent = await createWorkerAgent(
    { sessionId: "root", harness: false },
    { worker, onFailure: (error) => failures.push(error) },
  );

  await agent.session.shutdown();

  assert.deepEqual(failures, []);
  assert.equal(worker.terminated, 1);
});

test("a synchronous heartbeat pong leaves no timer behind after disposal", async (t) => {
  let nextTimer = 1;
  const timers = new Map();
  t.mock.method(globalThis, "setTimeout", (callback, delay) => {
    const handle = nextTimer;
    nextTimer += 1;
    timers.set(handle, { callback, delay });
    return handle;
  });
  t.mock.method(globalThis, "clearTimeout", (handle) => timers.delete(handle));
  const worker = new SynchronousHeartbeatWorker();
  const agent = await createWorkerAgent(
    { sessionId: "root", harness: false },
    { worker },
  );
  const heartbeat = [...timers.entries()].find(([, timer]) => (
    timer.delay === WORKER_HEARTBEAT_INTERVAL_MS
  ));
  assert.notEqual(heartbeat, undefined);

  timers.delete(heartbeat[0]);
  heartbeat[1].callback();

  assert.deepEqual(
    [...timers.values()].map(({ delay }) => delay),
    [WORKER_HEARTBEAT_INTERVAL_MS],
  );
  agent.dispose();
  assert.equal(timers.size, 0);
  assert.equal(worker.terminated, 1);
});

test("a silent Worker hang rejects all pending work once and config retry uses a fresh generation", { timeout: 2_000 }, async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const fixtures = [];
  const workers = [];
  const config = createAgentConfig({
    agent: { harness: false, sessionId: "root" },
    retry: 0,
  }, {
    async create(options, workerOptions) {
      const fixture = createFixture({ holdCompaction: true });
      const worker = new LoopbackWorker(fixture.createAgent);
      fixtures.push(fixture);
      workers.push(worker);
      return createWorkerAgent(options, { ...workerOptions, worker });
    },
    async prepare() {},
  });
  const statuses = [];
  const unsubscribe = config.subscribeAgent({}, () => statuses.push(config.getAgent().status));
  await waitFor(() => config.getAgent().status === "success");
  const original = config.getAgent().data;
  const turn = original.turn.prompt({ input: "remain pending" });
  const pending = [turn.result(), original.session.compact(), original.session.compact()];
  for (const operation of pending) void operation.catch(() => {});
  await tick();

  workers[0].silence();
  t.mock.timers.tick(WORKER_HEARTBEAT_INTERVAL_MS);
  t.mock.timers.tick(WORKER_HEARTBEAT_TIMEOUT_MS);
  t.mock.timers.tick(WORKER_HEARTBEAT_TIMEOUT_MS);
  const settled = await Promise.allSettled(pending);
  await waitFor(() => config.getAgent().status === "error");

  const failure = config.getAgent().error;
  assert.equal(failure.code, "worker_unresponsive");
  assert.match(failure.message, /stopped responding; retry to start a fresh Worker/);
  assert.equal(settled.every(({ status, reason }) => status === "rejected" && reason === failure), true);
  assert.equal(workers[0].terminated, 1);
  assert.equal(
    fixtures[0].log.filter(([kind]) => kind === "agent-dispose").length,
    1,
  );
  assert.deepEqual(statuses, ["pending", "success", "error"]);

  config.refetchAgent();
  await waitFor(() => workers.length === 2 && config.getAgent().status === "success");
  assert.notStrictEqual(config.getAgent().data, original);
  assert.equal(workers[1].terminated, 0);
  assert.equal(fixtures[0].disposedAgents.has("root"), true);
  assert.deepEqual(statuses, ["pending", "success", "error", "pending", "success"]);

  turn.dispose();
  unsubscribe();
  await waitFor(() => workers[1].terminated === 1);
  await config.destroy();
});

test("a healthy Worker stays live throughout a long streamed turn", { timeout: 2_000 }, async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const fixture = createFixture();
  const worker = new LoopbackWorker(fixture.createAgent);
  const failures = [];
  const agent = await createWorkerAgent(
    { sessionId: "root", harness: false },
    { worker, onFailure: (error) => failures.push(error) },
  );
  const events = [];
  const watch = agent.events.watch();
  watch.onEvent((event) => events.push(event.seq));
  const turn = agent.turn.prompt({ input: "stream for a long time" });
  const pending = turn.result();
  await tick();

  for (let minute = 1; minute <= 10; minute += 1) {
    for (let heartbeat = 0; heartbeat < 6; heartbeat += 1) {
      t.mock.timers.tick(WORKER_HEARTBEAT_INTERVAL_MS);
      await tick();
    }
    fixture.emit("root", minute);
    await tick();
  }

  assert.deepEqual(events, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
  assert.equal(
    worker.incoming.filter(({ type }) => type === "liveness.ping").length,
    60,
  );
  assert.deepEqual(failures, []);
  assert.equal(worker.terminated, 0);

  fixture.complete("root", "healthy completion");
  const result = await pending;
  assert.equal(result.finalMessage, "healthy completion");
  result.dispose();
  turn.dispose();
  watch.off();
  agent.dispose();
  t.mock.timers.tick(
    WORKER_HEARTBEAT_INTERVAL_MS + (2 * WORKER_HEARTBEAT_TIMEOUT_MS),
  );
  assert.equal(worker.terminated, 1);
  assert.deepEqual(failures, []);
});

test("aborting a boot that never resolves terminates its Worker and permits replacement", { timeout: 2_000 }, async () => {
  const controller = new AbortController();
  const worker = new SilentWorker();
  const pending = createWorkerAgent(
    { sessionId: "hung", harness: false },
    { signal: controller.signal, worker },
  );
  assert.equal(worker.incoming.at(-1).type, "boot");

  controller.abort();
  await assert.rejects(pending, { name: "AbortError" });
  assert.equal(worker.terminated, 1);
  assert.equal(worker.onmessage, null);
  assert.equal(worker.onerror, null);
  assert.equal(worker.onmessageerror, null);

  const fixture = createFixture();
  const replacementWorker = new LoopbackWorker(fixture.createAgent);
  const replacement = await createWorkerAgent(
    { sessionId: "replacement", harness: false },
    { worker: replacementWorker },
  );
  assert.equal(replacement.sessionId, "replacement");
  replacement.dispose();
  assert.equal(replacementWorker.terminated, 1);
});

test("pending RPCs and structured-clone configuration fail closed at explicit bounds", async () => {
  await assert.rejects(
    createWorkerAgent({ tools: { custom: () => {} } }, { worker: () => { throw new Error("must not construct"); } }),
    /cannot contain functions.*Worker boundary/,
  );
  await assert.rejects(
    createWorkerAgent({
      transport: Transport.hostManaged({ createWebSocket() {} }),
      harness: false,
    }, { worker: () => { throw new Error("must not construct"); } }),
    /host-managed transport callbacks must live inside a custom Worker/,
  );

  const fixture = createFixture({ holdCompaction: true });
  const worker = new LoopbackWorker(fixture.createAgent);
  const agent = await createWorkerAgent({ sessionId: "root", harness: false }, { worker, maxPendingRpcs: 2 });
  const first = agent.session.compact();
  const second = agent.session.compact();
  void first.catch(() => {});
  void second.catch(() => {});
  assert.throws(() => agent.session.compact(), /bound of 2 pending RPCs/);
  worker.crash("bounded cleanup");
  await assert.rejects(first, /bounded cleanup/);
  await assert.rejects(second, /bounded cleanup/);

  const replacementFixture = createFixture();
  const replacementWorker = new LoopbackWorker(replacementFixture.createAgent);
  const replacement = await createWorkerAgent(
    { sessionId: "root", harness: false },
    { worker: replacementWorker },
  );
  replacement.dispose();
  assert.equal(replacementWorker.terminated, 1);
});

test("host-managed transport is a clone-safe Worker descriptor without app callbacks", async () => {
  const fixture = createFixture();
  const worker = new LoopbackWorker(fixture.createAgent);
  const agent = await createWorkerAgent({
    sessionId: "hosted",
    harness: false,
    transport: Transport.hostManaged({
      websocketUrl: "wss://nanocodex.example/api/responses",
    }),
  }, { worker });

  assert.equal(agent.sessionId, "hosted");
  agent.dispose();
  assert.equal(worker.terminated, 1);
});

test("rebooting a runtime disposes the replaced Agent and suppresses stale completion", async () => {
  const created = [];
  const outgoing = [];
  const scope = { onmessage: null, postMessage: (message) => outgoing.push(message) };
  const runtime = installWorkerAgentRuntime(scope, {
    async createAgent({ sessionId }) {
      const fixture = createFixture();
      const agent = await fixture.createAgent({ sessionId });
      created.push({ agent, fixture });
      return agent;
    },
  });
  scope.onmessage({ data: { protocol: "nanocodex.worker-agent.v1", channel: "old", type: "boot", config: { sessionId: "old", harness: false } } });
  await tick();
  scope.onmessage({ data: { protocol: "nanocodex.worker-agent.v1", channel: "new", type: "boot", config: { sessionId: "new", harness: false } } });
  await tick();
  assert.equal(created[0].fixture.disposedAgents.has("old"), true);
  assert.equal(outgoing.at(-1).channel, "new");
  runtime.dispose();
  assert.equal(created[1].fixture.disposedAgents.has("new"), true);
});

test("a stale boot rejection cannot dispose the ready replacement", async () => {
  const first = deferred();
  const replacement = createFixture();
  const outgoing = [];
  const scope = { onmessage: null, postMessage: (message) => outgoing.push(message) };
  let attempts = 0;
  const runtime = installWorkerAgentRuntime(scope, {
    createAgent(options) {
      attempts += 1;
      return attempts === 1 ? first.promise : replacement.createAgent(options);
    },
  });
  scope.onmessage({ data: {
    protocol: "nanocodex.worker-agent.v1",
    channel: "old",
    type: "boot",
    config: { sessionId: "old", harness: false },
  } });
  await tick();
  scope.onmessage({ data: {
    protocol: "nanocodex.worker-agent.v1",
    channel: "new",
    type: "boot",
    config: { sessionId: "new", harness: false },
  } });
  await tick();
  assert.equal(outgoing.at(-1).type, "ready");
  assert.equal(outgoing.at(-1).channel, "new");

  first.reject(new Error("late boot failure"));
  await tick();
  assert.equal(replacement.disposedAgents.has("new"), false);
  assert.equal(outgoing.at(-1).type, "ready");

  runtime.dispose();
  assert.equal(replacement.disposedAgents.has("new"), true);
});

test("a historical fork completing across reboot disposes its stale child", async () => {
  const created = [];
  const outgoing = [];
  const scope = { onmessage: null, postMessage: (message) => outgoing.push(message) };
  const runtime = installWorkerAgentRuntime(scope, {
    async createAgent({ sessionId }) {
      const fixture = createFixture({ holdBranches: true });
      const agent = await fixture.createAgent({ sessionId });
      created.push({ agent, fixture });
      return agent;
    },
  });
  const envelope = (channel, message) => ({
    data: { protocol: "nanocodex.worker-agent.v1", channel, ...message },
  });
  scope.onmessage(envelope("old", {
    type: "boot",
    config: { sessionId: "old", harness: false },
  }));
  await tick();
  scope.onmessage(envelope("old", {
    type: "prompt",
    id: "turn-1",
    agentId: "agent-1",
    turnId: "turn-1",
    options: { input: "checkpoint" },
  }));
  await tick();
  scope.onmessage(envelope("old", {
    type: "rpc",
    id: "completion",
    method: "turn.result",
    args: ["turn-1"],
  }));
  created[0].fixture.complete("old", "done");
  await tick();
  assert.equal(outgoing.find((message) => message.id === "completion").value.resultId, "result-1");

  scope.onmessage(envelope("old", {
    type: "rpc",
    id: "historical-fork",
    method: "agent.fork",
    args: ["agent-1", "result-1"],
  }));
  await tick();
  scope.onmessage(envelope("new", {
    type: "boot",
    config: { sessionId: "new", harness: false },
  }));
  await tick();
  created[0].fixture.releaseBranch();
  await tick();

  assert.equal(created[0].fixture.disposedAgents.has("old-fork"), true);
  assert.equal(outgoing.some((message) => message.id === "historical-fork"), false);
  assert.equal(outgoing.at(-1).channel, "new");
  runtime.dispose();
  assert.equal(created[1].fixture.disposedAgents.has("new"), true);
});

test("Worker runtime prewarms the engine and exact browser harness before boot", async () => {
  const outgoing = [];
  const warmed = [];
  const module = emptyWasmModule();
  const scope = { onmessage: null, postMessage: (message) => outgoing.push(message) };
  const runtime = installWorkerAgentRuntime(scope, {
    prewarmLocal(harness, options) { warmed.push({ harness, options }); },
  });

  scope.onmessage({ data: {
    protocol: "nanocodex.worker-agent.v1",
    channel: "warm",
    type: "prewarm",
    harness: { threadId: "thread-1", origin: "https://nanocodex.test" },
    module,
  } });
  await tick();

  assert.deepEqual(warmed, [{
    harness: { threadId: "thread-1", origin: "https://nanocodex.test" },
    options: { module },
  }]);
  assert.equal(outgoing.at(-1).type, "prewarmed");
  runtime.dispose();
});

test("stable browser harness identity opts into Worker-owned durability", async () => {
  const durableWorker = new HarnessWorker();
  const durableAgent = await createWorkerAgent({
    accountConnectionRequests: true,
    sessionId: "model-session",
    threadId: "browser-thread",
  }, { worker: durableWorker });
  const durableConfig = durableWorker.incoming.find(({ type }) => type === "boot").config;
  assert.equal(durableConfig.workerDurabilityId, "browser-thread");
  assert.equal(durableConfig.harness.accountConnectionRequests, true);
  assert.equal(Object.hasOwn(durableConfig, "accountConnectionRequests"), false);
  assert.equal(Object.hasOwn(durableConfig, "threadId"), false);
  durableAgent.dispose();

  const harnessFreeWorker = new HarnessWorker();
  const harnessFreeAgent = await createWorkerAgent({
    harness: false,
    sessionId: "model-session",
  }, { worker: harnessFreeWorker });
  const harnessFreeConfig = harnessFreeWorker.incoming.find(({ type }) => type === "boot").config;
  assert.equal(Object.hasOwn(harnessFreeConfig, "workerDurabilityId"), false);
  harnessFreeAgent.dispose();

  const explicitWorker = new HarnessWorker();
  const explicitAgent = await createWorkerAgent({
    durabilityId: "caller-owned",
    threadId: "browser-thread",
  }, { worker: explicitWorker });
  const explicitConfig = explicitWorker.incoming.find(({ type }) => type === "boot").config;
  assert.equal(Object.hasOwn(explicitConfig, "workerDurabilityId"), false);
  assert.equal(explicitConfig.durabilityId, "caller-owned");
  explicitAgent.dispose();

  const ephemeralWorker = new HarnessWorker();
  const ephemeralAgent = await createWorkerAgent({
    durability: false,
    threadId: "ephemeral-browser-thread",
  }, { worker: ephemeralWorker });
  const ephemeralConfig = ephemeralWorker.incoming.find(({ type }) => type === "boot").config;
  assert.equal(Object.hasOwn(ephemeralConfig, "workerDurabilityId"), false);
  assert.equal(Object.hasOwn(ephemeralConfig, "durability"), false);
  ephemeralAgent.dispose();
});

test("Worker hydration constructs default durability inside its own isolate", async () => {
  const outgoing = [];
  const fixture = createFixture();
  const durability = Object.freeze({ load() {}, append() {} });
  let createdOptions;
  let durabilityCreations = 0;
  const scope = { onmessage: null, postMessage: (message) => outgoing.push(message) };
  const runtime = installWorkerAgentRuntime(scope, {
    createAgent(options) {
      createdOptions = options;
      return fixture.createAgent(options);
    },
    createDurabilityStore() {
      durabilityCreations += 1;
      return durability;
    },
  });

  scope.onmessage({ data: {
    protocol: "nanocodex.worker-agent.v1",
    channel: "durable",
    type: "boot",
    config: {
      harness: false,
      sessionId: "model-session",
      workerDurabilityId: "browser-thread",
    },
  } });
  await tick();

  assert.equal(outgoing.at(-1).type, "ready");
  assert.equal(durabilityCreations, 1);
  assert.strictEqual(createdOptions.durability, durability);
  assert.equal(createdOptions.durabilityId, "browser-thread");
  assert.equal(Object.hasOwn(createdOptions, "workerDurabilityId"), false);
  runtime.dispose();
});

test("Worker runtime overlaps WASM initialization and browser harness restoration", async () => {
  const harness = { threadId: "thread-1", origin: "https://nanocodex.test" };
  const module = emptyWasmModule();
  const started = [];
  let finishEngine;
  let finishBrowser;
  const engineReady = new Promise((resolve) => { finishEngine = resolve; });
  const browserReady = new Promise((resolve) => { finishBrowser = resolve; });

  const prewarming = prewarmWorkerRuntime(
    harness,
    {
      module,
      async loadAgent() {
        started.push("agent");
      },
      async loadBrowser() {
        return {
          async prepareBrowser(options) {
            started.push("browser");
            assert.strictEqual(options, harness);
            await browserReady;
          },
        };
      },
      async loadEngine() {
        return {
          async initializeBrowserEngine(options) {
            assert.strictEqual(options.module, module);
            started.push("engine");
            await engineReady;
          },
        };
      },
    },
  );

  await tick();
  assert.deepEqual(new Set(started), new Set(["agent", "engine", "browser"]));
  finishBrowser();
  await tick();
  finishEngine();
  await prewarming;
});

test("cold Worker boot awaits package preparation before creating its Agent", async () => {
  const outgoing = [];
  const warmed = [];
  const module = emptyWasmModule();
  const ready = deferred();
  const fixture = createFixture();
  let creations = 0;
  const scope = { onmessage: null, postMessage: (message) => outgoing.push(message) };
  const runtime = installWorkerAgentRuntime(scope, {
    createAgent(options) {
      creations += 1;
      return fixture.createAgent(options);
    },
    prewarmLocal(harness, options) {
      warmed.push({ harness, options });
      return ready.promise;
    },
  });

  scope.onmessage({ data: {
    protocol: "nanocodex.worker-agent.v1",
    channel: "cold",
    type: "boot",
    config: { harness: false, module, sessionId: "cold" },
  } });
  await tick();

  assert.deepEqual(warmed, [{ harness: false, options: { module } }]);
  assert.equal(creations, 0);
  assert.equal(outgoing.length, 0);

  ready.resolve();
  await tick();
  assert.equal(creations, 1);
  assert.equal(outgoing.at(-1).type, "ready");
  runtime.dispose();
});

test("Worker runtime prewarms a harness-free Agent without loading browser tools", async () => {
  const started = [];
  await prewarmWorkerRuntime(false, {
    async loadAgent() { started.push("agent"); },
    async loadBrowser() { throw new Error("browser tools must stay lazy"); },
    async loadEngine() {
      return { async initializeBrowserEngine() { started.push("engine"); } };
    },
  });
  assert.deepEqual(new Set(started), new Set(["agent", "engine"]));
});

test("private Worker preparation replaces stale ownership and is claimed by Agent.create", async () => {
  const firstFixture = createFixture();
  const first = new LoopbackWorker(firstFixture.createAgent, { prewarmLocal() {} });
  await prepareWorkerAgent({ harness: false }, { worker: first });
  assert.equal(first.terminated, 0);

  const secondFixture = createFixture();
  const second = new LoopbackWorker(secondFixture.createAgent, { prewarmLocal() {} });
  await prepareWorkerAgent({
    origin: "https://nanocodex.test",
    threadId: "00000000-0000-4000-8000-000000000002",
  }, { worker: second });
  assert.equal(first.terminated, 1);

  const claimedFixture = createFixture();
  const claimed = new LoopbackWorker(claimedFixture.createAgent, { prewarmLocal() {} });
  await prepareWorkerAgent({ harness: false }, { worker: claimed });
  assert.equal(second.terminated, 1);

  const agent = await createWorkerAgent({ harness: false });
  assert.equal(agent.sessionId, "root");
  agent.dispose();
  assert.equal(claimed.terminated, 1);
});

test("preparation deduplicates and creation claims the exact complete harness identity", async () => {
  const worker = new HarnessWorker();
  const module = emptyWasmModule();
  const options = {
    module,
    origin: "https://nanocodex.test",
    sessionId: "session-1",
    threadId: "thread-1",
  };

  const firstPreparation = prepareWorkerAgent(options, { worker });
  const secondPreparation = prepareWorkerAgent(options);

  assert.equal(firstPreparation, secondPreparation);
  await firstPreparation;
  assert.equal(worker.incoming.filter(({ type }) => type === "prewarm").length, 1);

  const agent = await createWorkerAgent(options);
  const prewarm = worker.incoming.find(({ type }) => type === "prewarm");
  const boot = worker.incoming.find(({ type }) => type === "boot");
  const harness = {
    origin: "https://nanocodex.test",
    threadId: "thread-1",
  };

  assert.deepEqual(prewarm.harness, harness);
  assert(prewarm.module instanceof WebAssembly.Module);
  assert.deepEqual(boot.config.harness, harness);
  assert(boot.config.module instanceof WebAssembly.Module);
  assert.equal(boot.config.sessionId, "session-1");
  assert.equal(agent.sessionId, "session-1");
  agent.dispose();
  assert.equal(worker.terminated, 1);
});

test("preparation replaces a matching harness warmed with a different WASM module", async () => {
  const harness = { harness: false, sessionId: "session-1" };
  const first = new HarnessWorker();
  await prepareWorkerAgent({ ...harness, module: emptyWasmModule() }, { worker: first });

  const second = new HarnessWorker();
  const module = emptyWasmModule();
  await prepareWorkerAgent({ ...harness, module }, { worker: second });

  assert.equal(first.terminated, 1);
  const agent = await createWorkerAgent({ ...harness, module });
  assert.equal(agent.sessionId, "session-1");
  agent.dispose();
  assert.equal(second.terminated, 1);
});

test("non-disabled preparation rejects an incomplete resource identity", () => {
  assert.throws(
    () => prepareWorkerAgent({ origin: "https://nanocodex.test" }),
    /requires a stable threadId or sessionId/,
  );
});

test("preparation stays abort-owned until claim and leaves the next prewarm claimable", { timeout: 2_000 }, async () => {
  const controller = new AbortController();
  const silent = new SilentWorker();
  const pending = prepareWorkerAgent(
    { harness: false },
    { signal: controller.signal, worker: silent },
  );
  assert.equal(silent.incoming.at(-1).type, "prewarm");

  controller.abort();
  await assert.rejects(pending, { name: "AbortError" });
  assert.equal(silent.terminated, 1);
  assert.equal(silent.onmessage, null);
  assert.equal(silent.onerror, null);
  assert.equal(silent.onmessageerror, null);

  const readyController = new AbortController();
  const readyFixture = createFixture();
  const readyWorker = new LoopbackWorker(readyFixture.createAgent, { prewarmLocal() {} });
  await prepareWorkerAgent(
    { harness: false },
    { signal: readyController.signal, worker: readyWorker },
  );
  assert.equal(readyWorker.terminated, 0);
  readyController.abort();
  assert.equal(readyWorker.terminated, 1);

  const fixture = createFixture();
  const replacementWorker = new LoopbackWorker(fixture.createAgent, { prewarmLocal() {} });
  await prepareWorkerAgent({ harness: false }, { worker: replacementWorker });
  const replacement = await createWorkerAgent({ harness: false });
  replacement.dispose();
  assert.equal(replacementWorker.terminated, 1);
});

class SilentWorker {
  constructor() {
    this.onmessage = null;
    this.onerror = null;
    this.onmessageerror = null;
    this.incoming = [];
    this.terminated = 0;
  }

  postMessage(data) { this.incoming.push(data); }
  terminate() { this.terminated += 1; }
}

class ThrowingPrewarmTerminatorWorker extends SilentWorker {
  constructor(terminationFailure) {
    super();
    this.terminationFailure = terminationFailure;
  }

  fail(message) {
    const prewarm = this.incoming.find(({ type }) => type === "prewarm");
    this.onmessage?.({
      data: {
        channel: prewarm.channel,
        error: { name: "Error", message },
        protocol: prewarm.protocol,
        type: "fatal",
      },
    });
  }

  terminate() {
    this.terminated += 1;
    throw this.terminationFailure;
  }
}

class HarnessWorker {
  constructor() {
    this.onmessage = null;
    this.onerror = null;
    this.onmessageerror = null;
    this.incoming = [];
    this.terminated = 0;
  }

  postMessage(data) {
    const message = structuredClone(data);
    this.incoming.push(message);
    if (message.type === "prewarm") {
      queueMicrotask(() => this.onmessage?.({
        data: {
          channel: message.channel,
          protocol: message.protocol,
          type: "prewarmed",
        },
      }));
    } else if (message.type === "boot") {
      queueMicrotask(() => this.onmessage?.({
        data: {
          channel: message.channel,
          protocol: message.protocol,
          root: {
            agentId: "agent-1",
            sessionId: message.config.sessionId ?? "generated-session",
          },
          type: "ready",
        },
      }));
    }
  }

  terminate() { this.terminated += 1; }
}

class MismatchedSessionWorker {
  constructor() {
    this.onmessage = null;
    this.onerror = null;
    this.onmessageerror = null;
    this.incoming = [];
    this.rollback = undefined;
    this.terminated = 0;
  }

  postMessage(data) {
    const message = structuredClone(data);
    this.incoming.push(message);
    if (message.type === "boot") {
      queueMicrotask(() => this.onmessage?.({
        data: {
          channel: message.channel,
          protocol: message.protocol,
          root: { agentId: "booted-agent", sessionId: "wrong-session" },
          type: "ready",
        },
      }));
    } else if (message.type === "rpc" && message.method === "agent.dispose") {
      this.rollback = message;
    }
  }

  acknowledgeRollback() {
    const message = this.rollback;
    this.rollback = undefined;
    queueMicrotask(() => this.onmessage?.({
      data: {
        channel: message.channel,
        id: message.id,
        protocol: message.protocol,
        type: "resolve",
      },
    }));
  }

  terminate() { this.terminated += 1; }
}

class ReadyThenCrashWorker {
  constructor() {
    this.onmessage = null;
    this.onerror = null;
    this.onmessageerror = null;
    this.terminated = 0;
  }

  postMessage(message) {
    if (message.type !== "boot") return;
    this.onmessage?.({
      data: {
        channel: message.channel,
        protocol: message.protocol,
        root: { agentId: "raw-agent", sessionId: message.config.sessionId },
        type: "ready",
      },
    });
    queueMicrotask(() => this.onerror?.({ message: "crashed after ready" }));
  }

  terminate() { this.terminated += 1; }
}

class ThrowingConstructionWorker {
  constructor(failures) {
    this.failures = failures;
    this.installing = true;
  }

  set onmessage(value) {
    if (value === null) throw this.failures[1];
  }

  set onerror(value) {
    if (this.installing && typeof value === "function") {
      this.installing = false;
      throw this.failures[0];
    }
    if (value === null) throw this.failures[2];
  }

  set onmessageerror(value) {
    if (value === null) throw this.failures[3];
  }

  postMessage() {}
  terminate() { throw this.failures[4]; }
}

class SynchronousHeartbeatWorker {
  constructor() {
    this.onmessage = null;
    this.onerror = null;
    this.onmessageerror = null;
    this.terminated = 0;
  }

  postMessage(message) {
    if (message.type === "boot") {
      this.onmessage?.({
        data: {
          channel: message.channel,
          protocol: message.protocol,
          root: { agentId: "root", sessionId: message.config.sessionId },
          type: "ready",
        },
      });
    } else if (message.type === "liveness.ping") {
      this.onmessage?.({
        data: {
          channel: message.channel,
          protocol: message.protocol,
          sequence: message.sequence,
          type: "liveness.pong",
        },
      });
    }
  }

  terminate() { this.terminated += 1; }
}

class LoopbackWorker {
  constructor(createAgent, runtimeOptions = {}) {
    this.onmessage = null;
    this.onerror = null;
    this.onmessageerror = null;
    this.silent = false;
    this.terminated = 0;
    this.incoming = [];
    this.outgoing = [];
    this.scope = {
      onmessage: null,
      postMessage: (data, transfer) => {
        const cloned = cloneMessage(data, transfer);
        this.outgoing.push(cloned);
        if (!this.silent) queueMicrotask(() => this.onmessage?.({ data: cloned }));
      },
    };
    this.runtime = installWorkerAgentRuntime(this.scope, { createAgent, ...runtimeOptions });
  }

  postMessage(data) {
    const cloned = cloneMessage(data);
    this.incoming.push(cloned);
    if (!this.silent) queueMicrotask(() => this.scope.onmessage?.({ data: cloned }));
  }
  terminate() {
    if (this.terminated) return;
    this.terminated += 1;
    this.runtime.dispose();
  }
  crash(message) { this.onerror?.({ message }); }
  silence() { this.silent = true; }
}

function createFixture(options = {}) {
  const watchers = new Set();
  const completions = new Map();
  const acceptances = new Map();
  const disposedAgents = new Set();
  const log = [];
  const resultStats = { snapshots: 0, usages: 0, released: 0 };
  let releaseSnapshot;
  let releaseBranch;
  const watcherStats = { active: 0, created: 0, released: 0, options: [] };
  const runtime = defineRuntime({
    key: "worker-test",
    name: "Worker test Agent",
    type: "test",
    // This runtime is the simulated Worker isolate. Its page-side client owns
    // the reservation in the test process; a real Worker has a separate global.
    reserveSessions: false,
    create: ({ sessionId = "root" } = {}) => rawAgent(sessionId),
    dispose: (agent) => agent.free(),
    subscribe(listener) {
      let active = true;
      const watcher = { listeners: new Set() };
      watcher.listeners.add(listener);
      watchers.add(watcher);
      watcherStats.active += 1;
      watcherStats.created += 1;
      watcherStats.options.push({ includeAllSessions: true });
      return () => {
        if (!active) return;
        active = false;
        watcher.listeners.clear();
        watchers.delete(watcher);
        watcherStats.active -= 1;
        watcherStats.released += 1;
      };
    },
    decorate: (agent) => agent.extend(agentActions()),
  });
  const fixture = {
    disposedAgents,
    log,
    resultStats,
    releaseSnapshot() {
      if (!releaseSnapshot) throw new Error("no retained snapshot request is pending");
      const release = releaseSnapshot;
      releaseSnapshot = undefined;
      release();
    },
    releaseBranch() {
      if (!releaseBranch) throw new Error("no retained branch request is pending");
      const release = releaseBranch;
      releaseBranch = undefined;
      release();
    },
    watcherStats,
    accept(sessionId, requestId) {
      const acceptance = acceptances.get(sessionId);
      if (!acceptance) throw new Error(`no pending acceptance for ${sessionId}`);
      acceptances.delete(sessionId);
      acceptance.resolve(requestId);
    },
    emit(requestId, seq, payload = {}) {
      const event = { protocol_version: 1, request_id: requestId, seq, type: "test", payload };
      const encoded = JSON.stringify(event);
      const encodedBytes = Buffer.byteLength(encoded);
      for (const watcher of watchers) {
        for (const listener of watcher.listeners) listener(event, encodedBytes, encoded);
      }
    },
    complete(sessionId, finalMessage) {
      const completion = completions.get(sessionId);
      if (!completion) throw new Error(`no pending turn for ${sessionId}`);
      completions.delete(sessionId);
      const snapshot = Object.freeze({
        version: 1,
        model: "gpt-5.6-sol",
        lineage_id: sessionId,
        prompt_cache_key: sessionId,
        workspace: `/workspace/${sessionId}`,
        canonical_context: {},
        history: [],
      });
      const usage = Object.freeze({
        input_tokens: 1,
        cached_input_tokens: 0,
        cache_write_input_tokens: 0,
        output_tokens: 2,
        reasoning_output_tokens: 0,
        total_tokens: 3,
        estimated_cost: null,
        cost_status: "usage_not_reported",
      });
      let released = false;
      const encodedSnapshot = JSON.stringify(snapshot);
      completion({
        finalMessage,
        snapshot() {
          resultStats.snapshots += 1;
          if (options.invalidSnapshot) return "{";
          if (!options.holdSnapshot) return encodedSnapshot;
          return new Promise((resolve) => { releaseSnapshot = () => resolve(encodedSnapshot); });
        },
        usage() { resultStats.usages += 1; return JSON.stringify(usage); },
        free() {
          if (released) return;
          released = true;
          resultStats.released += 1;
        },
      });
    },
    createAgent(config = {}) { return createAgentClient(runtime, config); },
  };

  function rawAgent(sessionId) {
    let disposed = false;
    const agent = {
      sessionId,
      prompt(input, id) {
        log.push(["prompt", sessionId, input, id]);
        let resolve;
        const result = new Promise((accept) => { resolve = accept; });
        completions.set(sessionId, resolve);
        let acceptance;
        if (options.holdAcceptance) {
          acceptance = new Promise((resolveAcceptance) => {
            acceptances.set(sessionId, { resolve: resolveAcceptance });
          });
        }
        return {
          accepted() {
            if (options.acceptanceError) throw options.acceptanceError;
            return acceptance ?? id;
          },
          result: () => result,
          async steer(steering) { log.push(["steer", sessionId, steering]); },
          async cancel() { log.push(["cancel", sessionId]); },
          free() { log.push(["turn-dispose", sessionId]); },
        };
      },
      promptContent(input, id) { return agent.prompt(JSON.parse(input)[0].text, id); },
      async fork() { log.push(["fork", sessionId]); return branch(`${sessionId}-fork`); },
      async forkFrom(at) { log.push([at ? "fork-at" : "fork", sessionId]); return branch(`${sessionId}-fork`); },
      async spawn() { log.push(["spawn", sessionId]); return branch(`${sessionId}-spawn`); },
      compact() {
        log.push(["compact", sessionId]);
        return options.holdCompaction ? new Promise(() => {}) : Promise.resolve();
      },
      async context() {
        log.push(["context", sessionId]);
        return JSON.stringify({ workspace: `/workspace/${sessionId}`, history: [] });
      },
      async setThinking(value) { log.push(["thinking", sessionId, value]); },
      async setModel(value) { log.push(["model", sessionId, value]); },
      async setFastMode(value) { log.push(["fast", sessionId, value]); },
      async appendDeveloperMessage(text) {
        log.push(["developer", sessionId, text]);
        return JSON.stringify({ workspace: `/workspace/${sessionId}`, history: [] });
      },
      async startRealtimeConversation() {
        log.push(["realtime-start", sessionId]);
        return JSON.stringify({ workspace: `/workspace/${sessionId}`, history: [] });
      },
      async endRealtimeConversation() {
        log.push(["realtime-end", sessionId]);
        return JSON.stringify({ workspace: `/workspace/${sessionId}`, history: [] });
      },
      realtimeDelegation(input, transcript) {
        const entries = JSON.parse(transcript);
        log.push(["realtime-delegation", sessionId, input, entries]);
        return `canonical:${input}:${entries.map(({ role, text }) => `${role}: ${text}`).join("\n")}`;
      },
      realtimeTailDelegation(transcript) {
        const entries = JSON.parse(transcript);
        log.push(["realtime-tail", sessionId, entries]);
        return entries.length ? "canonical-tail" : undefined;
      },
      browserVoice(voice) {
        log.push(["voice-create", sessionId, voice]);
        return {
          async start() { log.push(["voice-start", sessionId]); },
          callBody(sdp) {
            log.push(["voice-call", sessionId, sdp]);
            return JSON.stringify({
              session_id: sessionId,
              call_body: JSON.stringify({
                sdp,
                session: { audio: { output: { voice } } },
              }),
            });
          },
          completeCall(body, location) {
            log.push(["voice-complete", sessionId, body, location]);
            return JSON.stringify({ call_id: "rtc_test", sdp: body });
          },
          sidebandUrl(callId) {
            log.push(["voice-sideband", sessionId, callId]);
            return `/sideband/${callId}`;
          },
          sidebandOpened() {
            log.push(["voice-sideband-opened", sessionId]);
            return JSON.stringify({ frames: [], transcripts: [], schedule_flush: false });
          },
          sidebandClosed(connectedMs) {
            log.push(["voice-sideband-closed", sessionId, connectedMs]);
            return JSON.stringify({
              frames: [],
              transcripts: [],
              reconnect_after_ms: 200,
              schedule_flush: false,
            });
          },
          framesSent(count) { log.push(["voice-frames-sent", sessionId, count]); },
          requiresAgentAdmission(payload) {
            log.push(["voice-admission", sessionId, payload]);
            return JSON.parse(payload).type === "delegation.created";
          },
          realtimeMessage(payload) {
            log.push(["voice-message", sessionId, payload]);
            return JSON.stringify({ frames: [], transcripts: [], schedule_flush: false });
          },
          agentEvent(payload) {
            log.push(["voice-event", sessionId, payload]);
            return JSON.stringify({ frames: [], transcripts: [], schedule_flush: false });
          },
          flush(finalChunk) {
            log.push(["voice-flush", sessionId, finalChunk]);
            return JSON.stringify({ frames: [], transcripts: [], schedule_flush: false });
          },
          async stop() {
            log.push(["voice-stop", sessionId]);
            return JSON.stringify({ frames: ['{"type":"session.close"}'], transcripts: [], schedule_flush: false });
          },
          async cancel() { log.push(["voice-cancel", sessionId]); return true; },
          preferredPhysicalInput(current, labels) {
            log.push(["voice-input", sessionId, current, labels]);
            return 0;
          },
          free() { log.push(["voice-free", sessionId]); },
        };
      },
      async shutdown() { log.push(["shutdown", sessionId]); },
      free() {
        if (disposed) return;
        disposed = true;
        disposedAgents.add(sessionId);
        log.push(["agent-dispose", sessionId]);
      },
    };
    return agent;

    function branch(childSessionId) {
      const child = rawAgent(childSessionId);
      if (!options.holdBranches) return child;
      return new Promise((resolve) => { releaseBranch = () => resolve(child); });
    }
  }
  return fixture;
}

function emptyWasmModule() {
  return new WebAssembly.Module(new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0]));
}

function cloneMessage(data, transfer) {
  return transfer?.length
    ? structuredClone(data, { transfer })
    : structuredClone(data);
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((next, fail) => {
    resolve = next;
    reject = fail;
  });
  return { promise, reject, resolve };
}

function tick() { return new Promise((resolve) => setImmediate(resolve)); }

async function waitFor(predicate) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (predicate()) return;
    await tick();
  }
  throw new Error("condition did not become true");
}
