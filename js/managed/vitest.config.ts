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
    setupFiles: ["./test/durable-object-settle.ts"],
    // managed-image-journey needs vitest.images.config.ts bindings (npm run test:images).
    exclude: ["test/managed-image-journey.test.ts", "test/user-data-*.test.ts", "test/prompt-apps.test.ts", "test/jev-reliability.test.ts", "test/router-telemetry.test.ts", "test/provider-probe-schedule.test.ts", "test/provider-probe-slots.test.ts", "test/provider-telemetry-routing.test.ts", "test/thread-model-routing.test.ts", "test/account-hosted-tools.test.ts", "test/hosted-tools-broker.test.ts", "test/hosted-tools-protocol.test.ts"],
    // Bundle payment dependencies as Wrangler does; lazy loading otherwise pays
    // thousands of Vite/Workers module transforms inside the first tool call.
    deps: { optimizer: { ssr: {
      enabled: true,
      include: ["cron-parser", "mppx", "mppx/client", "mppx/mcp/client"],
      // Ox's optional native pool dynamically imports this on Node only.
      rolldownOptions: { external: [/^node:/] },
    } } },
  },
}));
