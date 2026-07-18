import assert from "node:assert/strict";
import { applyEvent, createState } from "/app/accumulator.mjs";

let state = createState();
state = applyEvent(state, { type: "file_chunk", path: "a.rs", sequence: 0, text: "a0" });
state = applyEvent(state, { type: "file_chunk", path: "b.rs", sequence: 0, text: "b0" });

const firstA = state.files[0];
const firstB = state.files[1];
const beforeUpdate = state;
state = applyEvent(state, { type: "file_chunk", path: "a.rs", sequence: 1, text: "a1" });
assert.equal(beforeUpdate.files[0].text, "a0", "input state was mutated");
assert.notEqual(state.files[0], firstA, "changed file retained stale identity");
assert.equal(state.files[1], firstB, "unchanged file identity was not preserved");
assert.deepEqual(state.files.map((file) => file.path), ["a.rs", "b.rs"]);

const afterUpdate = state;
assert.equal(
  applyEvent(state, { type: "file_chunk", path: "a.rs", sequence: 1, text: "duplicate" }),
  state,
  "duplicate sequence should be ignored without rebuilding state",
);
assert.equal(
  applyEvent(state, { type: "file_chunk", path: "a.rs", sequence: 0, text: "old" }),
  state,
  "older sequence should be ignored without rebuilding state",
);

state = applyEvent(state, { type: "snapshot_start", generation: 7 });
assert.equal(state.files, afterUpdate.files, "snapshot start flashed or rebuilt visible files");
assert.equal(state.pending.generation, 7);

state = applyEvent(state, { type: "file_chunk", path: "c.rs", sequence: 0, text: "c0" });
state = applyEvent(state, { type: "file_chunk", path: "a.rs", sequence: 0, text: "fresh" });
assert.equal(state.files, afterUpdate.files, "pending snapshot changed visible files");
assert.deepEqual(state.pending.files.map((file) => file.path), ["c.rs", "a.rs"]);

const pending = state;
assert.equal(
  applyEvent(state, { type: "snapshot_commit", generation: 6 }),
  state,
  "stale commit should be ignored",
);

state = applyEvent(state, { type: "snapshot_commit", generation: 7 });
assert.equal(state.generation, 7);
assert.equal(state.pending, null);
assert.equal(state.files, pending.pending.files, "matching commit should atomically swap the pending files");
assert.deepEqual(state.files, [
  { path: "c.rs", sequence: 0, text: "c0" },
  { path: "a.rs", sequence: 0, text: "fresh" },
]);
