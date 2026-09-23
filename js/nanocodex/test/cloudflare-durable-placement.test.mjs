import assert from "node:assert/strict";
import test from "node:test";
import { durablePlacementOptions, withIngressPlacement, TRUSTED_INGRESS_HEADER } from "nanocodex/cloudflare/durable-placement";

test("public placement API preserves regions and rejects unknown placement", async () => {
  assert.deepEqual(durablePlacementOptions("SJC"), { locationHint: "wnam" });
  assert.deepEqual(durablePlacementOptions("LHR"), { locationHint: "weur" });
  assert.equal(durablePlacementOptions("ZZZ"), undefined);
  assert.equal(durablePlacementOptions("sjc"), undefined);
  const requests = [];
  const original = { NANOCODEX: { fetch: async request => { requests.push(request); return new Response(); } } };
  for (const colo of ["SJC", null]) {
    const scoped = withIngressPlacement(original, colo);
    await scoped.NANOCODEX.fetch("https://broker.internal/users/fixture/wallet", { headers: { [TRUSTED_INGRESS_HEADER]: "NRT" } });
  }
  assert.deepEqual(requests.map(request => request.headers.get(TRUSTED_INGRESS_HEADER)), ["SJC", null]);
  assert.equal(original.trustedClientIngressColo, undefined);
});
