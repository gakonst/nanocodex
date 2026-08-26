import { spawn } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { chmod, mkdir, open, readFile, readdir, stat, unlink } from "node:fs/promises";
import { request as httpRequest } from "node:http";
import { connect as connectNet } from "node:net";
import { homedir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { Readable } from "node:stream";
import { fileURLToPath } from "node:url";

import { readCodexSubscription } from "../../services/managed/scripts/codex-auth-file.mjs";

const scriptPath = fileURLToPath(import.meta.url);
const webRoot = resolve(dirname(scriptPath), "..");
const repositoryRoot = resolve(webRoot, "..");
const reactRoot = resolve(repositoryRoot, "js/react");
const terminalRoot = resolve(repositoryRoot, "js/terminal");
const managedRoot = resolve(repositoryRoot, "services/managed");
const connectDialogRoot = resolve(webRoot, "connect-dialog");
const connectPlaygroundRoot = resolve(webRoot, "connect-playground");
const connectApiRoot = resolve(webRoot, "connect-api");
const localGatewayComposePath = resolve(webRoot, "docker-compose.dev.yml");
const LOCAL_DEVELOPMENT_PUBLIC_ORIGIN = "https://nanocodex.local";
const runtimeEnvironmentNames = [
  "CI",
  "COLORTERM",
  "COMSPEC",
  "FORCE_COLOR",
  "HOME",
  "LANG",
  "LANGUAGE",
  "LC_ALL",
  "LC_CTYPE",
  "LOGNAME",
  "NO_COLOR",
  "PATH",
  "PATHEXT",
  "SHELL",
  "SystemRoot",
  "TEMP",
  "TERM",
  "TMP",
  "TMPDIR",
  "TZ",
  "USER",
  "WINDIR",
];
const buildEnvironmentNames = [
  "AR",
  "CARGO_HOME",
  "CARGO_TARGET_DIR",
  "CC",
  "CFLAGS",
  "CXX",
  "CXXFLAGS",
  "DEVELOPER_DIR",
  "LDFLAGS",
  "MACOSX_DEPLOYMENT_TARGET",
  "PKG_CONFIG_PATH",
  "RUSTC_WRAPPER",
  "RUSTDOCFLAGS",
  "RUSTFLAGS",
  "RUSTUP_HOME",
  "RUSTUP_TOOLCHAIN",
  "SCCACHE_CACHE_SIZE",
  "SCCACHE_DIR",
  "SDKROOT",
];
const websiteEnvironmentNames = [
  "CLOUDFLARE_ENV",
  "CLOUDFLARE_INCLUDE_PROCESS_ENV",
  "CLOUDFLARE_LOAD_DEV_VARS_FROM_DOT_ENV",
  "GIT_MIRROR_TOKEN",
  "NANOCODEX_DEV_CONTAINERS",
  "NANOCODEX_LOCAL_DEPLOYMENT_SHA",
  "NANOCODEX_LOCAL_CODEX_RELAY_URL",
];
const managedEnvironmentNames = [
  "AGENT_IDLE_TIMEOUT_MS",
  "CODEX_HOME",
  "NANOCODEX_CODEX_RELAY_URL",
  "NANOCODEX_CODEX_AUTH_FILE",
  "NANOCODEX_ADMIN_TOKEN",
  "NANOCODEX_AUTH_MODE",
  "NANOCODEX_BROKER_PORT",
  "NANOCODEX_ROOM_ALLOCATOR_TOKEN",
  "NANOCODEX_WORKER_PORT",
  "OPENAI_API_KEY",
];
const publisherEnvironmentNames = [
  "NANOCODEX_COMMIT_LIMIT",
  "NANOCODEX_FORCE_SYNC",
  "NANOCODEX_GIT_UPLOAD_TIMEOUT_MS",
  "NANOCODEX_REPAIR_INVALID_PUBLICATION",
  "NANOCODEX_REPO",
];
let rootEnvironmentLoaded = false;

export function providerFreeWebEnvironment(environment, overrides = {}) {
  return {
    ...selectedEnvironment(environment, runtimeEnvironmentNames),
    ...selectedEnvironment(environment, websiteEnvironmentNames),
    ...definedEnvironment(overrides),
  };
}

export function managedChildEnvironment(environment) {
  return {
    ...selectedEnvironment(environment, runtimeEnvironmentNames),
    ...selectedEnvironment(environment, managedEnvironmentNames),
  };
}

export function localConnectorEnvironment(environment) {
  return definedEnvironment({
    NANOCODEX_LOCAL_GITHUB_OAUTH_CLIENT_ID:
      environment.NANOCODEX_GITHUB_OAUTH_CLIENT_ID
      ?? completeLegacyCredential(environment.GH_CLIENT_ID, environment.GH_CLIENT_SECRETS),
    NANOCODEX_LOCAL_GITHUB_OAUTH_CLIENT_SECRET:
      environment.NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET
      ?? completeLegacyCredential(environment.GH_CLIENT_SECRETS, environment.GH_CLIENT_ID),
    NANOCODEX_LOCAL_GOOGLE_OAUTH_CLIENT_ID:
      environment.NANOCODEX_GOOGLE_OAUTH_CLIENT_ID
      ?? completeLegacyCredential(environment.GOOGLE_CLIENT_ID, environment.GOOGLE_CLIENT_SECRET),
    NANOCODEX_LOCAL_GOOGLE_OAUTH_CLIENT_SECRET:
      environment.NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET
      ?? completeLegacyCredential(environment.GOOGLE_CLIENT_SECRET, environment.GOOGLE_CLIENT_ID),
    NANOCODEX_LOCAL_X_OAUTH_CLIENT_ID:
      environment.NANOCODEX_X_OAUTH_CLIENT_ID
      ?? completeLegacyCredential(environment.X_CLIENT_ID, environment.X_CLIENT_SECRET),
    NANOCODEX_LOCAL_X_OAUTH_CLIENT_SECRET:
      environment.NANOCODEX_X_OAUTH_CLIENT_SECRET
      ?? completeLegacyCredential(environment.X_CLIENT_SECRET, environment.X_CLIENT_ID),
  });
}

function completeLegacyCredential(value, pair) {
  return value?.trim() && pair?.trim() ? value.trim() : undefined;
}

export async function mainWorktreeEnvironmentPath(repositoryPath = repositoryRoot) {
  const gitMetadataPath = resolve(repositoryPath, ".git");
  const metadata = await stat(gitMetadataPath);
  if (metadata.isDirectory()) return resolve(repositoryPath, ".env");
  if (!metadata.isFile()) throw new Error(`${gitMetadataPath} is not Git metadata`);

  const pointer = (await readFile(gitMetadataPath, "utf8")).trim();
  const match = /^gitdir:\s*(.+)$/.exec(pointer);
  if (!match) throw new Error(`${gitMetadataPath} does not identify a Git directory`);
  const gitDirectory = resolve(dirname(gitMetadataPath), match[1]);
  const commonDirectory = resolve(
    gitDirectory,
    (await readFile(resolve(gitDirectory, "commondir"), "utf8")).trim(),
  );
  return resolve(dirname(commonDirectory), ".env");
}

export async function loadRootEnvironment(path, repositoryPath = repositoryRoot) {
  if (rootEnvironmentLoaded) throw new Error("the root environment was already loaded");
  rootEnvironmentLoaded = true;
  const environmentPath = path ?? await mainWorktreeEnvironmentPath(repositoryPath);
  try {
    process.loadEnvFile(environmentPath);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
}

export function parseLocalDevOptions(arguments_, environment = process.env) {
  const modeArguments = arguments_.filter((argument) => argument.startsWith("--auth-mode="));
  if (modeArguments.length > 1) throw new Error("--auth-mode may be provided only once");
  const modeArgument = modeArguments[0];
  const unknown = arguments_.filter((argument) =>
    argument !== "--without-multiplayer" && !argument.startsWith("--auth-mode=")
  );
  if (unknown.length > 0) throw new Error(`unknown local development option: ${unknown[0]}`);
  const withoutMultiplayer = arguments_.includes("--without-multiplayer");
  const requestedMode = withoutMultiplayer
    ? ""
    : modeArgument?.slice("--auth-mode=".length)
      ?? environment.NANOCODEX_AUTH_MODE?.trim()
      ?? "";
  if (requestedMode && requestedMode !== "api_key" && requestedMode !== "chatgpt") {
    throw new Error("--auth-mode must be api_key or chatgpt");
  }
  if (withoutMultiplayer && modeArgument) {
    throw new Error("--without-multiplayer cannot be combined with --auth-mode");
  }
  return { requestedMode: requestedMode || undefined, withoutMultiplayer };
}

export function localDevelopmentOrigin(raw = "http://127.0.0.1:5173") {
  const origin = new URL(raw);
  if (
    origin.protocol !== "http:" ||
    (origin.hostname !== "127.0.0.1" && origin.hostname !== "localhost") ||
    origin.username ||
    origin.password ||
    origin.pathname !== "/" ||
    origin.search ||
    origin.hash ||
    !origin.port
  ) {
    throw new Error("NANOCODEX_DEV_ORIGIN must be an explicit loopback HTTP origin");
  }
  return origin;
}

export function localDevelopmentInstance(
  repositoryPath,
  { primary = false, requestedName } = {},
) {
  const requested = requestedName?.trim();
  if (requested && !/^[A-Za-z0-9][A-Za-z0-9._-]{0,47}$/.test(requested)) {
    throw new Error("NANOCODEX_DEV_INSTANCE must contain only letters, digits, dots, underscores, or hyphens");
  }
  const canonical = primary && !requested;
  const base = sanitizeLocalDevelopmentSlug(requested || basename(repositoryPath));
  const pathSuffix = createHash("sha256").update(resolve(repositoryPath)).digest("hex").slice(0, 6);
  const id = canonical ? "main" : requested ? base : `${base.slice(0, 24)}-${pathSuffix}`;
  const publicHost = canonical ? "nanocodex.local" : `${id}.nanocodex.local`;
  const playgroundHost = canonical
    ? "playground.nanocodex.local"
    : `playground-${id}.nanocodex.local`;
  const port = canonical
    ? 5_173
    : 20_000 + createHash("sha256").update(id).digest().readUInt16BE(0) % 30_000;
  return Object.freeze({
    composeProject: canonical ? "nanocodex-dev" : `nanocodex-dev-${id}`,
    defaultOrigin: `http://127.0.0.1:${port}`,
    id,
    playgroundOrigin: `https://${playgroundHost}`,
    primary: canonical,
    publicOrigin: `https://${publicHost}`,
  });
}

function sanitizeLocalDevelopmentSlug(value) {
  const slug = value.toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 32);
  return slug || "local";
}

async function resolveLocalDevelopmentInstance(environment) {
  let primary = false;
  try {
    primary = (await stat(resolve(repositoryRoot, ".git"))).isDirectory();
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
  return localDevelopmentInstance(repositoryRoot, {
    primary,
    requestedName: environment.NANOCODEX_DEV_INSTANCE,
  });
}

export function localDevelopmentPublicOrigin(raw = LOCAL_DEVELOPMENT_PUBLIC_ORIGIN) {
  const origin = new URL(raw);
  if (
    origin.protocol !== "https:"
    || (origin.hostname !== "nanocodex.local" && !origin.hostname.endsWith(".nanocodex.local"))
    || origin.port
    || origin.username
    || origin.password
    || origin.pathname !== "/"
    || origin.search
    || origin.hash
  ) {
    throw new Error("the public local development origin must be HTTPS under nanocodex.local");
  }
  return origin;
}

export function localDevelopmentStatePath(userHome = homedir(), instanceId = "main") {
  const base = resolve(userHome, ".nanocodex", "web-development");
  if (instanceId === "main") return base;
  if (!/^[a-z0-9][a-z0-9-]{0,47}$/.test(instanceId)) {
    throw new Error("invalid local development instance ID");
  }
  return resolve(base, "instances", instanceId);
}

export function localDevelopmentBrowserOrigin(origin = localDevelopmentOrigin(), instanceId = "main") {
  const host = instanceId === "main" ? "nanocodex.localhost" : `${instanceId}.nanocodex.localhost`;
  return new URL(`http://${host}:${origin.port}`);
}

export function localDevelopmentBrowserPlaygroundOrigin(
  origin = localDevelopmentOrigin(),
  instanceId = "main",
) {
  const host = instanceId === "main"
    ? "playground.nanocodex.localhost"
    : `playground-${instanceId}.nanocodex.localhost`;
  return new URL(`http://${host}:${origin.port}`);
}

export async function assertLocalDevelopmentPortAvailable(hostname, rawPort) {
  const { server: { port } } = viteChildConfiguration(hostname, rawPort);
  const occupied = await Promise.all([
    loopbackPortIsListening("127.0.0.1", port),
    loopbackPortIsListening("::1", port),
  ]);
  if (occupied.some(Boolean)) {
    throw new Error(
      `local development port ${port} is already in use; set NANOCODEX_DEV_ORIGIN to a free explicit loopback origin`,
    );
  }
}

function loopbackPortIsListening(host, port) {
  return new Promise((resolveProbe, rejectProbe) => {
    const socket = connectNet({ host, port });
    let settled = false;
    const finish = (result, error) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      if (error) rejectProbe(error);
      else resolveProbe(result);
    };
    socket.setTimeout(250, () => finish(true));
    socket.once("connect", () => finish(true));
    socket.once("error", (error) => {
      if (["ECONNREFUSED", "EADDRNOTAVAIL", "EAFNOSUPPORT", "ENETUNREACH"]
        .includes(error?.code)) {
        finish(false);
      } else {
        finish(false, error);
      }
    });
  });
}

async function main() {
  requireLocalProcessGroups();
  const lifecycle = new LocalStackLifecycle();
  let developmentLease;
  let gatewayLaunch;
  lifecycle.installSignalHandlers();

  try {
    await loadRootEnvironment();
    const environment = process.env;
    if (process.argv.slice(2).length > 0) {
      throw new Error("local development has one production-shaped account and managed-agent topology");
    }
    const instance = await resolveLocalDevelopmentInstance(environment);
    const origin = localDevelopmentOrigin(environment.NANOCODEX_DEV_ORIGIN ?? instance.defaultOrigin);
    const publicOrigin = localDevelopmentPublicOrigin(instance.publicOrigin);
    const browserOrigin = localDevelopmentBrowserOrigin(origin, instance.id);
    const browserPlaygroundOrigin = localDevelopmentBrowserPlaygroundOrigin(origin, instance.id);
    const statePath = localDevelopmentStatePath(homedir(), instance.id);
    developmentLease = await acquireLocalDevelopmentLease(statePath);
    await assertLocalDevelopmentPortAvailable(origin.hostname, origin.port);
    const toolEnvironment = buildChildEnvironment(environment);
    await assertOrbStack(toolEnvironment, (...arguments_) =>
      lifecycle.run(...arguments_, "OrbStack preflight"));
    await lifecycle.run(
      process.execPath,
      [resolve(webRoot, "scripts/check-dev-wasm.mjs")],
      { cwd: webRoot, env: toolEnvironment },
      "development WASM preflight",
    );
    await rejectWorkerEnvironmentFiles();
    const head = await gitHead(
      environment.NANOCODEX_REPO ?? repositoryRoot,
      toolEnvironment,
      (...arguments_) => lifecycle.run(...arguments_, "local Git HEAD inspection"),
    );
    const mirrorToken = randomBytes(32).toString("base64url");
    const adminToken = randomBytes(32).toString("base64url");
    const localChatGptBootstrap = await readLocalChatGptBootstrap(environment);

    await ensureLocalDependencies(
      toolEnvironment,
      (...arguments_) => lifecycle.run(...arguments_, "local dependency installation"),
    );
    await lifecycle.run(
      "npm",
      ["run", "build", "--prefix", terminalRoot],
      { cwd: repositoryRoot, env: toolEnvironment },
      "terminal package build",
    );
    await Promise.all([
      lifecycle.run(
        "npm",
        ["run", "build", "--prefix", connectDialogRoot],
        { cwd: repositoryRoot, env: toolEnvironment },
        "Connect dialog build",
      ),
      lifecycle.run(
        "npm",
        ["run", "build", "--prefix", connectPlaygroundRoot],
        {
          cwd: repositoryRoot,
          env: {
            ...toolEnvironment,
            VITE_CONNECT_API_HOST: publicOrigin.origin,
            VITE_CONNECT_DIALOG_HOST: new URL("/connect-dialog/", publicOrigin).href,
          },
        },
        "Connect playground build",
      ),
      lifecycle.run(
        "npm",
        ["run", "build", "--prefix", connectApiRoot],
        { cwd: repositoryRoot, env: toolEnvironment },
        "Connect API build",
      ),
    ]);

    await lifecycle.run(process.execPath, [
      resolve(webRoot, "node_modules/wrangler/bin/wrangler.js"),
      "d1",
      "migrations",
      "apply",
      "EVALS_DB",
      "--local",
      "--env",
      "development",
      "--persist-to",
      statePath,
    ], { cwd: webRoot, env: { ...toolEnvironment, CI: "true" } }, "local D1 migration");

    const relayLaunch = localChatGptRelayChildLaunch(toolEnvironment);
    const relay = lifecycle.spawn(
      relayLaunch.command,
      relayLaunch.arguments,
      relayLaunch.options,
      "local ChatGPT transport relay",
    );
    const relayChild = relay.child;
    const relayUrl = await waitForLocalChatGptRelay(relayChild);

    const websiteLaunch = websiteChildLaunch(environment, origin, {
      CLOUDFLARE_ENV: "development",
      CLOUDFLARE_INCLUDE_PROCESS_ENV: "false",
      CLOUDFLARE_LOAD_DEV_VARS_FROM_DOT_ENV: "false",
      GIT_MIRROR_TOKEN: mirrorToken,
      NANOCODEX_LOCAL_ADMIN_TOKEN: adminToken,
      NANOCODEX_LOCAL_AGENT_IDLE_TIMEOUT_MS: environment.AGENT_IDLE_TIMEOUT_MS ?? "1000",
      NANOCODEX_LOCAL_DEPLOYMENT_SHA: head,
      NANOCODEX_LOCAL_CHATGPT_BOOTSTRAP: localChatGptBootstrap,
      NANOCODEX_LOCAL_CODEX_RELAY_URL: relayUrl,
      NANOCODEX_LOCAL_PUBLIC_ORIGIN: publicOrigin.origin,
      NANOCODEX_LOCAL_CONNECT_PLAYGROUND_HOST: new URL(instance.playgroundOrigin).hostname,
      NANOCODEX_LOCAL_STATE_PATH: statePath,
      ...localConnectorEnvironment(environment),
    });
    const webEnvironment = websiteLaunch.options.env;
    const website = lifecycle.spawn(
      websiteLaunch.command,
      websiteLaunch.arguments,
      websiteLaunch.options,
      "web multi-Worker stack",
    );

    await waitForHttp(
      new URL("/api/health", origin),
      [relayChild, website.child],
      (response) => verifyLocalHealthResponse(response),
    );

    gatewayLaunch = orbStackGatewayChildLaunch(
      toolEnvironment,
      origin,
      publicOrigin,
      new URL(instance.playgroundOrigin),
      instance.composeProject,
    );
    const gateway = lifecycle.spawn(
      gatewayLaunch.command,
      gatewayLaunch.arguments,
      gatewayLaunch.options,
      "OrbStack HTTPS gateway",
    );
    await waitForOrbStackGateway(
      new URL("/api/health", publicOrigin),
      [relayChild, website.child, gateway.child],
      toolEnvironment,
    );

    await lifecycle.run(process.execPath, [resolve(webRoot, "scripts/publish-repository.mjs")], {
      cwd: webRoot,
      env: {
        ...webEnvironment,
        ...selectedEnvironment(environment, publisherEnvironmentNames),
        NANOCODEX_GIT_ORIGIN: origin.origin,
        NANOCODEX_GIT_TOKEN: mirrorToken,
        NANOCODEX_REPO: environment.NANOCODEX_REPO ?? repositoryRoot,
      },
    }, "local repository publisher");
    await verifyLocalState(origin, head, {
      environment: toolEnvironment,
      verifyGit: (verifyOrigin, verifyHead, verifyEnvironment) =>
        verifyLocalGitAdvertisement(
          verifyOrigin,
          verifyHead,
          verifyEnvironment,
          (...arguments_) => lifecycle.run(...arguments_, "local Git readiness inspection"),
        ),
    });
    process.stderr.write(
      `Nanocodex local Workers are ready at ${publicOrigin.origin} (${instance.id}; ${head.slice(0, 7)}; `
      + "repository published; evals migrated; managed agents ready).\n"
      + `Connect playground: ${instance.playgroundOrigin}\n`
      + `Portable browser verification: ${browserOrigin.origin}\n`
      + `Portable Connect playground: ${browserPlaygroundOrigin.origin}\n`,
    );

    const exited = await Promise.race([relay.exit, website.exit, gateway.exit]);
    if (!lifecycle.signal && exited.code !== 0) {
      throw new Error(`${exited.name} exited with ${exited.code ?? exited.signal}`);
    }
    process.exitCode = exited.code ?? signalExitCode(exited.signal ?? lifecycle.signal);
  } catch (error) {
    if (!lifecycle.signal) throw error;
    process.exitCode = signalExitCode(lifecycle.signal);
  } finally {
    try {
      await lifecycle.stop();
    } finally {
      try {
        if (gatewayLaunch) await stopOrbStackGateway(gatewayLaunch);
      } finally {
        try {
          await developmentLease?.release();
        } finally {
          lifecycle.removeSignalHandlers();
        }
      }
    }
  }
}

export async function acquireLocalDevelopmentLease(
  statePath,
  { currentPid = process.pid, isProcessAlive = localProcessIsAlive } = {},
) {
  await mkdir(statePath, { recursive: true, mode: 0o700 });
  await chmod(statePath, 0o700);
  const path = resolve(statePath, "development.lock");
  const token = randomBytes(32).toString("base64url");
  for (let attempt = 0; attempt < 2; attempt += 1) {
    let handle;
    try {
      handle = await open(path, "wx", 0o600);
      await handle.writeFile(`${JSON.stringify({ pid: currentPid, token })}\n`);
      await handle.close();
      return Object.freeze({
        async release() {
          const retained = await readLocalDevelopmentLease(path);
          if (retained?.token === token) await unlink(path).catch((error) => {
            if (error?.code !== "ENOENT") throw error;
          });
        },
      });
    } catch (error) {
      const owned = Boolean(handle);
      await handle?.close().catch(() => {});
      if (error?.code !== "EEXIST") {
        if (owned) await unlink(path).catch(() => {});
        throw error;
      }
      const retained = await readLocalDevelopmentLease(path);
      if (Number.isSafeInteger(retained?.pid) && isProcessAlive(retained.pid)) {
        throw new Error(
          `Nanocodex local development is already running as process ${retained.pid}; one stable HTTPS origin owns one local stack`,
        );
      }
      await unlink(path).catch((unlinkError) => {
        if (unlinkError?.code !== "ENOENT") throw unlinkError;
      });
    }
  }
  throw new Error("could not acquire the Nanocodex local development lease");
}

async function readLocalDevelopmentLease(path) {
  try {
    const value = JSON.parse(await readFile(path, "utf8"));
    return value && typeof value === "object" && !Array.isArray(value) ? value : undefined;
  } catch (error) {
    if (error?.code === "ENOENT" || error instanceof SyntaxError) return undefined;
    throw error;
  }
}

function localProcessIsAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if (error?.code === "ESRCH") return false;
    if (error?.code === "EPERM") return true;
    throw error;
  }
}

export async function assertOrbStack(environment, execute = run) {
  const [status, context] = await Promise.all([
    execute("orb", ["status"], { capture: true, env: environment }),
    execute("docker", ["context", "show"], { capture: true, env: environment }),
  ]);
  if (status.trim() !== "Running" || context.trim() !== "orbstack") {
    throw new Error(
      "Nanocodex local development requires a running OrbStack Docker context for trusted nanocodex.local HTTPS",
    );
  }
}

export function orbStackGatewayChildLaunch(
  environment,
  origin,
  publicOrigin = localDevelopmentPublicOrigin(),
  playgroundOrigin = localDevelopmentPublicOrigin("https://playground.nanocodex.local"),
  composeProject = "nanocodex-dev",
) {
  if (!/^[a-z0-9][a-z0-9_-]{0,62}$/.test(composeProject)) {
    throw new Error("invalid local development Compose project");
  }
  return {
    command: "docker",
    arguments: [
      "compose",
      "--project-name",
      composeProject,
      "--file",
      localGatewayComposePath,
      "up",
      "--force-recreate",
      "--remove-orphans",
      "--no-color",
    ],
    options: {
      cwd: webRoot,
      env: {
        ...selectedEnvironment(environment, runtimeEnvironmentNames),
        NANOCODEX_DEV_HOST: publicOrigin.hostname,
        NANOCODEX_PLAYGROUND_HOST: playgroundOrigin.hostname,
        NANOCODEX_DEV_PORT: origin.port,
      },
      stdio: ["ignore", "inherit", "inherit"],
    },
  };
}

export function orbStackGatewayStop(gatewayLaunch) {
  return {
    command: gatewayLaunch.command,
    arguments: [
      "compose",
      "--project-name",
      gatewayLaunch.arguments[gatewayLaunch.arguments.indexOf("--project-name") + 1],
      "--file",
      localGatewayComposePath,
      "down",
      "--remove-orphans",
    ],
    options: {
      cwd: gatewayLaunch.options.cwd,
      env: gatewayLaunch.options.env,
    },
  };
}

async function stopOrbStackGateway(gatewayLaunch, execute = run) {
  const stop = orbStackGatewayStop(gatewayLaunch);
  await execute(stop.command, stop.arguments, stop.options);
}

export async function waitForOrbStackGateway(url, children, environment, execute = run) {
  let lastError;
  for (let attempt = 0; attempt < 45; attempt += 1) {
    if (children.some((child) => child.exitCode !== null || child.signalCode !== null)) {
      throw new Error(`a local process exited before ${url.href} became ready`);
    }
    try {
      const output = await execute("curl", [
        "--fail",
        "--silent",
        "--max-time",
        "1",
        url.href,
      ], { capture: true, env: environment });
      if (await verifyLocalHealthResponse(Response.json(JSON.parse(output)))) return;
      lastError = new Error(`${url.href} returned an invalid health document`);
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 100));
  }
  throw new Error(`${url.href} did not become ready: ${errorMessage(lastError)}`);
}

function selectedEnvironment(environment, names) {
  const selected = {};
  for (const name of names) {
    if (environment[name] !== undefined) selected[name] = environment[name];
  }
  return selected;
}

function definedEnvironment(environment) {
  return Object.fromEntries(
    Object.entries(environment).filter(([, value]) => value !== undefined),
  );
}

function buildChildEnvironment(environment) {
  return {
    ...selectedEnvironment(environment, runtimeEnvironmentNames),
    ...selectedEnvironment(environment, buildEnvironmentNames),
  };
}

export async function rejectWorkerEnvironmentFiles(directory = webRoot) {
  const entries = await readdir(directory);
  const devVars = entries.filter(
    (name) => name === ".dev.vars" || name.startsWith(".dev.vars."),
  );
  if (devVars.length > 0) {
    throw new Error(
      `website Worker env files are disabled; move local settings to the root .env: ${devVars.join(", ")}`,
    );
  }
}

export function viteChildConfiguration(hostname, rawPort) {
  const port = Number(rawPort);
  if (
    (hostname !== "127.0.0.1" && hostname !== "localhost")
    || !Number.isSafeInteger(port)
    || port < 1_024
    || port > 65_535
  ) {
    throw new Error("invalid local Vite authority");
  }
  return {
    envDir: false,
    server: {
      // Node may resolve `localhost` to only ::1 while the browser/HMR client
      // independently selects 127.0.0.1. Binding the Vite child explicitly to
      // IPv4 makes one authority own both document and WebSocket traffic and
      // lets strictPort reject an existing wildcard listener.
      host: hostname === "localhost" ? "127.0.0.1" : hostname,
      port,
      strictPort: true,
      watch: { ignored: ["**/.env*", "**/.dev.vars*"] },
    },
  };
}

export function websiteChildLaunch(
  environment,
  origin,
  overrides,
  sentinelNames = [],
) {
  return {
    command: process.execPath,
    arguments: [
      scriptPath,
      "--vite-child",
      origin.hostname,
      origin.port,
      ...(sentinelNames.length > 0 ? ["--environment-sentinel", ...sentinelNames] : []),
    ],
    options: localStackChildOptions({
      cwd: webRoot,
      env: providerFreeWebEnvironment(environment, overrides),
      stdio: sentinelNames.length > 0
        ? ["ignore", "pipe", "inherit"]
        : ["ignore", "inherit", "inherit", "ipc"],
    }),
  };
}

export function localChatGptRelayChildLaunch(environment) {
  return {
    command: process.execPath,
    arguments: [scriptPath, "--chatgpt-relay-child"],
    options: localStackChildOptions({
      cwd: webRoot,
      env: buildChildEnvironment(environment),
      stdio: ["ignore", "inherit", "inherit", "ipc"],
    }),
  };
}

export function waitForLocalChatGptRelay(child, timeoutMs = 10_000) {
  return new Promise((resolveReady, rejectReady) => {
    const timeout = setTimeout(() => {
      cleanup();
      rejectReady(new Error("local ChatGPT transport relay did not become ready"));
    }, timeoutMs);
    const cleanup = () => {
      clearTimeout(timeout);
      child.off("message", onMessage);
      child.off("error", onError);
      child.off("exit", onExit);
    };
    const onMessage = (message) => {
      if (message?.type !== "nanocodex.chatgpt-relay.ready") return;
      let url;
      try { url = new URL(message.url); } catch { return; }
      if (url.protocol !== "http:" || url.hostname !== "127.0.0.1" || !url.port
        || url.pathname !== "/" || url.search || url.hash) return;
      cleanup();
      resolveReady(url.href);
    };
    const onError = (error) => {
      cleanup();
      rejectReady(error);
    };
    const onExit = (code, signal) => {
      cleanup();
      rejectReady(new Error(
        `local ChatGPT transport relay exited before readiness with ${code ?? signal}`,
      ));
    };
    child.on("message", onMessage);
    child.once("error", onError);
    child.once("exit", onExit);
  });
}

export function localStackChildOptions(options, platform = process.platform) {
  return {
    ...options,
    detached: platform !== "win32",
  };
}

async function runViteChild(hostname, port) {
  const stopWatchingParent = watchLocalStackParent();
  const { createServer } = await import("vite");
  const server = await createServer(viteChildConfiguration(hostname, port));
  try {
    await server.listen();
    server.printUrls();
  } catch (error) {
    stopWatchingParent();
    throw error;
  }
}

function printEnvironmentSentinel(names) {
  const values = {};
  for (const name of names) {
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) {
      throw new Error(`invalid sentinel environment name: ${name}`);
    }
    values[name] = process.env[name] ?? null;
  }
  process.stdout.write(`${JSON.stringify(values)}\n`);
}

async function sendManagedReadySentinel() {
  if (!process.send) throw new Error("managed readiness sentinel requires an IPC channel");
  await new Promise((resolveSend, rejectSend) => {
    process.send({ type: "nanocodex.dev.ready" }, (error) => {
      if (error) rejectSend(error);
      else resolveSend();
    });
  });
  process.disconnect();
}

async function runLocalChatGptRelayChild(port = 0) {
  let server;
  let serverClosing = false;
  const sockets = new Set();
  let parentDisconnected = !process.connected;
  if (!process.send && !parentDisconnected) {
    throw new Error("local ChatGPT transport relay requires IPC");
  }
  const closeAfterParentDisconnect = () => {
    parentDisconnected = true;
    if (!server) return;
    if (!serverClosing) {
      serverClosing = true;
      server.close();
    }
    for (const socket of sockets) socket.destroy();
  };
  process.once("disconnect", closeAfterParentDisconnect);
  if (parentDisconnected) {
    process.removeListener("disconnect", closeAfterParentDisconnect);
    return;
  }

  const { startRelay } = await import("../container/relay.mjs");
  if (parentDisconnected || !process.connected) {
    process.removeListener("disconnect", closeAfterParentDisconnect);
    return;
  }
  server = startRelay({ host: "127.0.0.1", port });
  server.on("connection", (socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
  });
  server.once("close", () => {
    process.removeListener("disconnect", closeAfterParentDisconnect);
  });
  if (parentDisconnected || !process.connected) closeAfterParentDisconnect();
  await new Promise((resolveListening, rejectListening) => {
    if (server.listening) {
      resolveListening();
      return;
    }
    server.once("listening", resolveListening);
    server.once("error", rejectListening);
  });
  if (parentDisconnected || !process.connected) {
    closeAfterParentDisconnect();
    return;
  }
  const address = server.address();
  if (!address || typeof address === "string") {
    server.close();
    throw new Error("local ChatGPT transport relay has no TCP address");
  }
  await new Promise((resolveSend, rejectSend) => {
    const onSend = (error) => {
      if (!error) {
        resolveSend();
        return;
      }
      if (
        parentDisconnected
        || !process.connected
        || error.code === "EPIPE"
        || error.code === "ERR_IPC_CHANNEL_CLOSED"
      ) {
        closeAfterParentDisconnect();
        resolveSend();
        return;
      }
      rejectSend(error);
    };
    try {
      process.send({
        type: "nanocodex.chatgpt-relay.ready",
        url: `http://127.0.0.1:${address.port}/`,
      }, onSend);
    } catch (error) {
      onSend(error);
    }
  });
}

export async function verifyLocalHealthResponse(response, authMode) {
  if (!response.ok) return false;
  const health = await response.json().catch(() => undefined);
  if (health?.status !== "ok") {
    throw new Error("local website health returned an invalid status document");
  }
  if (!authMode) return true;
  if (
    health.agent_configured !== true
    || health.credential_source !== "managed"
    || health.interactive_auth !== false
    || health.auth_mode !== authMode
  ) {
    throw new Error(
      `local website health did not attest non-interactive managed ${authMode} access`,
    );
  }
  return true;
}

export async function verifyLocalModelPreconnect(
  origin,
  WebSocketImplementation,
  timeoutMs = 10_000,
) {
  const WebSocketClass = WebSocketImplementation ?? (await import("ws")).default;
  const url = new URL("/api/responses", origin);
  url.protocol = "ws:";
  url.searchParams.set("session_id", randomBytes(32).toString("base64url"));
  const socket = new WebSocketClass(url, {
    handshakeTimeout: timeoutMs,
    origin: origin.origin,
  });
  try {
    await new Promise((resolveReady, rejectReady) => {
      const timer = setTimeout(
        () => fail(new Error("local model preconnect timed out")),
        timeoutMs,
      );
      const cleanup = () => {
        clearTimeout(timer);
        socket.off("message", onMessage);
        socket.off("error", onError);
        socket.off("close", onClose);
        socket.off("unexpected-response", onUnexpectedResponse);
      };
      const fail = (error) => {
        cleanup();
        rejectReady(error);
      };
      const onMessage = (data, isBinary) => {
        let message;
        try {
          message = isBinary ? undefined : JSON.parse(data.toString("utf8"));
        } catch {
          message = undefined;
        }
        if (
          message?.type !== "nanocodex.proxy.ready"
          || Object.keys(message).length !== 1
        ) {
          fail(new Error("local model preconnect returned an invalid attestation"));
          return;
        }
        cleanup();
        resolveReady();
      };
      const onError = () => fail(new Error("local model preconnect failed"));
      const onClose = (code) => fail(
        new Error(`local model preconnect closed before readiness with ${code}`),
      );
      const onUnexpectedResponse = (_request, response) => {
        response.resume();
        fail(new Error(
          `local model preconnect upgrade returned HTTP ${response.statusCode ?? 502}`,
        ));
      };
      socket.once("message", onMessage);
      socket.once("error", onError);
      socket.once("close", onClose);
      socket.once("unexpected-response", onUnexpectedResponse);
    });
  } finally {
    socket.on("error", () => {});
    if (socket.readyState === 1) socket.close(1_000, "readiness_complete");
    else socket.terminate?.();
  }
}

async function ensureLocalDependencies(environment, execute = run) {
  const packages = localDependencyRequirements();
  const missing = [];
  for (const { root, requiredFiles } of packages) {
    for (const requiredFile of requiredFiles) {
      try {
        const metadata = await stat(resolve(root, requiredFile));
        if (!metadata.isFile() || metadata.size === 0) {
          missing.push(root);
          break;
        }
      } catch (error) {
        if (error?.code !== "ENOENT") throw error;
        missing.push(root);
        break;
      }
    }
  }
  if (missing.length === 0) return;
  process.stderr.write("Preparing missing local Cloudflare Worker dependencies.\n");
  await Promise.all(missing.map((root) => execute("npm", ["ci", "--prefix", root], {
    cwd: repositoryRoot,
    env: environment,
  })));
}

export function localDependencyRequirements() {
  return [
    {
      root: reactRoot,
      requiredFiles: ["node_modules/nanocodex/package.json"],
    },
    {
      root: terminalRoot,
      requiredFiles: [
        "node_modules/streamdown/package.json",
        "node_modules/typescript/bin/tsc",
      ],
    },
    {
      root: webRoot,
      requiredFiles: [
        "node_modules/accounts/package.json",
        "node_modules/wrangler/bin/wrangler.js",
      ],
    },
    {
      root: connectDialogRoot,
      requiredFiles: ["node_modules/wrangler/bin/wrangler.js"],
    },
    {
      root: connectPlaygroundRoot,
      requiredFiles: ["node_modules/wrangler/bin/wrangler.js"],
    },
    {
      root: connectApiRoot,
      requiredFiles: ["node_modules/wrangler/bin/wrangler.js"],
    },
    {
      root: resolve(managedRoot, "../egress"),
      requiredFiles: ["node_modules/wrangler/bin/wrangler.js"],
    },
    {
      root: managedRoot,
      requiredFiles: ["node_modules/wrangler/bin/wrangler.js"],
    },
  ];
}

export async function resolveLocalAuthMode(
  options,
  environment,
  loginAvailable = hasCodexLogin,
) {
  if (options.withoutMultiplayer) return undefined;
  if (options.requestedMode === "api_key") {
    if (!environment.OPENAI_API_KEY?.trim()) {
      throw new Error(
        "--auth-mode=api_key requires OPENAI_API_KEY in the shell or repository-root .env",
      );
    }
    return "api_key";
  }
  if (options.requestedMode === "chatgpt") {
    if (!await loginAvailable(environment)) {
      throw new Error(
        "--auth-mode=chatgpt requires an existing 0600 Codex login on this host; run `codex login` before starting localhost",
      );
    }
    return "chatgpt";
  }
  if (environment.OPENAI_API_KEY?.trim()) return "api_key";
  if (await loginAvailable(environment)) return "chatgpt";
  throw new Error(
    "No existing local model credential was found. Run `codex login` once on this host, set OPENAI_API_KEY in the repository-root .env, or use `npm run dev:web` to omit managed Multiplayer. Localhost never starts an OAuth or device-code flow.",
  );
}

async function hasCodexLogin(environment) {
  const codexHome = environment.CODEX_HOME ?? join(homedir(), ".codex");
  const path = resolve(environment.NANOCODEX_CODEX_AUTH_FILE ?? join(codexHome, "auth.json"));
  try {
    await readCodexSubscription(path);
    return true;
  } catch {
    return false;
  }
}

async function readLocalChatGptBootstrap(environment) {
  const codexHome = environment.CODEX_HOME ?? join(homedir(), ".codex");
  const path = resolve(environment.NANOCODEX_CODEX_AUTH_FILE ?? join(codexHome, "auth.json"));
  try {
    const credential = await readCodexSubscription(path);
    const document = JSON.parse(await readFile(path, "utf8"));
    const refreshToken = document?.tokens?.refresh_token;
    return JSON.stringify({
      access_token: credential.accessToken,
      account_id: credential.accountId,
      expires_at: credential.expiresAt,
      fedramp: credential.fedramp,
      ...(typeof refreshToken === "string" && refreshToken ? { refresh_token: refreshToken } : {}),
    });
  } catch {
    return undefined;
  }
}

export async function verifyLocalState(
  origin,
  head,
  {
    environment = buildChildEnvironment(process.env),
    request = localFetch,
    verifyGit = verifyLocalGitAdvertisement,
  } = {},
) {
  const snapshotUrl = new URL("/api/repository/snapshot", origin);
  snapshotUrl.searchParams.set("generation", head);
  const indexUrl = new URL("/api/repository/commit-index", origin);
  indexUrl.searchParams.set("generation", head);
  const [snapshotResponse, indexResponse, evalsResponse] = await Promise.all([
    request(snapshotUrl, AbortSignal.timeout(10_000)),
    request(indexUrl, AbortSignal.timeout(10_000)),
    request(new URL("/api/evals", origin), AbortSignal.timeout(10_000)),
  ]);
  for (const [name, response] of [
    ["repository snapshot", snapshotResponse],
    ["commit index", indexResponse],
    ["eval overview", evalsResponse],
  ]) {
    if (!response.ok) throw new Error(`${name} verification returned HTTP ${response.status}`);
  }
  const [snapshot, index, evals] = await Promise.all([
    snapshotResponse.json(),
    indexResponse.json(),
    evalsResponse.json(),
  ]);
  if (
    !isRepositoryMetadata(snapshot, head)
    || !isRepositoryMetadata(index, head)
    || snapshotResponse.headers.get("x-repository-generation") !== head
    || indexResponse.headers.get("x-repository-generation") !== head
  ) {
    throw new Error("local repository publication did not resolve the current Git revision");
  }
  if (
    index.version !== 1
    || !Array.isArray(index?.hashes)
    || index.hashes.length !== index.repository.indexedCommits
    || index.hashes[0] !== head
    || !index.hashes.every((hash) => typeof hash === "string" && /^[a-f0-9]{40}$/.test(hash))
    || new Set(index.hashes).size !== index.hashes.length
    || !isCommitScopeCounts(index.scopeCounts, index.hashes.length)
  ) {
    throw new Error("local commit metadata did not begin at the current Git revision");
  }
  if (evals?.schemaVersion !== 5 || !Array.isArray(evals.worksets)) {
    throw new Error("local evaluation database did not resolve the current empty-capable schema");
  }

  const source = Array.isArray(snapshot?.tree)
    ? snapshot.tree.find((entry) =>
        entry?.path === "README.md"
        && typeof entry?.contentUrl === "string"
        && /^[a-f0-9]{40}$/.test(entry?.objectId)
      ) ?? snapshot.tree.find((entry) =>
        typeof entry?.contentUrl === "string"
        && /^[a-f0-9]{40}$/.test(entry?.objectId)
      )
    : undefined;
  if (!source) throw new Error("local Source metadata contained no readable blob");
  const blobUrl = new URL(source.contentUrl, origin);
  if (
    blobUrl.origin !== origin.origin
    || blobUrl.pathname !== `/api/repository/blob/${source.objectId}`
    || blobUrl.search
    || blobUrl.hash
  ) {
    throw new Error("local Source metadata returned an invalid blob URL");
  }
  const commitPageUrl = new URL("/api/repository/commits", origin);
  commitPageUrl.searchParams.set("generation", head);
  commitPageUrl.searchParams.set("page", "0");
  const patchUrl = new URL(`/api/repository/commits/${head}/0000.diff`, origin);
  const [blobResponse, commitPageResponse, patchResponse] = await Promise.all([
    request(blobUrl, AbortSignal.timeout(10_000)),
    request(commitPageUrl, AbortSignal.timeout(10_000)),
    request(patchUrl, AbortSignal.timeout(10_000)),
  ]);
  for (const [name, response] of [
    ["Source blob", blobResponse],
    ["commit page", commitPageResponse],
    ["commit patch", patchResponse],
  ]) {
    if (!response.ok) throw new Error(`${name} verification returned HTTP ${response.status}`);
  }
  const commitPage = await commitPageResponse.json();
  const expectedPageHashes = index.hashes.slice(0, index.repository.commitPageSize);
  if (
    commitPageResponse.headers.get("x-repository-generation") !== head
    || !isCommitPage(commitPage, expectedPageHashes)
  ) {
    throw new Error("local commit page did not begin at the current Git revision");
  }
  if (patchResponse.headers.get("x-repository-generation") !== head) {
    throw new Error("local commit patch did not resolve the current Git revision");
  }
  const [, patchPrefix] = await Promise.all([
    requireResponseContent(blobResponse, "Source blob"),
    readResponsePrefix(patchResponse, "commit patch"),
    verifyGit(origin, head, environment),
  ]);
  if (!patchPrefix.startsWith(`From ${head} `)) {
    throw new Error("local commit patch did not begin at the current Git revision");
  }
}

function isRepositoryMetadata(value, head) {
  return value != null
    && typeof value === "object"
    && value.repository != null
    && typeof value.repository === "object"
    && value.repository.head === head
    && typeof value.repository.branch === "string"
    && Number.isSafeInteger(value.repository.indexedCommits)
    && value.repository.indexedCommits > 0
    && Number.isSafeInteger(value.repository.commitPageSize)
    && value.repository.commitPageSize > 0
    && value.repository.commitPageSize <= 32
    && typeof value.generatedAt === "string";
}

function isCommitScopeCounts(value, total) {
  return value != null
    && typeof value === "object"
    && value.all === total
    && ["eval", "fix", "docs", "perf"].every((scope) =>
      Number.isSafeInteger(value[scope])
      && value[scope] >= 0
      && value[scope] <= total
    );
}

function isCommitPage(value, expectedHashes) {
  return Array.isArray(value)
    && value.length === expectedHashes.length
    && value.every((commit, index) =>
      commit != null
      && typeof commit === "object"
      && commit.hash === expectedHashes[index]
      && typeof commit.shortHash === "string"
      && typeof commit.author === "string"
      && typeof commit.authoredAt === "string"
      && typeof commit.subject === "string"
      && typeof commit.body === "string"
      && isStringArray(commit.parents)
      && isStringArray(commit.refs)
      && Array.isArray(commit.files)
      && isCommitStats(commit.stats)
    );
}

function isStringArray(value) {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function isCommitStats(value) {
  return value != null
    && typeof value === "object"
    && ["files", "additions", "deletions"].every((field) =>
      Number.isSafeInteger(value[field]) && value[field] >= 0
    );
}

async function requireResponseContent(response, name) {
  const reader = response.body?.getReader();
  if (!reader) throw new Error(`${name} verification returned no body`);
  try {
    while (true) {
      const next = await reader.read();
      if (next.done) throw new Error(`${name} verification returned an empty body`);
      if (next.value.byteLength > 0) return;
    }
  } finally {
    await reader.cancel().catch(() => {});
  }
}

async function readResponsePrefix(response, name, limit = 256) {
  const reader = response.body?.getReader();
  if (!reader) throw new Error(`${name} verification returned no body`);
  const chunks = [];
  let total = 0;
  try {
    while (total < limit) {
      const next = await reader.read();
      if (next.done) break;
      if (next.value.byteLength === 0) continue;
      const chunk = next.value.slice(0, limit - total);
      chunks.push(chunk);
      total += chunk.byteLength;
      if (chunk.includes(0x0a)) break;
    }
  } finally {
    await reader.cancel().catch(() => {});
  }
  if (total === 0) throw new Error(`${name} verification returned an empty body`);
  const prefix = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    prefix.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder().decode(prefix);
}

export async function verifyLocalGitAdvertisement(
  origin,
  head,
  environment = buildChildEnvironment(process.env),
  execute = run,
) {
  const output = await execute("git", [
    "-c",
    "credential.helper=",
    "-c",
    "protocol.version=2",
    "ls-remote",
    "--symref",
    "--exit-code",
    new URL("/git", origin).href,
    "HEAD",
    "refs/heads/master",
  ], {
    capture: true,
    cwd: webRoot,
    env: {
      ...environment,
      GIT_CONFIG_GLOBAL: process.platform === "win32" ? "NUL" : "/dev/null",
      GIT_CONFIG_NOSYSTEM: "1",
      GIT_TERMINAL_PROMPT: "0",
    },
  });
  const lines = new Set(output.trimEnd().split(/\r?\n/));
  if (
    lines.size !== 3
    || !lines.has("ref: refs/heads/master\tHEAD")
    || !lines.has(`${head}\tHEAD`)
    || !lines.has(`${head}\trefs/heads/master`)
  ) {
    throw new Error("local read-only Git advertisement did not resolve the current HEAD");
  }
}

export async function verifyLocalMultiplayer(
  origin,
  request = localFetch,
  WebSocketImplementation,
  timeoutMs = 10_000,
) {
  const createUrl = new URL("/v1/rooms", origin);
  const createId = randomBytes(32).toString("base64url");
  const created = await request(
    createUrl,
    AbortSignal.timeout(timeoutMs),
    {
      method: "POST",
      headers: {
        "content-type": "application/json",
        origin: origin.origin,
      },
      body: JSON.stringify({
        create_id: createId,
        display_name: "Local verifier",
      }),
    },
  );
  if (created.status !== 201) {
    await created.body?.cancel();
    throw new Error(`local Multiplayer room creation returned HTTP ${created.status}`);
  }
  const receipt = await created.json().catch(() => undefined);
  const roomId = receipt?.room_id;
  const memberId = receipt?.member_id;
  const authMode = receipt?.auth_mode;
  const setCookie = created.headers.get("set-cookie");
  const cookie = setCookie?.split(";", 1)[0];
  const expectedCookieName = typeof roomId === "string"
    ? `nanocodex_room_${roomId.replaceAll("-", "")}`
    : undefined;
  if (
    typeof roomId !== "string"
    || !/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}~[A-Za-z0-9_-]{43}$/.test(roomId)
    || !cookie?.startsWith(`${expectedCookieName}=`)
    || !/^[A-Za-z0-9_-]{43}$/.test(cookie.slice(expectedCookieName.length + 1))
  ) {
    throw new Error("local Multiplayer room creation returned an invalid receipt");
  }

  const roomUrl = new URL(`/v1/rooms/${roomId}`, origin);
  let verificationError;
  try {
    if (
      typeof memberId !== "string"
      || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(memberId)
      || (authMode !== "api_key" && authMode !== "chatgpt")
    ) {
      throw new Error("local Multiplayer room creation returned an invalid receipt");
    }
    await verifyLocalMultiplayerSocket(
      origin,
      roomId,
      memberId,
      authMode,
      cookie,
      WebSocketImplementation,
      timeoutMs,
    );
  } catch (error) {
    verificationError = error;
  }

  let cleanupError;
  for (let attempt = 0; attempt < 30; attempt += 1) {
    try {
      const deleted = await request(
        roomUrl,
        AbortSignal.timeout(timeoutMs),
        { method: "DELETE", headers: { cookie } },
      );
      if (deleted.status === 204 || deleted.status === 404) {
        await deleted.body?.cancel();
        cleanupError = undefined;
        break;
      }
      await deleted.body?.cancel();
      cleanupError = new Error(`local Multiplayer room cleanup returned HTTP ${deleted.status}`);
    } catch (error) {
      cleanupError = error;
    }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 100));
  }
  if (verificationError && cleanupError) {
    throw new AggregateError([verificationError, cleanupError], "local Multiplayer verification and cleanup failed");
  }
  if (verificationError) throw verificationError;
  if (cleanupError) throw cleanupError;
}

async function verifyLocalMultiplayerSocket(
  origin,
  roomId,
  memberId,
  authMode,
  cookie,
  WebSocketImplementation,
  timeoutMs,
) {
  const WebSocketClass = WebSocketImplementation ?? (await import("ws")).default;
  const url = new URL(`/v1/rooms/${roomId}/ws`, origin);
  url.protocol = "ws:";
  url.searchParams.set("cursor", "0");
  const socket = new WebSocketClass(url, {
    handshakeTimeout: timeoutMs,
    headers: { cookie },
    origin: origin.origin,
  });
  try {
    await new Promise((resolveReady, rejectReady) => {
      let opened = false;
      const timer = setTimeout(
        () => fail(new Error("local Multiplayer room WebSocket timed out")),
        timeoutMs,
      );
      const cleanup = () => {
        clearTimeout(timer);
        socket.off("open", onOpen);
        socket.off("message", onMessage);
        socket.off("error", onError);
        socket.off("close", onClose);
        socket.off("unexpected-response", onUnexpectedResponse);
      };
      const fail = (error) => {
        cleanup();
        rejectReady(error);
      };
      const onOpen = () => { opened = true; };
      const onMessage = (data, isBinary) => {
        let message;
        try {
          message = isBinary ? undefined : JSON.parse(data.toString("utf8"));
        } catch {
          message = undefined;
        }
        if (
          !opened
          || !isValidLocalRoomReady(message, { authMode, memberId, roomId })
        ) {
          fail(new Error("local Multiplayer room WebSocket returned an invalid ready frame"));
          return;
        }
        cleanup();
        resolveReady();
      };
      const onError = () => fail(new Error("local Multiplayer room WebSocket failed"));
      const onClose = (code) => fail(
        new Error(`local Multiplayer room WebSocket closed before readiness with ${code}`),
      );
      const onUnexpectedResponse = (_request, response) => {
        response.resume();
        fail(new Error(
          `local Multiplayer room WebSocket upgrade returned HTTP ${response.statusCode ?? 502}`,
        ));
      };
      socket.once("open", onOpen);
      socket.once("message", onMessage);
      socket.once("error", onError);
      socket.once("close", onClose);
      socket.once("unexpected-response", onUnexpectedResponse);
    });
  } finally {
    socket.on("error", () => {});
    if (socket.readyState === 1) socket.close(1_000, "readiness_complete");
    else socket.terminate?.();
  }
}

function isValidLocalRoomReady(message, expected) {
  if (!message || typeof message !== "object" || Array.isArray(message)) return false;
  const fields = [
    "auth_mode",
    "can_end_room",
    "can_target_agent",
    "latest_cursor",
    "member_id",
    "members",
    "online_member_ids",
    "room_id",
    "type",
  ];
  const keys = Object.keys(message).sort();
  if (keys.length !== fields.length || keys.some((key, index) => key !== fields[index])) {
    return false;
  }
  if (
    message.type !== "ready"
    || message.room_id !== expected.roomId
    || message.member_id !== expected.memberId
    || message.auth_mode !== expected.authMode
    || message.can_target_agent !== true
    || message.can_end_room !== true
    || typeof message.latest_cursor !== "string"
    || !/^(0|[1-9][0-9]{0,18})$/.test(message.latest_cursor)
    || !Array.isArray(message.members)
    || message.members.length < 1
    || message.members.length > 64
    || !Array.isArray(message.online_member_ids)
    || message.online_member_ids.length < 1
    || message.online_member_ids.length > 64
  ) return false;

  const members = new Set();
  for (const member of message.members) {
    if (
      !member
      || typeof member !== "object"
      || Array.isArray(member)
      || Object.keys(member).sort().join(",") !== "id,name"
      || typeof member.id !== "string"
      || !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(member.id)
      || typeof member.name !== "string"
      || !member.name.trim()
      || Buffer.byteLength(member.name, "utf8") > 64
      || members.has(member.id)
    ) return false;
    members.add(member.id);
  }
  if (!members.has(expected.memberId)) return false;
  const online = new Set();
  for (const onlineMemberId of message.online_member_ids) {
    if (
      typeof onlineMemberId !== "string"
      || !members.has(onlineMemberId)
      || online.has(onlineMemberId)
    ) return false;
    online.add(onlineMemberId);
  }
  return online.has(expected.memberId);
}

async function waitForHttp(url, children, ready) {
  let lastError;
  for (let attempt = 0; attempt < 600; attempt += 1) {
    if (children.some((child) => child.exitCode !== null || child.signalCode !== null)) {
      throw new Error(`a local process exited before ${url.href} became ready`);
    }
    try {
      const response = await localFetch(url, AbortSignal.timeout(1_000));
      if (await ready(response)) {
        if (!response.bodyUsed) await response.body?.cancel();
        return;
      }
      if (!response.bodyUsed) await response.body?.cancel();
      lastError = new Error(`${url.href} returned HTTP ${response.status}`);
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 100));
  }
  throw new Error(`${url.href} did not become ready: ${errorMessage(lastError)}`);
}

function localFetch(url, signal, init = {}) {
  return new Promise((resolveFetch, rejectFetch) => {
    const outgoing = httpRequest(url, {
      method: init.method ?? "GET",
      headers: init.headers,
      signal,
    }, (incoming) => {
      const headers = new Headers();
      for (let index = 0; index < incoming.rawHeaders.length; index += 2) {
        headers.append(incoming.rawHeaders[index], incoming.rawHeaders[index + 1]);
      }
      const status = incoming.statusCode ?? 500;
      const bodyless = init.method === "HEAD" || status === 204 || status === 205 || status === 304;
      if (bodyless) incoming.resume();
      resolveFetch(new Response(bodyless ? null : Readable.toWeb(incoming), {
        status,
        statusText: incoming.statusMessage,
        headers,
      }));
    });
    outgoing.once("error", rejectFetch);
    outgoing.end(init.body);
  });
}

async function gitHead(repository, environment, execute = run) {
  const result = await execute("git", ["rev-parse", "HEAD"], {
    cwd: repository,
    capture: true,
    env: environment,
  });
  const head = result.trim();
  if (!/^[a-f0-9]{40}$/.test(head)) throw new Error("local Git HEAD is invalid");
  return head;
}

export class LocalStackLifecycle {
  #children = [];
  #exits = [];
  #graceMs;
  #handlers = new Map();
  #shutdown;
  #signal;

  constructor({ graceMs = 2_000 } = {}) {
    if (!Number.isSafeInteger(graceMs) || graceMs < 1) {
      throw new Error("local process-group shutdown grace must be a positive integer");
    }
    this.#graceMs = graceMs;
  }

  get children() {
    return this.#children;
  }

  get signal() {
    return this.#signal;
  }

  installSignalHandlers() {
    if (this.#handlers.size > 0) throw new Error("local signal ownership was already installed");
    for (const signal of ["SIGINT", "SIGTERM"]) {
      const handler = () => {
        this.#signal ??= signal;
        if (!this.#shutdown) {
          const shutdown = this.stop(signal);
          void shutdown.catch(() => {});
          return;
        }
        this.#forceStop();
      };
      this.#handlers.set(signal, handler);
      process.on(signal, handler);
    }
  }

  removeSignalHandlers() {
    for (const [signal, handler] of this.#handlers) process.removeListener(signal, handler);
    this.#handlers.clear();
  }

  spawn(command, arguments_, options, name = command) {
    if (this.#shutdown || this.#signal) {
      throw new Error(`cannot start ${name} while local shutdown is in progress`);
    }
    const child = spawn(command, arguments_, localStackChildOptions(options));
    const exit = childExit(child, name);
    void exit.catch(() => {});
    this.#children.push(child);
    this.#exits.push(exit);
    return { child, exit };
  }

  async run(command, arguments_, { capture = false, ...options } = {}, name = command) {
    const stdio = capture ? ["ignore", "pipe", "inherit"] : "inherit";
    const { child } = this.spawn(command, arguments_, { ...options, stdio }, name);
    let stdout = "";
    if (capture) {
      child.stdout.setEncoding("utf8").on("data", (chunk) => { stdout += chunk; });
    }
    await new Promise((resolveRun, rejectRun) => {
      child.once("error", rejectRun);
      child.once("close", (code, signal) => {
        if (code === 0) resolveRun();
        else rejectRun(new Error(`${command} exited with ${code ?? signal}`));
      });
    });
    if (!await waitForLocalStackGroups([child], this.#graceMs)) {
      throw new Error(`${name} left a live process group after its command exited`);
    }
    const retained = this.#children.indexOf(child);
    if (retained >= 0) {
      this.#children.splice(retained, 1);
      this.#exits.splice(retained, 1);
    }
    return stdout;
  }

  stop(signal = this.#signal ?? "SIGTERM") {
    if (!this.#shutdown) {
      const stopping = stopLocalStackChildren(this.#children, this.#exits, {
        graceMs: this.#graceMs,
        signal,
      });
      this.#shutdown = stopping.then(() => {
        this.#children.length = 0;
        this.#exits.length = 0;
      });
      void this.#shutdown.catch(() => {});
    }
    return this.#shutdown;
  }

  #forceStop() {
    for (const child of this.#children) {
      try {
        terminateLocalStackChild(child, "SIGKILL");
      } catch (error) {
        process.stderr.write(
          `Failed to force local process-group shutdown: ${errorMessage(error)}\n`,
        );
      }
    }
  }
}

function run(command, arguments_, { capture = false, ...options } = {}) {
  return new Promise((resolveRun, rejectRun) => {
    let stdout = "";
    const child = spawn(command, arguments_, {
      ...options,
      stdio: capture ? ["ignore", "pipe", "inherit"] : "inherit",
    });
    if (capture) child.stdout.setEncoding("utf8").on("data", (chunk) => { stdout += chunk; });
    child.once("error", rejectRun);
    child.once("close", (code, signal) => {
      if (code === 0) resolveRun(stdout);
      else rejectRun(new Error(`${command} exited with ${code ?? signal}`));
    });
  });
}

export function watchLocalStackParent({
  kill = process.kill,
  platform = process.platform,
  processObject = process,
} = {}) {
  if (typeof processObject.send !== "function") {
    throw new Error("local detached child requires an IPC parent watchdog");
  }
  let watching = true;
  const onDisconnect = () => {
    if (!watching) return;
    watching = false;
    if (platform === "win32") {
      processObject.exit(1);
      return;
    }
    try {
      kill(-processObject.pid, "SIGTERM");
    } catch (error) {
      if (error?.code !== "ESRCH") {
        processObject.stderr.write(
          `Failed to terminate orphaned local process group: ${errorMessage(error)}\n`,
        );
      }
      processObject.exit(1);
    }
  };
  processObject.once("disconnect", onDisconnect);
  if (processObject.connected === false) queueMicrotask(onDisconnect);
  return () => {
    watching = false;
    processObject.removeListener("disconnect", onDisconnect);
  };
}

function childExit(child, name) {
  return new Promise((resolveExit, rejectExit) => {
    child.once("error", rejectExit);
    child.once("exit", (code, signal) => resolveExit({ name, code, signal }));
  });
}

export function waitForManagedStack(child, timeoutMs = 60_000) {
  return new Promise((resolveReady, rejectReady) => {
    const timeout = setTimeout(() => {
      cleanup();
      rejectReady(new Error("managed Multiplayer stack did not attest readiness"));
    }, timeoutMs);
    const onMessage = (message) => {
      if (
        message == null
        || typeof message !== "object"
        || message.type !== "nanocodex.dev.ready"
        || Object.keys(message).length !== 1
      ) return;
      cleanup();
      resolveReady();
    };
    const onError = (error) => {
      cleanup();
      rejectReady(error);
    };
    const onExit = (code, signal) => {
      cleanup();
      rejectReady(new Error(`managed Multiplayer stack exited before readiness with ${code ?? signal}`));
    };
    const cleanup = () => {
      clearTimeout(timeout);
      child.off("message", onMessage);
      child.off("error", onError);
      child.off("exit", onExit);
    };
    child.on("message", onMessage);
    child.once("error", onError);
    child.once("exit", onExit);
  });
}

export function terminateLocalStackChild(
  child,
  signal,
  kill = process.kill,
  platform = process.platform,
) {
  if (!Number.isSafeInteger(child.pid) || child.pid <= 0) return false;
  if (platform === "win32") {
    if (child.exitCode !== null || child.signalCode !== null) return false;
    return child.kill(signal);
  }
  try {
    kill(-child.pid, signal);
    return true;
  } catch (error) {
    // Every group created by this lifecycle has the current process's uid. An
    // inaccessible group therefore cannot still be the group we created; its
    // numeric pgid was either released or reused outside our authority.
    if (error?.code === "ESRCH" || error?.code === "EPERM") return false;
    throw error;
  }
}

export async function stopLocalStackChildren(
  children,
  exits,
  {
    graceMs = 2_000,
    isAlive = localStackChildIsAlive,
    signal = "SIGTERM",
    terminate = terminateLocalStackChild,
  } = {},
) {
  if (!Number.isSafeInteger(graceMs) || graceMs < 1) {
    throw new Error("local process-group shutdown grace must be a positive integer");
  }
  const errors = signalLocalStackChildren(children, signal, terminate);
  const settled = Promise.allSettled(exits);
  if (!await waitForLocalStackGroups(children, graceMs, isAlive)) {
    const remaining = children.filter((child) => isAlive(child));
    errors.push(...signalLocalStackChildren(remaining, "SIGKILL", terminate));
  }
  if (!await waitForLocalStackGroups(children, graceMs, isAlive)) {
    errors.push(new Error("local process groups remained after SIGKILL"));
  }
  if (!await settleWithin(settled, graceMs)) {
    errors.push(new Error("local stack children did not exit after process-group SIGKILL"));
  }
  if (errors.length > 0) {
    throw new AggregateError(errors, "local process-group shutdown failed");
  }
}

export function localStackChildIsAlive(
  child,
  kill = process.kill,
  platform = process.platform,
) {
  if (!Number.isSafeInteger(child.pid) || child.pid <= 0) return false;
  if (platform === "win32") {
    return child.exitCode === null && child.signalCode === null;
  }
  try {
    kill(-child.pid, 0);
    return true;
  } catch (error) {
    if (error?.code === "ESRCH") return false;
    if (error?.code === "EPERM") return false;
    throw error;
  }
}

function signalLocalStackChildren(children, signal, terminate) {
  const errors = [];
  for (const child of children) {
    try {
      terminate(child, signal);
    } catch (error) {
      errors.push(error);
    }
  }
  return errors;
}

async function waitForLocalStackGroups(
  children,
  timeoutMs,
  isAlive = localStackChildIsAlive,
) {
  const deadline = Date.now() + timeoutMs;
  while (children.some((child) => isAlive(child))) {
    const remaining = deadline - Date.now();
    if (remaining <= 0) return false;
    await new Promise((resolveDelay) => setTimeout(resolveDelay, Math.min(25, remaining)));
  }
  return true;
}

export function requireLocalProcessGroups(platform = process.platform) {
  if (platform === "win32") {
    throw new Error(
      "Nanocodex local development requires Unix process-group semantics so descendant shutdown can be proved",
    );
  }
}

async function settleWithin(settled, timeoutMs) {
  let timer;
  const completed = await Promise.race([
    settled.then(() => true),
    new Promise((resolveTimeout) => {
      timer = setTimeout(() => resolveTimeout(false), timeoutMs);
    }),
  ]);
  clearTimeout(timer);
  return completed;
}

function signalExitCode(signal) {
  if (signal === "SIGINT") return 130;
  if (signal === "SIGTERM") return 143;
  return signal ? 1 : 0;
}

function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

if (resolve(process.argv[1] ?? "") === scriptPath) {
  if (process.argv[2] === "--vite-child") {
    if (process.argv[5] === "--environment-sentinel") {
      printEnvironmentSentinel(process.argv.slice(6));
    } else {
      if (process.argv.length !== 5) throw new Error("--vite-child requires hostname and port");
      await runViteChild(process.argv[3], process.argv[4]);
    }
  } else if (process.argv[2] === "--environment-sentinel") {
    printEnvironmentSentinel(process.argv.slice(3));
  } else if (process.argv[2] === "--managed-ready-sentinel") {
    await sendManagedReadySentinel();
  } else if (process.argv[2] === "--chatgpt-relay-child") {
    const relayPort = process.argv[3] === undefined ? 0 : Number(process.argv[3]);
    if (!Number.isSafeInteger(relayPort) || relayPort < 0 || relayPort > 65_535) {
      throw new Error("--chatgpt-relay-child port must be an integer from 0 through 65535");
    }
    await runLocalChatGptRelayChild(relayPort);
  } else {
    await main();
  }
}
