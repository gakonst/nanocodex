import assert from "node:assert/strict";
import test from "node:test";
import { activePhoneCall, steerablePhoneCall, pollPhoneCalls, steeringOperation } from "./phoneCalls.ts";

test("steering retries preserve the request identity only for identical instructions", () => {
  const first = steeringOperation(undefined, "Ask about delivery");
  assert.equal(steeringOperation(first, first.instructions), first);
  assert.notEqual(steeringOperation(first, "Ask about pickup").operation_id, first.operation_id);
  assert.notEqual(steeringOperation(undefined, first.instructions).operation_id, first.operation_id);
});
test("hidden tabs and inactive chats do not poll", () => {
  assert.equal(pollPhoneCalls(true, "visible"), true);
  assert.equal(pollPhoneCalls(true, "hidden"), false);
  assert.equal(pollPhoneCalls(false, "visible"), false);
});
test("terminal and unfamiliar statuses cannot receive active call actions", () => {
  for (const status of ["completed", "busy", "failed", "no-answer", "canceled", "unexpected"]) assert.equal(activePhoneCall(status), false);
  for (const status of ["preparing", "unknown", "queued", "initiated", "ringing", "in-progress"]) assert.equal(activePhoneCall(status), true);
});

test("preparing and unknown calls allow hangup but cannot be steered", () => {
  for (const status of ["preparing", "unknown"]) {
    assert.equal(activePhoneCall(status), true);
    assert.equal(steerablePhoneCall(status), false);
  }
  for (const status of ["queued", "initiated", "ringing", "in-progress"]) assert.equal(steerablePhoneCall(status), true);
  for (const status of ["completed", "failed", "canceled"]) assert.equal(steerablePhoneCall(status), false);
});
