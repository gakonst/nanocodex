import assert from "node:assert/strict";
import { test } from "node:test";

import { createPaymentSessionOwner } from "../src/paymentSessionOwner.ts";

test("the example closes replaced and failed MPP sessions exactly once", async () => {
  const owner = createPaymentSessionOwner<FakePaymentSession>();
  const first = session();
  const failed = session();

  await owner.open(async () => first, async () => "agent");
  await assert.rejects(
    owner.open(
      async () => failed,
      async () => {
        throw new Error("Agent.create failed");
      },
    ),
    /Agent\.create failed/,
  );
  await owner.clear();

  assert.equal(first.closes, 1);
  assert.equal(failed.closes, 1);
});

function session(): FakePaymentSession {
  const value: FakePaymentSession = {
    closes: 0,
    mpp: {
      close() {
        value.closes += 1;
      },
    },
  };
  return value;
}

type FakePaymentSession = {
  closes: number;
  mpp: { close(): void };
};
