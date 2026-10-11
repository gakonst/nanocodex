// Runs only the synthetic pool-stall fixture, without the settle hook, for
// test/workers-pool-diagnostics-journey.test.mjs. Not part of "vitest run".
//
// The fixture imports virtual:pool-stall-hold and does not await it. This
// config's load hook keeps that import pending in the main process until the
// file named by NANOCODEX_POOL_STALL_RELEASE exists. Vitest's runner awaits its
// pending RPCs before posting "testfileFinished", so the fixture's file passes
// and its runner then stays silent until the journey releases the import. The
// result is the #951 symptom, without depending on timing. This is synthetic
// fault injection to exercise the diagnostic; it is not the root cause of any
// CI hang. The hold is bounded: after NANOCODEX_POOL_STALL_HOLD_MS (default 120s)
// the import fails and the run ends, even if nothing releases it. The journey
// treats an exit before its release as a failure.
import { existsSync } from "node:fs";
import { defineConfig, type UserConfig } from "vitest/config";
import base from "./vitest.config.ts";

const HOLD = "virtual:pool-stall-hold";
const holdUntilReleased = {
  name: "nanocodex:pool-stall-hold",
  resolveId(id: string) {
    return id === HOLD ? "\0" + HOLD : undefined;
  },
  async load(id: string) {
    if (id !== "\0" + HOLD) return undefined;
    const release = process.env.NANOCODEX_POOL_STALL_RELEASE;
    if (!release) throw new Error("NANOCODEX_POOL_STALL_RELEASE must name the file whose creation releases " + HOLD);
    const limit = Number(process.env.NANOCODEX_POOL_STALL_HOLD_MS) || 120_000;
    const deadline = Date.now() + limit;
    while (!existsSync(release)) {
      if (Date.now() > deadline) throw new Error(HOLD + " was not released within " + limit + "ms");
      await new Promise(resolve => setTimeout(resolve, 100));
    }
    return "export const released = true;";
  },
};

export default defineConfig(async env => {
  const config = await (base as (env: unknown) => Promise<UserConfig>)(env);
  return {
    ...config,
    plugins: [...(config.plugins ?? []), holdUntilReleased],
    test: { ...config.test, include: ["test-fixtures/workers-pool-stall/*.test.ts"], setupFiles: [] },
  };
});
