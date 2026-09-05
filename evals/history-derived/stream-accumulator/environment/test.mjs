import assert from "node:assert/strict";
import { applyEvent, createState } from "./accumulator.mjs";

let state = createState();
state = applyEvent(state, {
  type: "file_chunk",
  path: "a.rs",
  sequence: 0,
  text: "a",
});
assert.equal(state.files[0].text, "a");
console.log("ok");
