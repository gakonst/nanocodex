import { gitProvider } from "../test-fixtures/git-provider.mjs";
import { cloudflareTest, readD1Migrations } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

export default defineConfig(async () => ({
  plugins: [cloudflareTest({
    wrangler: { configPath: "./wrangler.test.jsonc" },
    miniflare: { bindings: { CRM_MIGRATIONS: await readD1Migrations("./migrations") }, outboundService: async (request) => {
      const response = await gitProvider(request);
      return response ?? new Response("Unexpected test network request", { status: 502 });
    } },
  })],
  test: {
    include: ["test/**/*.test.ts"],
    exclude: ["test/jev-reliability.test.ts", "test/router-telemetry.test.ts", "test/provider-probe-schedule.test.ts", "test/provider-probe-slots.test.ts", "test/provider-telemetry-routing.test.ts", "test/thread-model-routing.test.ts"],
    // Bundle cron-parser's CommonJS/Luxon boundary as Wrangler does in production.
    deps: { optimizer: { ssr: { enabled: true, include: ["cron-parser"] } } },
  },
}));
