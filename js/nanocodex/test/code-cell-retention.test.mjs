import assert from "node:assert/strict";
import { test } from "node:test";
import { webcrypto } from "node:crypto";
import { createCodeRuntime } from "../../nanocodex-tools/runtime/code-runtime.mjs";

globalThis.crypto ??= webcrypto;
const text = receipt => typeof receipt.output === "string" ? receipt.output
  : receipt.output.filter(item => item.type === "input_text").map(item => item.text).join("\n");
const parse = async receipt => JSON.parse(await receipt);
const id = receipt => text(receipt).match(/Script running with cell ID ([^\s]+)/)?.[1];
const tick = () => new Promise(resolve => setImmediate(resolve));

async function yielded() {
  let release, started;
  const gate = new Promise(resolve => { release = resolve; });
  const admitted = new Promise(resolve => { started = resolve; });
  let calls = 0;
  const runtime = createCodeRuntime({ effect: { async handler() {
    calls++;
    started();
    await gate;
    return { receipt: "synthetic-once" };
  } } });
  runtime.beginTurn("owner");
  const pending = runtime.executeCodeObserved(
    'text("partial"); text(await tools.effect({})); text("finished");', "owner", "origin");
  await admitted;
  assert.equal(runtime.preempt("owner", "origin"), true);
  const first = await parse(pending);
  assert.ok(id(first));
  assert.match(text(first), /partial/);
  return { runtime, first, release, calls: () => calls };
}

// This integration boundary executes the real native evaluator and cell
// registry. It characterizes lifecycle loss without replaying production work;
// it does not establish which lifecycle occurred in a production incident.
test("completion between yielded exec and wait retains final output and receipt exactly once", async () => {
  const cell = await yielded();
  try {
    cell.release();
    await tick();
    const final = await parse(cell.runtime.waitCodeObserved(
      JSON.stringify({ cell_id: id(cell.first) }), "owner", "wait"));
    assert.equal(final.success, true);
    assert.equal(final.cell.running, false);
    assert.equal(final.cell.origin_call_id, "origin");
    assert.match(text(final), /Script completed/);
    assert.match(text(final), /finished/);
    assert.doesNotMatch(text(final), /partial/);
    assert.equal(final.nested_calls.length, 1);
    assert.equal(cell.calls(), 1);
    console.log(JSON.stringify({ scenario: "completion-before-wait", initial: cell.first, final, calls: cell.calls() }));
  } finally { cell.release(); await cell.runtime.reset(); }
});

for (const loss of ["cancel-turn", "release-session", "replace-runtime"]) {
  test(`${loss} after yielded exec reproduces missing cell without a second effect`, async () => {
    const cell = await yielded();
    let replacement;
    try {
      if (loss === "cancel-turn") cell.runtime.cancelTurn("owner");
      if (loss === "release-session") cell.runtime.releaseSession("owner");
      if (loss === "replace-runtime") {
        await cell.runtime.reset();
        replacement = createCodeRuntime();
      }
      const final = await parse((replacement ?? cell.runtime).waitCodeObserved(
        JSON.stringify({ cell_id: id(cell.first) }), "owner", "wait"));
      assert.equal(final.success, false);
      assert.match(text(final), /CODE_CELL_UNAVAILABLE: exec cell .* not found/);
      assert.match(text(final), /execution outcome is unknown/);
      assert.match(text(final), /Do not rerun the script or retry uncertain effects/);
      assert.equal(final.cell, undefined); // No fabricated terminal receipt.
      assert.deepEqual(final.nested_calls, []);
      assert.match(text(final), loss === "replace-runtime"
        ? /different runtime generation/ : /unavailable in this session/);
      const again = await parse((replacement ?? cell.runtime).waitCodeObserved(
        JSON.stringify({ cell_id: id(cell.first), terminate: true }), "owner", "wait-again"));
      assert.equal(again.success, false);
      assert.match(text(again), /cannot resume it or confirm termination/);
      assert.equal(cell.calls(), 1);
      console.log(JSON.stringify({ scenario: loss, initial: cell.first, final, calls: cell.calls() }));
    } finally { cell.release(); await cell.runtime.reset(); await replacement?.reset(); }
  });
}


test("wrong session cannot inspect, consume, or terminate another session's yielded cell", async () => {
  const cell = await yielded();
  try {
    const wrong = await parse(cell.runtime.waitCodeObserved(
      JSON.stringify({ cell_id: id(cell.first), terminate: true }), "other", "wait"));
    assert.equal(wrong.success, false);
    assert.match(text(wrong), /unavailable in this session/);
    assert.doesNotMatch(text(wrong), /synthetic-once|partial/);
    assert.equal(wrong.cell, undefined);
    cell.release();
    await tick();
    const final = await parse(cell.runtime.waitCodeObserved(
      JSON.stringify({ cell_id: id(cell.first) }), "owner", "wait"));
    assert.equal(final.success, true);
    assert.match(text(final), /Script completed/);
    assert.equal(final.nested_calls.length, 1);
    assert.equal(cell.calls(), 1);
  } finally { cell.release(); await cell.runtime.reset(); }
});

test("consumed final receipt is not misreported as runtime replacement or replayed", async () => {
  const cell = await yielded();
  try {
    cell.release();
    await tick();
    const first = await parse(cell.runtime.waitCodeObserved(
      JSON.stringify({ cell_id: id(cell.first) }), "owner", "wait"));
    assert.equal(first.success, true);
    const second = await parse(cell.runtime.waitCodeObserved(
      JSON.stringify({ cell_id: id(cell.first) }), "owner", "wait-again"));
    assert.equal(second.success, false);
    assert.match(text(second), /CODE_CELL_UNAVAILABLE/);
    assert.doesNotMatch(text(second), /different runtime generation/);
    assert.deepEqual(second.nested_calls, []);
    assert.equal(cell.calls(), 1);
  } finally { cell.release(); await cell.runtime.reset(); }
});

const drain = async (runtime, callId) => {
  const updates = [];
  for (let update; (update = await runtime.nextCodeUpdate("owner", callId)) !== null;) updates.push(JSON.parse(update));
  return updates;
};

// A model can answer without waiting on a yielded cell. The host relay keeps
// that cell's nested work reporting after the turn, so its call terminates.
test("turn completion relays an unawaited cell's nested completion exactly once", async () => {
  const cell = await yielded();
  try {
    const relays = JSON.parse(cell.runtime.detachTurn("owner"));
    assert.equal(relays.length, 1);
    assert.equal(relays[0].origin_call_id, "origin");
    assert.ok(relays[0].relay_id.startsWith(`relay:${id(cell.first)}:`));
    assert.equal(cell.runtime.detachTurn("owner"), "[]", "a relayed cell is never detached twice");
    const relayed = drain(cell.runtime, relays[0].relay_id);
    cell.release();
    const updates = await relayed;
    assert.deepEqual(updates.map(update => update.type), ["nested_call_completed"]);
    assert.equal(updates[0].call.call_id, "origin/code-1");
    // A later wait still returns the final output but never re-delivers the update.
    const waited = cell.runtime.waitCodeObserved(JSON.stringify({ cell_id: id(cell.first) }), "owner", "wait");
    const later = drain(cell.runtime, "wait");
    const final = await parse(waited);
    assert.equal(final.success, true);
    assert.match(text(final), /finished/);
    assert.deepEqual(await later, []);
    assert.equal(cell.calls(), 1);
  } finally { cell.release(); await cell.runtime.reset(); }
});

test("a wait that attaches to a relayed cell ends the relay and takes later updates", async () => {
  const cell = await yielded();
  try {
    const [relay] = JSON.parse(cell.runtime.detachTurn("owner"));
    const relayed = drain(cell.runtime, relay.relay_id);
    const waited = cell.runtime.waitCodeObserved(JSON.stringify({ cell_id: id(cell.first), yield_time_ms: 60_000 }), "owner", "wait");
    const later = drain(cell.runtime, "wait");
    assert.deepEqual(await relayed, [], "the relay ends when the wait attaches");
    cell.release();
    const final = await parse(waited);
    assert.equal(final.success, true);
    assert.deepEqual((await later).map(update => update.type), ["nested_call_completed"]);
  } finally { cell.release(); await cell.runtime.reset(); }
});

test("an aborted relayed cell still reports its nested call's terminal result", async () => {
  const cell = await yielded();
  try {
    const [relay] = JSON.parse(cell.runtime.detachTurn("owner"));
    const relayed = drain(cell.runtime, relay.relay_id);
    cell.runtime.cancel("owner");
    const updates = await relayed;
    assert.deepEqual(updates.map(update => update.type), ["nested_call_completed"]);
    assert.equal(updates[0].call.call_id, "origin/code-1");
  } finally { cell.release(); await cell.runtime.reset(); }
});

test("cancelling a turn relays the dropped observer's pending and aborted nested results once", async () => {
  const cell = await yielded();
  try {
    // A wait is observing when the turn is cancelled; its reader is dropped.
    const waited = cell.runtime.waitCodeObserved(JSON.stringify({ cell_id: id(cell.first), yield_time_ms: 60_000 }), "owner", "wait");
    await tick();
    const relays = JSON.parse(cell.runtime.cancelTurnWithUpdates("owner"));
    assert.equal(relays.length, 1);
    assert.equal(relays[0].origin_call_id, "origin");
    const updates = await drain(cell.runtime, relays[0].relay_id);
    assert.deepEqual(updates.map(update => update.type), ["nested_call_completed"]);
    assert.equal(updates[0].call.call_id, "origin/code-1");
    assert.equal(updates[0].call.success, false);
    assert.equal(updates[0].call.structured_result.outcome, "unknown");
    await waited;
    assert.equal(cell.runtime.cancelTurnWithUpdates("owner"), "[]", "a cancelled cell is not relayed again");
  } finally { cell.release(); await cell.runtime.reset(); }
});

test("cancelling a later turn leaves an earlier turn's relayed cell untouched", async () => {
  const cell = await yielded();
  try {
    const [relay] = JSON.parse(cell.runtime.detachTurn("owner"));
    cell.runtime.beginTurn("owner");
    assert.equal(cell.runtime.cancelTurnWithUpdates("owner"), "[]");
    const relayed = drain(cell.runtime, relay.relay_id);
    cell.release();
    assert.deepEqual((await relayed).map(update => update.type), ["nested_call_completed"]);
    assert.equal((await relayed)[0].call.success, true);
  } finally { cell.release(); await cell.runtime.reset(); }
});

test("a cell relayed again after a wait gets a fresh relay id", async () => {
  const cell = await yielded();
  try {
    const [first] = JSON.parse(cell.runtime.detachTurn("owner"));
    const firstDrain = drain(cell.runtime, first.relay_id);
    const waited = cell.runtime.waitCodeObserved(JSON.stringify({ cell_id: id(cell.first), yield_time_ms: 10 }), "owner", "wait");
    assert.equal((await parse(waited)).cell.running, true);
    const [second] = JSON.parse(cell.runtime.detachTurn("owner"));
    assert.notEqual(second.relay_id, first.relay_id);
    assert.deepEqual(await firstDrain, []);
    const secondDrain = drain(cell.runtime, second.relay_id);
    cell.release();
    assert.deepEqual((await secondDrain).map(update => update.type), ["nested_call_completed"]);
  } finally { cell.release(); await cell.runtime.reset(); }
});

test("cancelling the turn that is waiting on an earlier turn's cell relays its terminal once", async () => {
  const cell = await yielded();
  try {
    const [detached] = JSON.parse(cell.runtime.detachTurn("owner"));
    const detachedDrain = drain(cell.runtime, detached.relay_id);
    cell.runtime.beginTurn("owner");
    // The later turn waits on the earlier cell, then is cancelled mid-wait.
    const waited = cell.runtime.waitCodeObserved(JSON.stringify({ cell_id: id(cell.first), yield_time_ms: 60_000 }), "owner", "wait");
    await tick();
    assert.deepEqual(await detachedDrain, [], "the wait ended the detach relay");
    const relays = JSON.parse(cell.runtime.cancelTurnWithUpdates("owner"));
    assert.equal(relays.length, 1, "the waited cell belongs to the cancelled turn");
    const updates = await drain(cell.runtime, relays[0].relay_id);
    assert.deepEqual(updates.map(update => update.type), ["nested_call_completed"]);
    assert.equal(updates[0].call.structured_result.outcome, "unknown");
    await waited;
  } finally { cell.release(); await cell.runtime.reset(); }
});
