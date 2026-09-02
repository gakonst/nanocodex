import assert from "node:assert/strict";
import test from "node:test";

import { connectAuthOrigin, deviceVerificationUrl } from "../src/deviceRedirect.mts";

test("production device verification opens the first-class main-site route", () => {
  assert.equal(
    deviceVerificationUrl(
      "https://nanocodex-connect-api.gakonst.workers.dev",
      "ABCDWXYZ",
    ).href,
    "https://nanocodex.gakonst.workers.dev/connect?api_origin=https%3A%2F%2Fnanocodex-connect-api.gakonst.workers.dev&user_code=ABCDWXYZ",
  );
});

test("local device verification stays on the exact local API origin", () => {
  assert.equal(
    deviceVerificationUrl("http://review.nanocodex.localhost:20735", "ABCDWXYZ").href,
    "http://review.nanocodex.localhost:20735/connect?api_origin=http%3A%2F%2Freview.nanocodex.localhost%3A20735&user_code=ABCDWXYZ",
  );
  assert.equal(
    deviceVerificationUrl("http://localhost:5190", "ABCDWXYZ").href,
    "http://localhost:5190/connect?api_origin=http%3A%2F%2Flocalhost%3A5190&user_code=ABCDWXYZ",
  );
  assert.throws(() => deviceVerificationUrl("http://127.0.0.1:8787/path", "ABCDWXYZ"), /not allowed/);
});

test("Connect authentication binds production and local ceremonies to exact trusted origins", () => {
  assert.equal(
    connectAuthOrigin("https://nanocodex-connect-api.gakonst.workers.dev"),
    "https://nanocodex-connect-api.gakonst.workers.dev",
  );
  assert.equal(
    connectAuthOrigin("http://review.nanocodex.localhost:20735"),
    "http://review.nanocodex.localhost:20735",
  );
  assert.equal(connectAuthOrigin("http://localhost:5190"), "http://localhost:5190");
  assert.throws(() => connectAuthOrigin("https://connect.attacker.example"), /not allowed/);
  assert.throws(() => connectAuthOrigin("http://127.0.0.1:8787/path"), /not allowed/);
});
