import assert from "node:assert/strict";
import { test } from "node:test";
import { mergeAccountHands, restoredAccountHands } from "../src/account-hands.mjs";

const phone = { id: "ios-phone", name: "iPhone", workspace: "/ios-phone", capabilities: ["native", "filesystem", "background_limited"] };

test("account Hands remain visible offline and only fresh presence reconnects them", () => {
  const online = mergeAccountHands([], [{ ...phone, route_token: "private", status: "offline" }]);
  assert.deepEqual(online, [{ ...phone, status: "connected" }]);
  const offline = mergeAccountHands(online, []);
  assert.deepEqual(offline, [{ ...phone, status: "offline" }]);
  assert.deepEqual(restoredAccountHands(online), offline);
  assert.deepEqual(mergeAccountHands(offline, [phone]), online);
  assert.throws(() => mergeAccountHands(online, [phone, phone]), /duplicate/);
  assert.throws(() => mergeAccountHands(online, [{ ...phone, capabilities: [null] }]), /invalid/);
});
