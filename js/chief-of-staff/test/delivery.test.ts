import assert from "node:assert/strict";
import test from "node:test";
import { claimDelivery, completeDelivery, releaseDelivery } from "../src/delivery.ts";

test("delivery claims fence concurrent callbacks until the lease expires", () => {
  const first = claimDelivery(undefined, 100, 200, "first");
  assert.equal(first.status, "claimed");
  assert.equal(claimDelivery(first.record, 150, 250, "second").status, "in_progress");

  const reclaimed = claimDelivery(first.record, 200, 300, "second");
  assert.equal(reclaimed.status, "claimed");
  assert.equal(reclaimed.token, "second");
});

test("only the active claim can complete or release a delivery", () => {
  const claim = claimDelivery(undefined, 100, 200, "active");
  assert.equal(completeDelivery(claim.record, "stale"), undefined);
  assert.equal(releaseDelivery(claim.record, "stale"), false);

  const completed = completeDelivery(claim.record, "active");
  assert.deepEqual(completed, { status: "completed" });
  assert.equal(claimDelivery(completed, 300, 400, "another").status, "completed");
});
