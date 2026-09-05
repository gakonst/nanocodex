import assert from "node:assert/strict";
import test from "node:test";

import {
  hostedAppToolCatalog,
  hostedToolCatalogDigest,
} from "../tools/hostedCatalog.mjs";

const tool = {
  name: "cleanup",
  description: "Inspect one browser tab.",
  parameters: {
    type: "object",
    properties: { action: { type: "string" } },
    required: ["action"],
    additionalProperties: false,
  },
  handler() {},
};

test("app tool catalog digests are stable and cover the complete emitted contract", async () => {
  const catalog = hostedAppToolCatalog([tool]);
  assert.deepEqual(catalog.map(({ provider, remote_name }) => [provider, remote_name]), [
    ["javascript", "cleanup"],
  ]);
  const digest = await hostedToolCatalogDigest(catalog);
  assert.match(digest, /^0x[0-9a-f]{64}$/);
  assert.equal(await hostedToolCatalogDigest([{ ...catalog[0] }]), digest);
  assert.notEqual(await hostedToolCatalogDigest([{
    ...catalog[0],
    definition: { ...catalog[0].definition, description: "A broader tool." },
  }]), digest);
  assert.notEqual(await hostedToolCatalogDigest([{ ...catalog[0], timeout_ms: 1 }]), digest);
});
