import assert from "node:assert/strict";
import test from "node:test";

import { classifyMachineUsdOrder } from "../dist/machineUsdOrder.mjs";

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
