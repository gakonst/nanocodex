import assert from "node:assert/strict";
import test from "node:test";

import { RequestError, authorized, parseFetchPrompt } from "../src/request.ts";

test("fetch prompt parsing is bounded and typed", () => {
  assert.deepEqual(parseFetchPrompt('{"prompt":" hello "}'), {
    prompt: "hello",
    thinking: "high",
  });
  assert.throws(
    () => parseFetchPrompt('{"prompt":"hello","thinking":"wild"}'),
    (error) => error instanceof RequestError && error.status === 400,
  );
});

test("fetch endpoint requires the exact bearer token", () => {
  assert.equal(authorized("Bearer secret", "secret"), true);
  assert.equal(authorized("Bearer wrong", "secret"), false);
  assert.equal(authorized(null, "secret"), false);
});
