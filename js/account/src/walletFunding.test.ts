import assert from "node:assert/strict";
import test from "node:test";

import {
  classifyFundingOrder,
  decodeFundingAttempt,
  decodeMachineUsdConfig,
  decodeWalletBalance,
  defaultFundingAmountCents,
  formatDollars,
  formatWalletBalance,
} from "./walletFunding.ts";

const account = "0x1111111111111111111111111111111111111111";

test("decodes and formats the canonical MACH balance", () => {
  const balance = decodeWalletBalance({
    account,
    balance: "12345678",
    decimals: 6,
    symbol: "MACH",
    token: "0x20c000000000000000000000f37de3740ADec032",
  }, account.toUpperCase());
  assert.equal(balance.atomics, 12_345_678n);
  assert.equal(formatWalletBalance(balance), "$12.345678");
  assert.equal(formatWalletBalance({ ...balance, atomics: 0n }), "$0.00");
  assert.equal(formatWalletBalance({ ...balance, atomics: 1_200_000n }), "$1.20");
  assert.throws(() => decodeWalletBalance({
    account,
    balance: "-1",
    decimals: 6,
    symbol: "MACH",
    token: "0x20c000000000000000000000f37de3740ADec032",
  }, account));
  assert.throws(() => decodeWalletBalance({
    account,
    balance: "12345678",
    decimals: 6,
    symbol: "MACH",
    token: "0x20c000000000000000000000f37de3740ADec032",
  }, "0x2222222222222222222222222222222222222222"));
});

test("validates MACH onramp limits and selects the one-click amount", () => {
  const config = decodeMachineUsdConfig({
    chain_id: 4217,
    min_usd_amount_cents: 500,
    max_usd_amount_cents: 10_000,
    onramp_enabled: true,
    stripe_publishable_key: "pk_test_example",
    token_address: "0x20c000000000000000000000f37de3740ADec032",
  });
  assert.equal(defaultFundingAmountCents(config), 500);
  assert.equal(defaultFundingAmountCents({ ...config, minUsdAmountCents: 700 }), 700);
  assert.equal(defaultFundingAmountCents({ ...config, minUsdAmountCents: 100, maxUsdAmountCents: 400 }), 400);
  assert.equal(formatDollars(500), "$5.00");
  assert.throws(() => decodeMachineUsdConfig({
    chain_id: 1,
    min_usd_amount_cents: 500,
    max_usd_amount_cents: 10_000,
    stripe_publishable_key: "pk_test_example",
    token_address: "0x20c000000000000000000000f37de3740ADec032",
  }), /configuration/);
});

test("accepts only Stripe hosted checkout and known order states", () => {
  assert.deepEqual(decodeFundingAttempt({
    order: { id: "order-1" },
    payment: { checkout_url: "https://checkout.stripe.com/c/pay/cs_test" },
  }, "token"), {
    checkoutUrl: "https://checkout.stripe.com/c/pay/cs_test",
    id: "order-1",
    orderToken: "token",
  });
  assert.throws(() => decodeFundingAttempt({
    order: { id: "order-1" },
    payment: { checkout_url: "https://example.com/checkout" },
  }, "token"), /checkout URL/);
  assert.throws(() => decodeFundingAttempt({
    order: { id: "order-1" },
    payment: { checkout_url: "https://user:secret@checkout.stripe.com/c/pay/cs_test" },
  }, "token"), /checkout URL/);
  for (const status of ["requires_payment", "processing", "issuing"]) {
    assert.equal(classifyFundingOrder({ status }), "pending");
  }
  assert.equal(classifyFundingOrder({ status: "failed" }), "failed");
  assert.equal(classifyFundingOrder({
    status: "complete",
    issuance_transaction_hash: `0x${"1".repeat(64)}`,
  }), "complete");
  assert.throws(() => classifyFundingOrder({
    status: "complete",
    issuance_transaction_hash: "0x01",
  }), /order response/);
});
