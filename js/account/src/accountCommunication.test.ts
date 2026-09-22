import assert from "node:assert/strict";
import test from "node:test";
import { decodeAccountCommunication } from "./agentContactDetails.ts";

test("accepts assigned contacts and explicitly unassigned accounts", () => {
  assert.deepEqual(decodeAccountCommunication({ email: "agent@example.com", phone: "+14155550123" }), { email: "agent@example.com", phone: "+14155550123" });
  assert.deepEqual(decodeAccountCommunication({ email: null, phone: null }), { email: null, phone: null });
});
test("does not interpret missing or malformed contacts as unassigned", () => {
  for (const value of [{}, null, { email: "bad", phone: null }, { email: null, phone: "123" }]) assert.throws(() => decodeAccountCommunication(value));
});
