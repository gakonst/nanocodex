import { gitProvider } from "../test-fixtures/git-provider.mjs";
import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [cloudflareTest({
    wrangler: { configPath: "./wrangler.test.jsonc" },
    miniflare: { outboundService: async (request) => {
      const response = await gitProvider(request);
      return response ?? new Response("Unexpected test network request", { status: 502 });
    } },
  })],
  test: {
    include: ["test/**/*.test.ts"],
    // Bundle cron-parser's CommonJS/Luxon boundary as Wrangler does in production.
    deps: { optimizer: { ssr: { enabled: true, include: ["cron-parser"] } } },
  },
});
