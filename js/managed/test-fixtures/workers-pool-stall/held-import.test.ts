import { it } from "vitest";

// Synthetic pool stall for test/workers-pool-diagnostics-journey.test.mjs, run by
// vitest.pool-stall.config.ts. This test passes, but its import stays pending
// in the main process until the journey releases it. The runner therefore
// cannot post this file's "testfileFinished", whatever the timing.
it("returns while a module import is still pending", () => {
  void import("virtual:pool-stall-hold");
});
