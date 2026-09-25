import { cloudflareTest, readD1Migrations } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

export default defineConfig(async () => ({
  plugins: [cloudflareTest({
    wrangler: { configPath: "./wrangler.crm.test.jsonc" },
    miniflare: { bindings: { CRM_MIGRATIONS: await readD1Migrations("./migrations") } },
  })],
  test: { include: ["test/crm*.test.ts"], exclude: ["test/crm-managed.test.ts", "test/crm-automation.test.ts"] },
}));
