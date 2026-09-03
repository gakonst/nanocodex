import assert from "node:assert/strict";
import test from "node:test";

import { normalizeSmsPhone } from "../dist/smsPhone.js";

test("normalizes common international phone input without guessing local numbers", () => {
  assert.equal(normalizeSmsPhone("13417327405"), "+13417327405");
  assert.equal(normalizeSmsPhone("1 (341) 732-7405"), "+13417327405");
  assert.equal(normalizeSmsPhone("00 30 697 123 4567"), "+306971234567");
  assert.equal(normalizeSmsPhone("+30 697 123 4567"), "+306971234567");

  assert.equal(normalizeSmsPhone("3417327405"), undefined);
  assert.equal(normalizeSmsPhone("01234567890"), undefined);
  assert.equal(normalizeSmsPhone("5511987654321"), undefined);
  assert.equal(normalizeSmsPhone("+1 341 CALL-NOW"), undefined);
  assert.equal(normalizeSmsPhone("1234567890123456"), undefined);
});
