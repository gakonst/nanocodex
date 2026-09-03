import { randomBytes } from "node:crypto";
import { constants } from "node:fs";
import { access, readFile, realpath, stat } from "node:fs/promises";
import { spawn } from "node:child_process";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { parseEnv } from "node:util";

import { nanocodexTools } from "./tools.mjs";
import { chatGptSubscription } from "./chatgpt-subscription.mjs";
import { defaultCodexAuthFile, readCodexSubscription } from "./codex-auth-file.mjs";
import { startChatGptWorkerEgress } from "./chatgpt-egress.mjs";
import { startLocalOAuthRelay } from "./oauth-relay-server.mjs";

const LOCAL_SPONSORED_CHATGPT_USER_ID = "00000000-0000-4000-8000-000000000001";

const buildScript = fileURLToPath(new URL("./scripts/build-js-package.sh", import.meta.url));
const repositoryRoot = fileURLToPath(new URL("../../", import.meta.url));
const rustBuildFiles = new Set([
  resolve(repositoryRoot, "Cargo.lock"),
  resolve(repositoryRoot, "Cargo.toml"),
  resolve(repositoryRoot, ".cargo/config.toml"),
  resolve(repositoryRoot, "js/nanocodex/Cargo.toml"),
]);
const rustBuildDirectories = [
  resolve(repositoryRoot, "crates"),
  resolve(repositoryRoot, "js/nanocodex/src"),
];
const rustBuildWatchRoots = [...rustBuildFiles, ...rustBuildDirectories];
const packageManifest = fileURLToPath(new URL("./package.json", import.meta.url));
const sourcePackageManifest = fileURLToPath(
  new URL("../../js/nanocodex-vite/package.json", import.meta.url),
);
const browserPackage = new URL(import.meta.resolve("nanocodex/browser"));
const generatedPackage = [
  new URL("../pkg-web/nanocodex.js", browserPackage),
  new URL(import.meta.resolve("nanocodex/wasm")),
];
const packageBuilds = new Map();

export function createNanocodexVitePlugin(options, integration) {
  const tools = nanocodexTools();
  const devApplications = normalizeDevApplications(options.devApplications);
  const chatGpt = options.chatGpt ?? {};
  const credentialBrokerWorker = integration.target === "cloudflare"
    && typeof chatGpt.credentialBrokerWorker === "string"
    ? chatGpt.credentialBrokerWorker
    : undefined;
  const direct = integration.target === "vite" && chatGpt !== false
    ? chatGptSubscription(chatGpt)
    : undefined;
  const buildJsPackage = integration.buildJsPackage ?? ensureJsPackage;
  const loadOAuthBindings = integration.loadOAuthBindings ?? localOAuthBindings;
  const startOAuthRelay = integration.startOAuthRelay ?? startLocalOAuthRelay;
  const rebuildJsPackage = integration.rebuildJsPackage ?? runJsPackageBuild;
  let buildPromise;
  let workerAuth;
  let egress;
  let oauthRelay;
  let devApplicationServers = [];
  let devApplicationCleanup;
  let cleanupPromise;

  const cleanup = () => cleanupPromise ??= (async () => {
    try {
      await egress?.close();
    } finally {
      egress = undefined;
      workerAuth = undefined;
      integration.setDevBindings?.(undefined);
    }
  })();

  const cleanupOAuthRelay = async () => {
    const active = oauthRelay;
    oauthRelay = undefined;
    await active?.close();
  };

  const cleanupDevApplications = () => devApplicationCleanup ??= (async () => {
    const active = devApplicationServers;
    devApplicationServers = [];
    await Promise.all(active.map(({ server }) => server.close()));
  })();

  return {
    name: "nanocodex",
    enforce: "pre",
    resolveId: tools.resolveId,
    async config(config, environment) {
      await (buildPromise ??= buildJsPackage(environment.command === "build"));
      if (options.oauthRelay === true && environment.command === "serve") {
        oauthRelay ??= await startOAuthRelay();
      } else {
        await cleanupOAuthRelay();
      }
      const nestedWorker = workerPlugins(config.worker?.plugins);
      if (
        integration.target !== "cloudflare"
        || environment.command !== "serve"
      ) {
        if (integration.target === "cloudflare") await cleanup();
        integration.setDevBindings?.(undefined);
        return nanocodexConfig(nestedWorker, false);
      }

      await cleanup();
      cleanupPromise = undefined;
      let oauthBindings = {};
      if (options.oauthRelay === true && credentialBrokerWorker) {
        try {
          oauthBindings = await loadOAuthBindings();
        } catch (error) {
          await cleanupOAuthRelay();
          throw new Error(`Nanocodex local OAuth setup failed: ${errorMessage(error)}.`);
        }
      }
      if (chatGpt === false) {
        integration.setDevBindings(Object.freeze(oauthBindings));
        return nanocodexConfig(nestedWorker, false);
      }
      try {
        const configuredAuthFile = chatGpt.authFile === undefined
          ? defaultCodexAuthFile()
          : chatGpt.authFile;
        const authFile = configuredAuthFile instanceof URL
          ? fileURLToPath(configuredAuthFile)
          : configuredAuthFile;
        workerAuth = await readCodexSubscription(authFile);
        egress = await startChatGptWorkerEgress();
        if (credentialBrokerWorker) {
          integration.setDevBindings(Object.freeze({
            ...oauthBindings,
            ALLOW_INSECURE_LOOPBACK_RELAY: "true",
            CODEX_RELAY_URL: egress.relayUrl,
            NANOCODEX_LOCAL_SPONSORED_TRIAL_RESET: "true",
            NANOCODEX_SPONSORED_CHATGPT_USER_ID: LOCAL_SPONSORED_CHATGPT_USER_ID,
            LOCAL_CHATGPT_BOOTSTRAP: JSON.stringify({
              access_token: workerAuth.accessToken,
              account_id: workerAuth.accountId,
              expires_at: workerAuth.expiresAt,
              fedramp: workerAuth.fedramp,
            }),
          }));
        } else {
          integration.setDevBindings(Object.freeze({
            ...oauthBindings,
            ENVIRONMENT: "development",
            NANOCODEX_DEV_CHATGPT_ACCESS_TOKEN: workerAuth.accessToken,
            NANOCODEX_DEV_CHATGPT_ACCOUNT_ID: workerAuth.accountId,
            NANOCODEX_DEV_CHATGPT_FEDRAMP: String(workerAuth.fedramp),
            NANOCODEX_DEV_CHATGPT_EXPIRES_AT: String(workerAuth.expiresAt),
            NANOCODEX_DEV_CHATGPT_EGRESS_URL: egress.url,
            NANOCODEX_DEV_CHATGPT_SESSION_ID: randomBytes(32).toString("base64url"),
          }));
        }
      } catch (error) {
        await Promise.all([cleanup(), cleanupOAuthRelay()]);
        throw new Error(
          `Nanocodex local ChatGPT setup failed: ${errorMessage(error)}. Run \`codex login\` and retry.`,
        );
      }
      return nanocodexConfig(nestedWorker, Boolean(credentialBrokerWorker));
    },
    async configureServer(vite) {
      vite.httpServer?.once("close", () => {
        void Promise.all([cleanup(), cleanupOAuthRelay(), cleanupDevApplications()]);
      });
      if (devApplications.length > 0) {
        const createViteServer = integration.createViteServer
          ?? (await import("vite")).createServer;
        try {
          devApplicationServers = await createDevApplicationServers(
            vite,
            devApplications,
            createViteServer,
          );
        } catch (error) {
          await cleanupDevApplications();
          throw error;
        }
        vite.middlewares.use(devApplicationMiddleware(devApplicationServers));
      }
      if (await isSourceCheckout()) {
        watchRustBuildInputs(vite, rebuildJsPackage);
      }
      if (integration.target === "vite") {
        await direct?.configureServer(vite);
        return;
      }
      if (!workerAuth) return;
      vite.config.logger.info(
        `[nanocodex] local ChatGPT subscription ready through ${credentialBrokerWorker ?? "the application Worker"} (expires ${new Date(workerAuth.expiresAt).toISOString()})`,
      );
    },
    async closeBundle() {
      await Promise.all([cleanup(), cleanupOAuthRelay(), cleanupDevApplications()]);
    },
  };
}

function nanocodexConfig(nestedWorker, localSponsoredTrialReset) {
  return {
    define: {
      __NANOCODEX_LOCAL_SPONSORED_TRIAL_RESET__: JSON.stringify(localSponsoredTrialReset),
    },
    worker: { plugins: nestedWorker },
  };
}

function normalizeDevApplications(configured) {
  if (configured === undefined) return [];
  if (!Array.isArray(configured)) throw new TypeError("Nanocodex devApplications must be an array");
  const paths = new Set();
  const applications = configured.map((application, index) => {
    if (application === null || typeof application !== "object") {
      throw new TypeError(`Nanocodex devApplications[${index}] must be an object`);
    }
    const path = normalizeDevApplicationPath(application.path, index);
    if (paths.has(path)) throw new TypeError(`Nanocodex devApplications contains duplicate path ${path}`);
    paths.add(path);
    const root = application.root;
    if (typeof root !== "string" && !(root instanceof URL)) {
      throw new TypeError(`Nanocodex devApplications[${index}].root must be a path or file URL`);
    }
    if (typeof root === "string" && root.length === 0) {
      throw new TypeError(`Nanocodex devApplications[${index}].root must not be empty`);
    }
    if (root instanceof URL && root.protocol !== "file:") {
      throw new TypeError(`Nanocodex devApplications[${index}].root must be a file URL`);
    }
    const headers = normalizeDevApplicationHeaders(application.headers, index);
    return Object.freeze({ path, root, headers });
  });
  return Object.freeze(applications.sort((left, right) => right.path.length - left.path.length));
}

function normalizeDevApplicationHeaders(configured, index) {
  if (configured === undefined) return Object.freeze({});
  if (configured === null || typeof configured !== "object" || Array.isArray(configured)) {
    throw new TypeError(`Nanocodex devApplications[${index}].headers must be an object`);
  }
  try {
    return Object.freeze(Object.fromEntries(new Headers(configured)));
  } catch (error) {
    throw new TypeError(
      `Nanocodex devApplications[${index}].headers contains an invalid header: ${errorMessage(error)}`,
    );
  }
}

function normalizeDevApplicationPath(configured, index) {
  if (typeof configured !== "string") {
    throw new TypeError(`Nanocodex devApplications[${index}].path must be a URL path`);
  }
  const path = configured.endsWith("/") ? configured.slice(0, -1) : configured;
  if (path === "" || path === "/" || !path.startsWith("/") || path.includes("?") || path.includes("#")) {
    throw new TypeError(`Nanocodex devApplications[${index}].path must be a non-root URL path`);
  }
  for (const encoded of path.slice(1).split("/")) {
    let segment;
    try {
      segment = decodeURIComponent(encoded);
    } catch {
      throw new TypeError(`Nanocodex devApplications[${index}].path must have valid URL encoding`);
    }
    if (segment === "" || segment === "." || segment === ".." || segment.includes("/") || segment.includes("\\")) {
      throw new TypeError(`Nanocodex devApplications[${index}].path must have safe URL segments`);
    }
  }
  return path;
}

async function createDevApplicationServers(vite, applications, createViteServer) {
  const mounted = [];
  try {
    for (const application of applications) {
      const root = application.root instanceof URL
        ? fileURLToPath(application.root)
        : resolve(vite.config.root, application.root);
      const server = await createViteServer({
        root,
        base: `${application.path}/`,
        appType: "spa",
        clearScreen: false,
        server: {
          middlewareMode: true,
          hmr: vite.httpServer ? { server: vite.httpServer } : false,
        },
      });
      mounted.push({ ...application, server });
    }
    return mounted;
  } catch (error) {
    await Promise.all(mounted.map(({ server }) => server.close()));
    throw error;
  }
}

function devApplicationMiddleware(applications) {
  return (request, response, next) => {
    const method = request.method ?? "GET";
    if (method !== "GET" && method !== "HEAD") return next();
    let pathname;
    try {
      pathname = new URL(request.url ?? "/", "http://nanocodex.invalid").pathname;
    } catch {
      return next();
    }
    const application = applications.find(({ path }) => pathname === path || pathname.startsWith(`${path}/`));
    if (!application) return next();
    for (const [name, value] of Object.entries(application.headers)) {
      response.setHeader(name, value);
    }

    const originalUrl = request.url;
    let restored = false;
    const restore = () => {
      if (restored) return;
      restored = true;
      request.url = originalUrl;
      response.off("finish", restore);
      response.off("close", restore);
    };
    response.once("finish", restore);
    response.once("close", restore);
    application.server.middlewares(request, response, (error) => {
      restore();
      next(error);
    });
  };
}

async function localOAuthBindings() {
  const environment = { ...await mainCheckoutEnvironment(), ...process.env };
  return oauthBindingsFromEnvironment(environment);
}

export function oauthBindingsFromEnvironment(environment) {
  return {
    ...oauthCredentialPair(environment, {
      label: "GitHub",
      ids: ["NANOCODEX_GITHUB_OAUTH_CLIENT_ID", "GITHUB_OAUTH_CLIENT_ID", "GH_CLIENT_ID"],
      secrets: ["NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET", "GITHUB_OAUTH_CLIENT_SECRET", "GH_CLIENT_SECRETS"],
      targetId: "GITHUB_OAUTH_CLIENT_ID",
      targetSecret: "GITHUB_OAUTH_CLIENT_SECRET",
    }),
    ...oauthCredentialPair(environment, {
      label: "Google",
      ids: ["NANOCODEX_GOOGLE_OAUTH_CLIENT_ID", "GOOGLE_OAUTH_CLIENT_ID", "GOOGLE_CLIENT_ID"],
      secrets: ["NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET", "GOOGLE_OAUTH_CLIENT_SECRET", "GOOGLE_CLIENT_SECRET"],
      targetId: "GOOGLE_OAUTH_CLIENT_ID",
      targetSecret: "GOOGLE_OAUTH_CLIENT_SECRET",
    }),
    ...oauthCredentialPair(environment, {
      label: "Slack",
      ids: ["NANOCODEX_SLACK_OAUTH_CLIENT_ID", "SLACK_OAUTH_CLIENT_ID", "SLACK_CLIENT_ID"],
      secrets: ["NANOCODEX_SLACK_OAUTH_CLIENT_SECRET", "SLACK_OAUTH_CLIENT_SECRET", "SLACK_CLIENT_SECRET"],
      targetId: "SLACK_OAUTH_CLIENT_ID",
      targetSecret: "SLACK_OAUTH_CLIENT_SECRET",
    }),
    ...oauthCredentialPair(environment, {
      label: "X",
      ids: ["NANOCODEX_X_OAUTH_CLIENT_ID", "X_OAUTH_CLIENT_ID", "X_CLIENT_ID"],
      secrets: ["NANOCODEX_X_OAUTH_CLIENT_SECRET", "X_OAUTH_CLIENT_SECRET", "X_CLIENT_SECRET"],
      targetId: "X_OAUTH_CLIENT_ID",
      targetSecret: "X_OAUTH_CLIENT_SECRET",
    }),
  };
}

async function mainCheckoutEnvironment() {
  try {
    const metadataPath = resolve(repositoryRoot, ".git");
    const metadata = await stat(metadataPath);
    let environmentPath = resolve(repositoryRoot, ".env");
    if (metadata.isFile()) {
      const pointer = (await readFile(metadataPath, "utf8")).trim();
      const match = /^gitdir:\s*(.+)$/.exec(pointer);
      if (!match) throw new Error(`${metadataPath} does not identify a Git directory`);
      const gitDirectory = resolve(dirname(metadataPath), match[1]);
      const commonDirectory = resolve(
        gitDirectory,
        (await readFile(resolve(gitDirectory, "commondir"), "utf8")).trim(),
      );
      environmentPath = resolve(dirname(commonDirectory), ".env");
    }
    return parseEnv(await readFile(environmentPath, "utf8"));
  } catch (error) {
    if (error?.code === "ENOENT") return {};
    throw error;
  }
}

function oauthCredentialPair(environment, names) {
  const id = firstEnvironmentValue(environment, names.ids);
  const secret = firstEnvironmentValue(environment, names.secrets);
  if (Boolean(id) !== Boolean(secret)) {
    throw new Error(`local ${names.label} OAuth client ID and secret must be configured together`);
  }
  return id && secret
    ? { [names.targetId]: id, [names.targetSecret]: secret }
    : {};
}

function firstEnvironmentValue(environment, names) {
  for (const name of names) {
    const value = environment[name]?.trim();
    if (value) return value;
  }
  return undefined;
}

async function ensureJsPackage(release = false) {
  const mode = release ? "release" : "development";
  if (!packageBuilds.has(mode)) packageBuilds.set(mode, (async () => {
    if (process.env.CI && await generatedPackageIsPresent()) return;
    if (!await isSourceCheckout()) {
      await Promise.all(generatedPackage.map((artifact) => access(artifact, constants.R_OK)));
      return;
    }
    await runJsPackageBuild(release);
  })());
  return packageBuilds.get(mode);
}

async function runJsPackageBuild(release = false) {
  await access(buildScript, constants.X_OK);
  await new Promise((resolve, reject) => {
    const child = spawn(buildScript, release ? ["--release"] : [], {
      cwd: repositoryRoot,
      stdio: "inherit",
    });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0) resolve();
      else reject(new Error(
        `Nanocodex WASM generation failed${signal ? ` (${signal})` : ` with exit code ${code}`}`,
      ));
    });
  });
}

function watchRustBuildInputs(vite, rebuild) {
  vite.watcher.add(rustBuildWatchRoots);
  let debounce;
  let queued = false;
  let running;

  const run = () => {
    queued = true;
    running ??= (async () => {
      while (queued) {
        queued = false;
        vite.config.logger.info("[nanocodex] Rust/WASM input changed; rebuilding bindings...");
        await rebuild();
        vite.ws.send({ type: "full-reload" });
        vite.config.logger.info("[nanocodex] Rust/WASM bindings rebuilt");
      }
    })().catch((error) => {
      vite.config.logger.error(`[nanocodex] Rust/WASM rebuild failed: ${errorMessage(error)}`);
    }).finally(() => {
      running = undefined;
      if (queued) run();
    });
  };
  const changed = (path) => {
    if (!isRustBuildInput(path)) return;
    clearTimeout(debounce);
    debounce = setTimeout(run, 75);
  };
  const close = () => {
    clearTimeout(debounce);
    for (const event of ["add", "change", "unlink"]) vite.watcher.off(event, changed);
  };
  for (const event of ["add", "change", "unlink"]) vite.watcher.on(event, changed);
  vite.httpServer?.once("close", close);
}

function isRustBuildInput(path) {
  if (rustBuildFiles.has(path)) return true;
  return rustBuildDirectories.some((directory) => {
    const nested = relative(directory, path);
    return nested !== "" && !nested.startsWith(`..${sep}`) && nested !== ".." && !isAbsolute(nested);
  });
}

async function generatedPackageIsPresent() {
  try {
    await Promise.all(generatedPackage.map((artifact) => access(artifact, constants.R_OK)));
    return true;
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
}

async function isSourceCheckout() {
  try {
    const [loaded, source] = await Promise.all([
      realpath(packageManifest),
      realpath(sourcePackageManifest),
    ]);
    return loaded === source;
  } catch (error) {
    if (error?.code === "ENOENT") return false;
    throw error;
  }
}

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
