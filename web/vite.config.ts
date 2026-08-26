import { cloudflare } from "@cloudflare/vite-plugin";
import react from "@vitejs/plugin-react";
import { nanocodexTools } from "nanocodex/tools/vite";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import { rewriteDocsDevModuleUrl } from "./vite/docsDevModules.ts";
import { localManagedAuxiliaryWorkers } from "./vite/localWorkerTopology.ts";
import {
  documentStatusForPath,
  renderLinkPreviewDocument,
} from "./worker/linkPreview.ts";
import { isManagedRoutePath } from "./worker/managedProxy.ts";

const repositoryRoot = fileURLToPath(new URL("..", import.meta.url));
const connectDialogIndex = new URL("./connect-dialog/index.html", import.meta.url);
const connectDialogRoot = fileURLToPath(new URL("./connect-dialog", import.meta.url));
const connectPlaygroundIndex = new URL("./connect-playground/index.html", import.meta.url);
const connectPlaygroundRoot = fileURLToPath(new URL("./connect-playground", import.meta.url));
const repositoryRevision = execFileSync("git", ["rev-parse", "HEAD"], {
  cwd: repositoryRoot,
  encoding: "utf8",
}).trim();

function localConnectApplications(): Plugin {
  return {
    name: "nanocodex-local-connect-applications",
    enforce: "pre",
    apply: "serve" as const,
    configureServer(vite) {
      vite.middlewares.use(async (request, response, next) => {
        const method = request.method ?? "GET";
        const url = new URL(request.url ?? "/", "https://localhost");
        let hostname: string | undefined;
        try {
          hostname = request.headers.host
            ? new URL(`https://${request.headers.host}`).hostname
            : undefined;
        } catch {
          next();
          return;
        }

        const serveDocument = async (
          index: URL,
          transformPath: string,
          rewrite?: (html: string) => string,
        ) => {
          const source = await readFile(index, "utf8");
          const html = await vite.transformIndexHtml(transformPath, rewrite?.(source) ?? source);
          response.statusCode = 200;
          response.setHeader("cache-control", "no-store");
          response.setHeader("content-type", "text/html; charset=utf-8");
          response.end(method === "HEAD" ? undefined : html);
        };

        const playgroundHost = process.env.NANOCODEX_LOCAL_CONNECT_PLAYGROUND_HOST;
        if (playgroundHost && hostname === playgroundHost) {
          if (method !== "GET" && method !== "HEAD") {
            response.statusCode = 405;
            response.setHeader("allow", "GET, HEAD");
            response.setHeader("cache-control", "no-store");
            response.end();
            return;
          }
          if (url.pathname.startsWith("/src/")) {
            request.url = `/@fs${connectPlaygroundRoot}${url.pathname}${url.search}`;
            next();
            return;
          }
          if (url.pathname.startsWith("/connect-playground/src/")) {
            const sourcePath = url.pathname.slice("/connect-playground".length);
            request.url = `/@fs${connectPlaygroundRoot}${sourcePath}${url.search}`;
            next();
            return;
          }
          if (
            url.pathname.startsWith("/@")
            || url.pathname.startsWith("/node_modules/")
            || url.pathname.startsWith("/__vite")
          ) {
            next();
            return;
          }
          if (request.headers.accept?.includes("text/html")) {
            try {
              await serveDocument(connectPlaygroundIndex, `${url.pathname}${url.search}`);
            } catch (error) {
              next(error as Error);
            }
            return;
          }
          response.statusCode = 404;
          response.setHeader("cache-control", "no-store");
          response.end(method === "HEAD" ? undefined : "Not found");
          return;
        }

        if (url.pathname === "/connect-dialog" || url.pathname.startsWith("/connect-dialog/")) {
          if (method !== "GET" && method !== "HEAD") {
            next();
            return;
          }
          if (url.pathname.startsWith("/connect-dialog/src/")) {
            const sourcePath = url.pathname.slice("/connect-dialog".length);
            request.url = `/@fs${connectDialogRoot}${sourcePath}${url.search}`;
            next();
            return;
          }
          if (request.headers.accept?.includes("text/html")) {
            try {
              response.setHeader(
                "content-security-policy",
                "frame-ancestors 'self' http://nanocodex.localhost:* http://*.nanocodex.localhost:*",
              );
              await serveDocument(
                connectDialogIndex,
                `${url.pathname}${url.search}`,
                (html) => html.replace('src="/src/main.tsx"', 'src="/connect-dialog/src/main.tsx"'),
              );
            } catch (error) {
              next(error as Error);
            }
            return;
          }
        }
        next();
      });
    },
  };
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
        const acceptsHtml = request.headers.accept?.includes("text/html") ?? false;
        if ((request.method !== "GET" && request.method !== "HEAD") || !acceptsHtml) {
          next();
          return;
        }
        if (isManagedRoutePath(url.pathname)) {
          next();
          return;
        }
        const status = documentStatusForPath(url.pathname);
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
        const origin = process.env.NANOCODEX_LOCAL_PUBLIC_ORIGIN
          ?? context.server?.resolvedUrls?.local[0]
          ?? "http://localhost:5173";
        const url = new URL(context.path, origin);
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
    __NANOCODEX_DEPLOYMENT_SHA__: JSON.stringify(repositoryRevision),
  },
  plugins: [
    localConnectApplications(),
    applicationRouteFallback(),
    linkPreviewMetadata(),
    deploymentBuildAttestation(),
    nanocodexTools(),
    react(),
    cloudflare({
      auxiliaryWorkers: localManagedAuxiliaryWorkers(),
      persistState: process.env.NANOCODEX_LOCAL_STATE_PATH
        ? { path: process.env.NANOCODEX_LOCAL_STATE_PATH }
        : undefined,
      config: (config) => ({
        // `npm run dev` mints this one-use bootstrap credential after rejecting
        // local env files. Wrangler's required-secret loader cannot consume
        // process.env while env-file loading is disabled, so bind this exact
        // non-provider token explicitly to the local Worker.
        ...(process.env.CLOUDFLARE_ENV === "development"
          ? { secrets: undefined }
          : {}),
        vars: {
          ...config.vars,
          ...(process.env.CLOUDFLARE_ENV === "development"
            && process.env.GIT_MIRROR_TOKEN
            ? { GIT_MIRROR_TOKEN: process.env.GIT_MIRROR_TOKEN }
            : {}),
          ...(process.env.NANOCODEX_LOCAL_DEPLOYMENT_SHA
            ? { DEPLOYMENT_SHA: process.env.NANOCODEX_LOCAL_DEPLOYMENT_SHA }
            : {}),
        },
        dev: {
          ...config.dev,
          // Every local Worker asks the OS for an ephemeral inspector port.
          // The website, broker, and managed Worker can then start together
          // even when another checkout already has an inspector open.
          inspector_port: 0,
          // The website, Worker APIs, Durable Objects, D1, and Just Bash do
          // not need Docker. Container-backed experiments remain explicit.
          enable_containers: process.env.NANOCODEX_DEV_CONTAINERS === "1",
        },
      }),
    }),
  ],
  resolve: {
    dedupe: [
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
    ],
  },
  // Local SDK packages stay live during development. Vite's persistent
  // dependency cache must not hold an older Worker/React contract after a
  // package edit, and the WASM glue plus binary are indivisible.
  optimizeDeps: {
    exclude: ["nanocodex", "nanocodex-react"],
  },
  worker: {
    format: "es",
    // Vite creates a separate plugin graph for nested browser Workers. The
    // Nanocodex browser-tool adapter must therefore be installed in both the
    // page build above and this Worker build.
    plugins: () => [nanocodexTools()],
  },
  server: {
    strictPort: true,
    allowedHosts: [".nanocodex.localhost"],
    // The playground and application use sibling localhost hosts. Their API
    // requests carry the account session, so Vite's preflight must opt into
    // credentialed CORS before the request can reach the local Worker.
    cors: {
      origin: true,
      credentials: true,
    },
    // The live artifact frame intentionally has an opaque sandbox origin. Its
    // module graph therefore needs CORS even though it is served by this host.
    headers: { "Access-Control-Allow-Origin": "*" },
    fs: {
      allow: [repositoryRoot],
    },
  },
});
