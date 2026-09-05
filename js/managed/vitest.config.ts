import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

const liveBrowser = process.env.NANOCODEX_LIVE_BROWSER === "true";

export default defineConfig({
  plugins: [cloudflareTest({ remoteBindings: liveBrowser, wrangler: { configPath: "./wrangler.test.jsonc" } })],
  test: {
    include: liveBrowser ? ["test/browser-control.live.test.ts"] : ["test/**/*.test.ts"],
    exclude: liveBrowser ? [] : ["test/**/*.live.test.ts"],
    // Bundle cron-parser's CommonJS/Luxon boundary as Wrangler does in production.
    deps: { optimizer: { ssr: { enabled: true, include: ["cron-parser"] } } },
  },
});
