import assert from "node:assert/strict";
import { test } from "node:test";
import { requireSameOriginMutation } from "../src/same-origin-mutation.ts";

test("runtime-neutral origin guard preserves session rejection and API-key semantics", async () => {
  const url = new URL("https://account.example/v1/credentials");
  const session = { kind: "account_session" };
  assert.equal(requireSameOriginMutation(new Request(url, { headers: { origin: url.origin } }), url, session), undefined);
  for (const headers of [{}, { origin: "https://other.example" }]) {
    const response = requireSameOriginMutation(new Request(url, { headers }), url, session);
    assert.equal(response.status, 403);
    assert.equal(response.headers.get("cache-control"), "no-store");
    assert.equal(response.headers.get("x-content-type-options"), "nosniff");
    assert.deepEqual(await response.json(), { error: "forbidden_origin" });
  }
  assert.equal(requireSameOriginMutation(new Request(url), url, { kind: "api_key" }), undefined);
});
