import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import type { NextConfig } from "next";
import { withWorkflow } from "workflow/next";

const exampleRoot = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = process.env.VERCEL ? exampleRoot : resolve(exampleRoot, "../..");

const securityHeaders = [
  { key: "Cache-Control", value: "no-store" },
  { key: "Cross-Origin-Opener-Policy", value: "same-origin" },
  { key: "Permissions-Policy", value: "camera=(), microphone=(), geolocation=()" },
  { key: "Referrer-Policy", value: "no-referrer" },
  { key: "X-Content-Type-Options", value: "nosniff" },
  { key: "X-Frame-Options", value: "DENY" },
];

const nextConfig: NextConfig = {
  outputFileTracingIncludes: {
    "/*": ["./workflows/nanocodex.wasm"],
  },
  serverExternalPackages: ["@vercel/functions", "nanocodex", "pg", "ws"],
  turbopack: { root: repositoryRoot },
  async headers() {
    return [{ source: "/(.*)", headers: securityHeaders }];
  },
};

export default withWorkflow(nextConfig);
