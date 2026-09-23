import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [cloudflareTest({ miniflare: {
    compatibilityDate: "2026-07-29",
    compatibilityFlags: ["nodejs_compat"],
    durableObjects: { NANOCODEX_ACCOUNT_TOOLS: "AccountHostedTools" },
    outboundService: () => new Response("Unexpected test network request", { status: 502 }),
  }, main: "./test/sandbox-enrollment-worker.ts" })],
  test: { include: ["test/sandbox-desktop.test.ts", "test/sandbox-enrollment.test.ts"] },
});
