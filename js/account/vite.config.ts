import react from "@vitejs/plugin-react";
import { nanocodex } from "nanocodex-vite/cloudflare";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import { isLocalDocumentRequest } from "./scripts/local-document-request.mjs";
import { rewriteDocsDevModuleUrl } from "./vite/docsDevModules.ts";
import {
  documentStatusForPath,
  renderLinkPreviewDocument,
} from "./worker/linkPreview.ts";
import { isManagedRoutePath } from "./worker/managedProxy.ts";
import { isConnectApiBrowserRoutePath } from "./worker/connectApiProxy.ts";

const repositoryRoot = fileURLToPath(new URL("../..", import.meta.url));
const repositoryRevision = execFileSync("git", ["rev-parse", "HEAD"], {
  cwd: repositoryRoot,
  encoding: "utf8",
}).trim();
const localPublicOrigin = process.env.PORTLESS_URL
  ?? process.env.NANOCODEX_LOCAL_PUBLIC_ORIGIN
  ?? "http://nanocodex.localhost:5173";
const localServerPort = process.env.PORT ? Number(process.env.PORT) : undefined;

if (localServerPort !== undefined
  && (!Number.isSafeInteger(localServerPort) || localServerPort < 1 || localServerPort > 65_535)) {
  throw new Error("PORT must be a valid TCP port");
}

function applicationRouteFallback(): Plugin {
  return {
    name: "nanocodex-application-route-fallback",
    enforce: "pre",
    apply: "serve" as const,
    configureServer(vite) {
      vite.middlewares.use(async (request, response, next) => {
        const docsModuleUrl = rewriteDocsDevModuleUrl(request.url);
        if (docsModuleUrl != null && (request.method === "GET" || request.method === "HEAD")) {
          request.url = docsModuleUrl;
          next();
          return;
        }
        const url = new URL(request.url ?? "/", "https://localhost");
        if (isManagedRoutePath(url.pathname) || isConnectApiBrowserRoutePath(url.pathname)) {
          next();
          return;
        }
        const status = documentStatusForPath(url.pathname);
        if (!isLocalDocumentRequest(request, status != null)) {
          next();
          return;
        }
        if (status == null) {
          response.statusCode = 404;
          response.setHeader("cache-control", "no-store");
          response.setHeader("content-type", "text/plain; charset=utf-8");
          response.end(request.method === "HEAD" ? undefined : "Not found");
          return;
        }
        try {
          const template = await readFile(new URL("./index.html", import.meta.url), "utf8");
          const html = await vite.transformIndexHtml(`${url.pathname}${url.search}`, template);
          response.statusCode = status;
          response.setHeader("cache-control", "no-store");
          response.setHeader("content-type", "text/html; charset=utf-8");
          response.end(request.method === "HEAD" ? undefined : html);
        } catch (error) {
          next(error as Error);
        }
      });
    },
  };
}

function linkPreviewMetadata(): Plugin {
  return {
    name: "nanocodex-link-preview-metadata",
    apply: "serve" as const,
    transformIndexHtml: {
      order: "post",
      handler(html, context) {
        const url = new URL(context.path, localPublicOrigin);
        return renderLinkPreviewDocument(html, url);
      },
    },
  };
}

function deploymentBuildAttestation(): Plugin {
  return {
    name: "nanocodex-deployment-build-attestation",
    apply: "build" as const,
    async closeBundle() {
      const config = await readFile(new URL("./wrangler.jsonc", import.meta.url));
      await writeFile(
        new URL("./dist/nanocodex/build-attestation.json", import.meta.url),
        `${JSON.stringify({
          revision: repositoryRevision,
          wranglerConfigSha256: createHash("sha256").update(config).digest("hex"),
        })}\n`,
      );
    },
  };
}

export default defineConfig({
  // Some browser dependencies feature-detect `process` but assume that a
  // detected shim also contains `env`. The browser has no environment access;
  // make that empty boundary explicit instead of letting a partial shim crash.
  define: {
    "process.env": "{}",
  },
  plugins: [
    nanocodex({
      chatGpt: { credentialBrokerWorker: "nanocodex-egress" },
      devApplications: [{
        headers: {
          "content-security-policy": "frame-ancestors 'self' https://nanocodex.localhost https://*.nanocodex.localhost http://nanocodex.localhost:* http://*.nanocodex.localhost:*",
        },
        path: "/connect-dialog",
        root: new URL("../connect-dialog", import.meta.url),
      }],
      oauthRelay: true,
      cloudflare: {
        inspectorPort: 0,
        auxiliaryWorkers: [
          { configPath: "../egress/wrangler.broker.jsonc", devOnly: true },
          { configPath: "../managed/wrangler.jsonc", devOnly: true },
          { configPath: "../chief-of-staff/wrangler.jsonc", devOnly: true },
          { configPath: "../connect-api/wrangler.jsonc", devOnly: true },
        ],
      },
    }),
    applicationRouteFallback(),
    linkPreviewMetadata(),
    deploymentBuildAttestation(),
    react(),
  ],
  resolve: {
    dedupe: [
      "accounts",
      "react",
      "react-dom",
      "nanocodex",
      "nanocodex-react",
      "@pierre/theme",
      "@shikijs/core",
      "@shikijs/engine-javascript",
      "@shikijs/langs",
      "@shikijs/primitive",
      "@shikijs/types",
      "@tanstack/react-virtual",
      "shiki",
      "streamdown",
      "viem",
    ],
  },
  // Local SDK packages stay live during development. Vite's persistent
  // dependency cache must not hold an older Worker/React contract after a
  // package edit, and the WASM glue plus binary are indivisible.
  optimizeDeps: {
    exclude: ["nanocodex", "nanocodex-connect-ui", "nanocodex-react"],
  },
  worker: {
    format: "es",
  },
  server: {
    allowedHosts: [".nanocodex.localhost"],
    host: process.env.HOST,
    origin: process.env.PORTLESS_URL,
    port: localServerPort,
    // The Connect playground calls the paired application origin with its
    // account cookie, while the live artifact frame has an opaque `null`
    // origin. Reflect only the local Nanocodex development authorities and
    // explicitly permit credentials; a wildcard silently blocks the real
    // connection POST after a successful preflight.
    cors: {
      credentials: true,
      origin: [
        /^https?:\/\/(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)?nanocodex\.localhost(?::\d+)?$/,
        /^https?:\/\/(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)?playground\.nanocodex\.localhost(?::\d+)?$/,
        "null",
      ],
    },
    fs: {
      allow: [repositoryRoot],
    },
  },
});
