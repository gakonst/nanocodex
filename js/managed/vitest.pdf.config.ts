import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [cloudflareTest({ miniflare: {
    compatibilityDate: "2026-07-29",
    compatibilityFlags: ["nodejs_compat"],
    outboundService: () => new Response("PDF extraction must not use the network", { status: 502 }),
  } })],
  test: { include: ["test/pdf-runtime.test.ts"] },
});
