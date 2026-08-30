import { randomBytes } from "node:crypto";
import { extname, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { nanocodexTools } from "../tools/vite.mjs";
import { generateFile as generateWebMcpManifest } from "../webmcp/generator.mjs";
import { chatGptSubscription } from "./chatgpt-subscription.mjs";
import { defaultCodexAuthFile, readCodexSubscription } from "./codex-auth-file.mjs";
import { startChatGptWorkerEgress } from "./chatgpt-egress.mjs";

export function createNanocodexVitePlugin(options, integration) {
  const tools = nanocodexTools();
  const chatGpt = options.chatGpt ?? {};
  const direct = integration.target === "vite" && chatGpt !== false
    ? chatGptSubscription(chatGpt)
    : undefined;
  let workerAuth;
  let egress;
  let cleanupPromise;
  let viteRoot = process.cwd();
  let viteLogger;
  let webMcpTimer;
  let webMcpWatch;
  let webMcpGeneration;
  let webMcpQueued = false;

  const cleanup = () => cleanupPromise ??= (async () => {
    if (webMcpTimer) clearTimeout(webMcpTimer);
    webMcpTimer = undefined;
    webMcpWatch?.();
    webMcpWatch = undefined;
    try {
      await egress?.close();
    } finally {
      egress = undefined;
      workerAuth = undefined;
      integration.setDevBindings?.(undefined);
    }
  })();

  return {
    name: "nanocodex",
    enforce: "pre",
    resolveId: tools.resolveId,
    configResolved(config) {
      viteRoot = resolve(config.root ?? process.cwd());
      viteLogger = config.logger;
    },
    async config(config, environment) {
      const nestedWorker = workerPlugins(config.worker?.plugins);
      if (
        integration.target !== "cloudflare"
        || environment.command !== "serve"
        || chatGpt === false
      ) {
        if (integration.target === "cloudflare") await cleanup();
        integration.setDevBindings?.(undefined);
        return { worker: { plugins: nestedWorker } };
      }

      await cleanup();
      cleanupPromise = undefined;
      try {
        const configuredAuthFile = chatGpt.authFile === undefined
          ? defaultCodexAuthFile()
          : chatGpt.authFile;
        const authFile = configuredAuthFile instanceof URL
          ? fileURLToPath(configuredAuthFile)
          : configuredAuthFile;
        workerAuth = await readCodexSubscription(authFile);
        egress = await startChatGptWorkerEgress();
        integration.setDevBindings(Object.freeze({
          ENVIRONMENT: "development",
          NANOCODEX_DEV_CHATGPT_ACCESS_TOKEN: workerAuth.accessToken,
          NANOCODEX_DEV_CHATGPT_ACCOUNT_ID: workerAuth.accountId,
          NANOCODEX_DEV_CHATGPT_FEDRAMP: String(workerAuth.fedramp),
          NANOCODEX_DEV_CHATGPT_EXPIRES_AT: String(workerAuth.expiresAt),
          NANOCODEX_DEV_CHATGPT_EGRESS_URL: egress.url,
          NANOCODEX_DEV_CHATGPT_SESSION_ID: randomBytes(32).toString("base64url"),
        }));
      } catch (error) {
        await cleanup();
        throw new Error(
          `Nanocodex local ChatGPT setup failed: ${errorMessage(error)}. Run \`codex login\` and retry.`,
        );
      }
      return { worker: { plugins: nestedWorker } };
    },
    async configureServer(vite) {
      await updateWebMcpManifest();
      watchWebMcp(vite);
      if (integration.target === "vite") {
        await direct?.configureServer(vite);
        return;
      }
      if (!workerAuth) return;
      vite.config.logger.info(
        `[nanocodex] local ChatGPT subscription ready through the application Worker (expires ${new Date(workerAuth.expiresAt).toISOString()})`,
      );
      vite.httpServer?.once("close", () => { void cleanup(); });
    },
    async buildStart() {
      await updateWebMcpManifest();
    },
    async closeBundle() {
      await cleanup();
    },
  };

  async function updateWebMcpManifest() {
    if (options.webMcp === undefined || options.webMcp === false) return;
    if (webMcpGeneration) {
      webMcpQueued = true;
      return webMcpGeneration;
    }
    const configured = options.webMcp === true ? {} : options.webMcp;
    if (!configured || typeof configured !== "object" || Array.isArray(configured)) {
      throw new TypeError("Nanocodex Vite webMcp must be true, false, or an options object");
    }
    const root = resolve(viteRoot, configured.root ?? ".");
    webMcpGeneration = generateWebMcpManifest({
      ...configured,
      root,
    }).then((result) => {
      if (result.changed) {
        viteLogger?.info?.(
          `[nanocodex] generated ${relative(viteRoot, result.path)} (${result.manifest.tools.length} review-required tools)`,
        );
      }
      return result;
    }).finally(async () => {
      webMcpGeneration = undefined;
      if (webMcpQueued) {
        webMcpQueued = false;
        await updateWebMcpManifest();
      }
    });
    return webMcpGeneration;
  }

  function watchWebMcp(vite) {
    if (options.webMcp === undefined || options.webMcp === false || webMcpWatch) return;
    const configured = options.webMcp === true ? {} : options.webMcp;
    const root = resolve(viteRoot, configured.root ?? ".");
    const output = resolve(root, configured.output ?? "webmcp.manifest.json");
    const changed = (path) => {
      const absolute = resolve(path);
      const local = relative(root, absolute);
      if (absolute === output || local.startsWith(`..${sep}`) || local === ".."
          || !WEBMCP_SOURCE_EXTENSIONS.has(extname(absolute).toLowerCase())) return;
      if (webMcpTimer) clearTimeout(webMcpTimer);
      webMcpTimer = setTimeout(() => {
        webMcpTimer = undefined;
        void updateWebMcpManifest().catch((error) => vite.config.logger.error(errorMessage(error)));
      }, 50);
    };
    vite.watcher.on("add", changed);
    vite.watcher.on("change", changed);
    vite.watcher.on("unlink", changed);
    webMcpWatch = () => {
      vite.watcher.off("add", changed);
      vite.watcher.off("change", changed);
      vite.watcher.off("unlink", changed);
    };
  }
}

const WEBMCP_SOURCE_EXTENSIONS = new Set([
  ".cjs", ".gql", ".graphql", ".htm", ".html", ".js", ".json",
  ".jsx", ".mjs", ".ts", ".tsx", ".yaml", ".yml",
]);

function workerPlugins(existing) {
  return () => {
    const configured = typeof existing === "function" ? existing() : [];
    const plugins = (configured ?? []).flat(Infinity).filter(Boolean);
    return plugins.some((plugin) => plugin?.name === "nanocodex-tools")
      ? plugins
      : [nanocodexTools(), ...plugins];
  };
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}
