import { cloudflareTest } from "@cloudflare/vitest-pool-workers";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [cloudflareTest({
    wrangler: { configPath: "./wrangler.test.jsonc" },
    miniflare: {
      bindings: { CREDENTIAL_ENCRYPTION_KEY: btoa("0123456789abcdef0123456789abcdef") },
      outboundService: async (request) => {
        const url = new URL(request.url);
        if (url.href === "https://auth.openai.com/oauth/token" && request.method === "POST") {
          const body = await request.json() as Record<string, unknown>;
          if (body.client_id !== "app_EMoamEEZ73f0CkXaXp7hrann" || body.refresh_token !== "synthetic-refresh"
            || body.grant_type !== "refresh_token") return Response.json({ error: "invalid" }, { status: 401 });
          const exp = Math.floor(Date.now() / 1000) + 3600;
          const payload = { exp, "https://api.openai.com/auth": { chatgpt_account_id: "synthetic-account", chatgpt_account_is_fedramp: false } };
          const access = `e30.${Buffer.from(JSON.stringify(payload)).toString("base64url")}.signature`;
          return Response.json({ access_token: access, refresh_token: "synthetic-rotated" });
        }
        if (url.href === "https://chatgpt.com/backend-api/codex/responses") {
          return Response.json({ routed: true, account: request.headers.get("chatgpt-account-id"),
            authorization: request.headers.get("authorization")?.startsWith("Bearer ") ? "bearer" : null });
        }
        return new Response("unexpected outbound", { status: 599 });
      },
    },
  })],
  test: { include: ["test/**/*.cf.test.ts"], testTimeout: 20_000 },
});
