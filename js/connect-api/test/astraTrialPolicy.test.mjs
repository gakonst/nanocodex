import assert from "node:assert/strict";
import test from "node:test";

import {
  astraTrialAppId,
  astraTrialAppOrigin,
  astraTrialMppLimit,
  hasConsistentAstraTrialIdentity,
  hasAstraTrialSpendPolicy,
} from "../src/astraTrialPolicy.mts";

const mach = "0x20c000000000000000000000f37de3740adec032";
const recipient = "0x1234567890abcdef1234567890abcdef12345678";

function policy() {
  return {
    limits: [{ token: mach, limit: astraTrialMppLimit.toString(), period: 86_400 }],
    scopes: [{ address: mach, selector: "0x95777d59", recipients: [recipient] }],
  };
}

test("pins the registered Astra trial identity and one-shot amount", () => {
  assert.equal(astraTrialAppId, "astra-one-shot");
  assert.equal(astraTrialAppOrigin, "https://nanocodex-astra-mpp-trial.gakonst.workers.dev");
  assert.equal(astraTrialMppLimit, 100_000n);
  assert.equal(hasConsistentAstraTrialIdentity(astraTrialAppId, astraTrialAppOrigin), true);
  assert.equal(hasConsistentAstraTrialIdentity("another-app", "https://another.example"), true);
  assert.equal(hasConsistentAstraTrialIdentity(astraTrialAppId, "https://another.example"), false);
  assert.equal(hasConsistentAstraTrialIdentity("another-app", astraTrialAppOrigin), false);
  const value = policy();
  assert.equal(hasAstraTrialSpendPolicy(value.limits, value.scopes), true);
});

test("accepts only one MACH transferWithMemo scope to one nonzero recipient", () => {
  const value = policy();
  assert.equal(hasAstraTrialSpendPolicy([
    ...value.limits,
    { token: mach, limit: "1", period: 86_400 },
  ], value.scopes), false);
  assert.equal(hasAstraTrialSpendPolicy(value.limits, [
    ...value.scopes,
    { address: mach, selector: "0xa9059cbb", recipients: [recipient] },
  ]), false);
  assert.equal(hasAstraTrialSpendPolicy(value.limits, [{
    ...value.scopes[0],
    selector: "0xa9059cbb",
  }]), false);
  assert.equal(hasAstraTrialSpendPolicy(value.limits, [{
    ...value.scopes[0],
    recipients: ["0x0000000000000000000000000000000000000000"],
  }]), false);
  assert.equal(hasAstraTrialSpendPolicy([{ ...value.limits[0], limit: "49999999" }], value.scopes), false);
});
