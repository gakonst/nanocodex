import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [cloudflareTest({ wrangler: { configPath: "./wrangler.test.jsonc" } })],
  test: {
    include: ["test/**/*.test.ts"],
    // Bundle cron-parser's CommonJS/Luxon boundary as Wrangler does in production.
    deps: { optimizer: { ssr: { enabled: true, include: ["cron-parser"] } } },
  },
});
