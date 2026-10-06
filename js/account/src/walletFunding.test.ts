import assert from "node:assert/strict";
import test from "node:test";

import {
  decodeWalletBalance,
} from "./walletFunding.ts";

const account = "0x1111111111111111111111111111111111111111";

test("decodes the canonical MACH balance and rejects negative or foreign-account values", () => {
  const balance = decodeWalletBalance({
    account,
    balance: "12345678",
    decimals: 6,
    symbol: "MACH",
    token: "0x20c000000000000000000000f37de3740ADec032",
  }, account.toUpperCase());
  assert.equal(balance.atomics, 12_345_678n);
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
