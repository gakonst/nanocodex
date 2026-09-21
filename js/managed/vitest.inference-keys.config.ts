import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [cloudflareTest({ wrangler: { configPath: "./wrangler.inference-keys.test.jsonc" } })],
  test: { include: ["test/inference-keys.test.ts"] },
});
