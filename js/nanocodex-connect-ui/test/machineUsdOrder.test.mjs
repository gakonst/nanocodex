import assert from "node:assert/strict";
import test from "node:test";

import { classifyMachineUsdOrder } from "../dist/machineUsdOrder.mjs";

test("requires_payment remains pending while Stripe webhooks catch up", () => {
  assert.equal(classifyMachineUsdOrder({ status: "requires_payment" }), "pending");
});

test("processing and issuing remain pending", () => {
  assert.equal(classifyMachineUsdOrder({ status: "processing" }), "pending");
  assert.equal(classifyMachineUsdOrder({ status: "issuing" }), "pending");
});

test("only fulfilled issuance completes an order", () => {
  assert.equal(classifyMachineUsdOrder({
    status: "complete",
    issuance_transaction_hash: "0x1234",
  }), "complete");
  assert.throws(
    () => classifyMachineUsdOrder({ status: "complete" }),
    /order response is invalid/,
  );
});

test("the public failed state is terminal", () => {
  assert.equal(classifyMachineUsdOrder({ status: "failed" }), "failed");
});
