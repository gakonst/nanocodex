import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [cloudflareTest({ wrangler: { configPath: "./wrangler.projects.test.jsonc" } })],
  test: { include: ["test/conversation-projects.test.ts", "test/project-threads.test.ts"] },
});
