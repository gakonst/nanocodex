import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";
export default defineConfig({
  plugins: [cloudflareTest({ wrangler: { configPath: "./wrangler.jsonc" }, miniflare: { serviceBindings: { NANOCODEX_EMAIL_AGENT: async () => new Response("test") }, bindings: { EMAIL_SEND_ENABLED: "false", MAILBOX_OWNER_ID: "owner", MAILBOX_ADMIN_ID: "owner", MAILBOX_ADDRESS: "agent@example.com" } } })],
  test: { include: ["test/**/*.test.ts"] },
});
