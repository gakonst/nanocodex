import { randomBytes } from "node:crypto";
import { mkdir, rename, unlink, writeFile } from "node:fs/promises";
import { basename, dirname, extname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { nanocodexTools } from "../tools/vite.mjs";
import {
  generateFile as generateWebMcpManifest,
  validate as validateWebMcpManifest,
} from "../webmcp/generator.mjs";
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
  let webMcpResult;
  let development = false;

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
    resolveId(source, importer) {
      if (source === AUTOMATIC_WEBMCP_MODULE) return AUTOMATIC_WEBMCP_RESOLVED;
      if (source === AUTOMATIC_WEBMCP_CLIENT) return automaticWebMcpClient;
      return tools.resolveId(source, importer);
    },
    load(id) {
      if (id !== AUTOMATIC_WEBMCP_RESOLVED) return null;
      return [
        `import { prepareAutomaticWebMcp } from ${JSON.stringify(AUTOMATIC_WEBMCP_CLIENT)};`,
        "prepareAutomaticWebMcp().catch((error) => console.error('[nanocodex] automatic WebMCP generation failed', error));",
      ].join("\n");
    },
    transformIndexHtml() {
      if (!development || !automaticWebMcp(options)) return undefined;
      return [{
        tag: "script",
        attrs: { type: "module", src: `/@id/${AUTOMATIC_WEBMCP_MODULE}` },
        injectTo: "body",
      }];
    },
    configResolved(config) {
      viteRoot = resolve(config.root ?? process.cwd());
      viteLogger = config.logger;
    },
    async config(config, environment) {
      development = environment.command === "serve";
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
      configureAutomaticWebMcp(vite);
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
    if (options.webMcp === false) return;
    if (webMcpGeneration) {
      webMcpQueued = true;
      return webMcpGeneration;
    }
    const configured = generationOptions(options.webMcp);
    if (!configured || typeof configured !== "object" || Array.isArray(configured)) {
      throw new TypeError("Nanocodex Vite webMcp must be true, false, or an options object");
    }
    const root = resolve(viteRoot, configured.root ?? ".");
    webMcpGeneration = generateWebMcpManifest({
      ...configured,
      root,
    }).then((result) => {
      webMcpResult = result;
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
    if (options.webMcp === false || webMcpWatch) return;
    const configured = generationOptions(options.webMcp);
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
        void updateWebMcpManifest().then((result) => {
          if (automaticWebMcp(options) && result?.changed) {
            vite.ws?.send?.({ type: "full-reload" });
          }
        }).catch((error) => vite.config.logger.error(errorMessage(error)));
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

  function configureAutomaticWebMcp(vite) {
    if (!automaticWebMcp(options)) return;
    if (typeof vite.middlewares?.use !== "function") {
      throw new TypeError("Nanocodex automatic WebMCP requires Vite middleware support");
    }
    vite.middlewares.use(AUTOMATIC_WEBMCP_ENDPOINT, async (request, response) => {
      try {
        if (request.method === "GET") {
          const result = webMcpResult ?? await updateWebMcpManifest();
          const generated = result.manifest.generatedBy === "nanocodex-agent"
            ? result.manifest
            : undefined;
          return json(response, 200, {
            appId: developmentAppId(viteRoot),
            sourceRevision: result.manifest.sourceRevision,
            manifest: result.manifest,
            ...(generated === undefined ? {} : { generated }),
          });
        }
        if (request.method !== "POST") return json(response, 405, { error: "method not allowed" });
        const body = await readJson(request);
        const current = webMcpResult ?? await updateWebMcpManifest();
        if (body?.sourceRevision !== current.manifest.sourceRevision) {
          return json(response, 409, { error: "website source changed during WebMCP generation" });
        }
        validateWebMcpManifest(body.manifest);
        const manifest = verifiedAgentManifest(current.manifest, body.manifest);
        await writeManifest(current.path, manifest);
        webMcpResult = Object.freeze({ changed: true, manifest, path: current.path });
        vite.config.logger.info(
          `[nanocodex] verified ${relative(viteRoot, current.path)} with the authenticated browser Agent (${manifest.tools.length} tools)`,
        );
        return json(response, 200, manifest);
      } catch (error) {
        vite.config.logger.error(errorMessage(error));
        return json(response, 400, { error: errorMessage(error) });
      }
    });
  }
}

function generationOptions(webMcp) {
  if (webMcp === undefined || webMcp === true) return {};
  if (!webMcp || typeof webMcp !== "object" || Array.isArray(webMcp)) {
    throw new TypeError("Nanocodex Vite webMcp must be true, false, or an options object");
  }
  const { automatic: _automatic, ...generation } = webMcp;
  return generation;
}

function automaticWebMcp(options) {
  return options.webMcp !== false && options.webMcp?.automatic !== false;
}

function verifiedAgentManifest(draft, proposed) {
  if (proposed.sourceRevision !== draft.sourceRevision) {
    throw new Error("Nanocodex returned a manifest for the wrong source revision");
  }
  const candidates = new Map(draft.tools.map((tool) => [
    JSON.stringify(tool.implementation),
    tool,
  ]));
  const tools = proposed.tools.map((tool) => {
    const implementation = JSON.stringify(tool.implementation);
    const candidate = candidates.get(implementation);
    if (!candidate || JSON.stringify(tool.evidence) !== JSON.stringify(candidate.evidence)) {
      throw new Error(`Nanocodex changed source-owned execution evidence for ${tool.name}`);
    }
    candidates.delete(implementation);
    return Object.freeze({
      ...tool,
      annotations: candidate.annotations,
      // The automatic browser publisher cannot supply application-owned
      // handlers. Preserve custom candidates for review, but never expose an
      // invocation that would only fail at call time.
      approved: tool.approved === true && candidate.implementation.kind !== "custom",
    });
  });
  return Object.freeze({
    ...proposed,
    generatedAt: new Date().toISOString(),
    generatedBy: "nanocodex-agent",
    root: ".",
    sourceRevision: draft.sourceRevision,
    tools: Object.freeze(tools),
  });
}

async function writeManifest(path, manifest) {
  await mkdir(dirname(path), { recursive: true });
  const temporary = join(dirname(path), `.${basename(path)}.${process.pid}.${randomBytes(8).toString("hex")}.tmp`);
  try {
    await writeFile(temporary, `${JSON.stringify(manifest, null, 2)}\n`, { flag: "wx" });
    await rename(temporary, path);
  } finally {
    await unlink(temporary).catch((error) => {
      if (error?.code !== "ENOENT") throw error;
    });
  }
}

function developmentAppId(root) {
  const name = basename(root).replace(/[^A-Za-z0-9._:-]+/g, "-").replace(/^-+|-+$/g, "") || "website";
  return `webmcp-dev:${name}`.slice(0, 128);
}

async function readJson(request) {
  const chunks = [];
  let bytes = 0;
  for await (const chunk of request) {
    bytes += chunk.length;
    if (bytes > 2_000_000) throw new Error("automatic WebMCP result exceeds 2 MB");
    chunks.push(chunk);
  }
  try { return JSON.parse(Buffer.concat(chunks).toString("utf8")); }
  catch (error) { throw new Error("automatic WebMCP result is invalid JSON", { cause: error }); }
}

function json(response, status, value) {
  response.statusCode = status;
  response.setHeader("content-type", "application/json; charset=utf-8");
  response.setHeader("cache-control", "no-store");
  response.end(JSON.stringify(value));
}

const WEBMCP_SOURCE_EXTENSIONS = new Set([
  ".cjs", ".gql", ".graphql", ".htm", ".html", ".js", ".json",
  ".jsx", ".mjs", ".ts", ".tsx", ".yaml", ".yml",
]);
const AUTOMATIC_WEBMCP_MODULE = "virtual:nanocodex-webmcp";
const AUTOMATIC_WEBMCP_RESOLVED = `\0${AUTOMATIC_WEBMCP_MODULE}`;
const AUTOMATIC_WEBMCP_CLIENT = "virtual:nanocodex-webmcp-client";
const AUTOMATIC_WEBMCP_ENDPOINT = "/__nanocodex/webmcp";
const automaticWebMcpClient = fileURLToPath(new URL("./auto-webmcp-client.mjs", import.meta.url));

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
