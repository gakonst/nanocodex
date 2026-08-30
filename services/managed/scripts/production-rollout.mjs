import { execFileSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  buildProductionBrokerConfig,
} from "../../egress/scripts/production-broker.mjs";
import {
  isMissingWorkerDeleteError,
  redactSecrets,
  runBoundedProcess,
} from "./child-process.mjs";
import {
  verifyManagedWasmArtifact,
} from "../../../js/bindings/scripts/check-managed-wasm.mjs";

const workersRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repositoryRoot = resolve(workersRoot, "../..");
const webRoot = resolve(repositoryRoot, "web");
const brokerRoot = resolve(workersRoot, "../egress");
const managedConfigPath = resolve(workersRoot, "wrangler.jsonc");
const brokerConfigPath = resolve(brokerRoot, "wrangler.broker.jsonc");
const webArtifactConfigPath = resolve(webRoot, "dist/nanocodex/wrangler.json");
const webBuildAttestationPath = resolve(webRoot, "dist/nanocodex/build-attestation.json");
const webSourceConfigPath = resolve(webRoot, "wrangler.jsonc");
const wranglerPath = resolve(workersRoot, "node_modules/wrangler/bin/wrangler.js");
const webWranglerPath = resolve(webRoot, "node_modules/wrangler/bin/wrangler.js");
const managedMainPath = resolve(workersRoot, "src/index.ts");
const brokerMainPath = resolve(brokerRoot, "src/egress.ts");
const probeMainPath = resolve(workersRoot, "scripts/production-boundary-probe-worker.mjs");
const lifecycleAbort = new AbortController();

const BROKER_NAME = "nanocodex-egress";
const MANAGED_TEMPLATE_NAME = "nanocodex-managed-development";
const MANAGED_NAME = "nanocodex-durable-agent";
const WEB_NAME = "nanocodex";
const MANAGED_DURABLE_OBJECT_MIGRATIONS = [
  ["v1", [
    ["NANOCODEX_SESSIONS", "NanocodexSession"],
    ["NANOCODEX_ROOMS", "MultiplayerRoom"],
    ["NANOCODEX_MULTIPLAYER_QUOTA", "MultiplayerQuota"],
  ]],
  ["v2", [
    ["NANOCODEX_AUTH", "NonceStorage"],
    ["NANOCODEX_USERS", "UserAccount"],
    ["NANOCODEX_API_KEYS", "ApiKeyRecord"],
  ]],
  ["v3", [["NANOCODEX_MEMORY", "MemoryScope"]]],
  ["v4", [["NANOCODEX_ORGANIZATIONS", "Organization"]]],
];
const PROVIDER_NAMES = [
  "NANOCODEX_MANAGED_AUTH_MODE",
  "OPENAI_API_KEY",
  "CODEX_OAUTH_BOOTSTRAP",
  "CODEX_RELAY_URL",
  "NANOCODEX_MANAGED_OPENAI_API_KEY",
  "NANOCODEX_MANAGED_CODEX_OAUTH_BOOTSTRAP",
  "NANOCODEX_MANAGED_CODEX_RELAY_URL",
];
const APPLICATION_SECRET_NAMES = [
  "NANOCODEX_ADMIN_TOKEN",
  "NANOCODEX_BROKER_PROBE_TOKEN",
  "NANOCODEX_CREDENTIAL_ENCRYPTION_KEY",
  "NANOCODEX_GITHUB_OAUTH_CLIENT_ID",
  "NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET",
  "NANOCODEX_GOOGLE_OAUTH_CLIENT_ID",
  "NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET",
  "NANOCODEX_X_OAUTH_CLIENT_ID",
  "NANOCODEX_X_OAUTH_CLIENT_SECRET",
  "NANOCODEX_WHOOP_OAUTH_CLIENT_ID",
  "NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET",
  "NANOCODEX_BOUNDARY_PROBE_TOKEN",
  "SESSION_CREDENTIAL_KEY",
  "SESSION_CREDENTIAL_KEY_PREVIOUS",
];

export function assertProductionPreflight(environment) {
  const revision = productionRevision(environment.TARGET_SHA);
  requiredEnvironment(environment, "CLOUDFLARE_ACCOUNT_ID");
  if (environment.CLOUDFLARE_API_TOKEN_CONFIGURED !== "true"
    && environment.CLOUDFLARE_OAUTH_CONFIGURED !== "true") {
    throw new Error(
      "Cloudflare API token or an authenticated local Wrangler OAuth session is required for production rollout",
    );
  }
  requireConfigured(environment, "NANOCODEX_CREDENTIAL_ENCRYPTION_KEY_CONFIGURED");
  requireConfigured(environment, "NANOCODEX_BROKER_PROBE_TOKEN_CONFIGURED");
  requireConfigured(environment, "NANOCODEX_GITHUB_OAUTH_CLIENT_ID_CONFIGURED");
  requireConfigured(environment, "NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET_CONFIGURED");
  requireConfigured(environment, "NANOCODEX_GOOGLE_OAUTH_CLIENT_ID_CONFIGURED");
  requireConfigured(environment, "NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET_CONFIGURED");
  requireOptionalConfiguredPair(
    environment,
    "NANOCODEX_X_OAUTH_CLIENT_ID_CONFIGURED",
    "NANOCODEX_X_OAUTH_CLIENT_SECRET_CONFIGURED",
    "X OAuth application credentials",
  );
  requireOptionalConfiguredPair(
    environment,
    "NANOCODEX_WHOOP_OAUTH_CLIENT_ID_CONFIGURED",
    "NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET_CONFIGURED",
    "WHOOP OAuth application credentials",
  );
  requireConfigured(environment, "NANOCODEX_GIT_TOKEN_CONFIGURED");
  const adminToken = requiredSecret(environment, "NANOCODEX_ADMIN_TOKEN");
  assertTokenStrength(adminToken, "NANOCODEX_ADMIN_TOKEN");
  return { adminToken, revision };
}

export function assertProductionCheckout(revision, {
  dirty,
  head,
  originMaster,
}) {
  const expected = productionRevision(revision);
  if (head !== expected) {
    throw new Error("production rollout checkout must match TARGET_SHA");
  }
  if (originMaster !== expected) {
    throw new Error("production rollout checkout must be the fetched origin/master");
  }
  if (dirty) {
    throw new Error("production rollout checkout must not contain tracked changes");
  }
}

export function assertWebBuildAttestation(attestation, revision, sourceConfig) {
  assertRecord(attestation, "website build attestation");
  const expectedRevision = productionRevision(revision);
  if (attestation.revision !== expectedRevision) {
    throw new Error("website build artifact must match the exact production revision");
  }
  const expectedConfig = createHash("sha256").update(sourceConfig).digest("hex");
  if (attestation.wranglerConfigSha256 !== expectedConfig) {
    throw new Error("website build artifact must match the production Wrangler config");
  }
}

export function buildManagedProductionConfig(baseConfig, {
  mainPath = managedMainPath,
} = {}) {
  assertRecord(baseConfig, "managed config");
  if (baseConfig.name !== MANAGED_TEMPLATE_NAME) {
    throw new Error("production managed config must use the non-production template name");
  }
  if (baseConfig.workers_dev !== false || baseConfig.routes !== undefined) {
    throw new Error("production managed Worker must remain private");
  }
  assertExactService(
    baseConfig.services,
    "NANOCODEX",
    BROKER_NAME,
    "production managed Worker",
  );
  const durableObjects = new Map(
    (baseConfig.durable_objects?.bindings ?? []).map((binding) => [
      binding?.name,
      binding?.class_name,
    ]),
  );
  const expectedBindings = MANAGED_DURABLE_OBJECT_MIGRATIONS.flatMap(([, bindings]) => bindings);
  if ((baseConfig.durable_objects?.bindings ?? []).length !== expectedBindings.length
    || durableObjects.size !== expectedBindings.length) {
    throw new Error("production managed Worker has an unexpected Durable Object binding");
  }
  for (const [name, className] of expectedBindings) {
    if (durableObjects.get(name) !== className) {
      throw new Error(`production managed Worker requires ${name}`);
    }
  }
  const migrationTags = baseConfig.migrations?.map((migration) => migration?.tag);
  const expectedTags = MANAGED_DURABLE_OBJECT_MIGRATIONS.map(([tag]) => tag);
  if (JSON.stringify(migrationTags) !== JSON.stringify(expectedTags)) {
    throw new Error("production managed Worker requires the complete ordered migration history");
  }
  for (const [index, [tag, bindings]] of MANAGED_DURABLE_OBJECT_MIGRATIONS.entries()) {
    const actual = baseConfig.migrations[index]?.new_sqlite_classes;
    const expected = bindings.map(([, className]) => className);
    if (JSON.stringify(actual) !== JSON.stringify(expected)) {
      throw new Error(`production managed Worker requires the exact ${tag} SQLite migration`);
    }
  }
  assertNoProviderConfiguration(baseConfig, "managed config");

  return {
    ...baseConfig,
    name: MANAGED_NAME,
    main: resolve(mainPath),
  };
}

export function managedSecretPayload(adminToken) {
  assertTokenStrength(adminToken, "NANOCODEX_ADMIN_TOKEN");
  return { NANOCODEX_ADMIN_TOKEN: adminToken };
}

export function webSecretPayload(gitMirrorToken) {
  assertTokenStrength(gitMirrorToken, "NANOCODEX_GIT_TOKEN");
  return { GIT_MIRROR_TOKEN: gitMirrorToken };
}

export function buildBoundaryProbeConfig({
  name,
  revision,
  mainPath = probeMainPath,
} = {}) {
  if (typeof name !== "string"
    || !/^nanocodex-boundary-[a-z0-9-]{12,48}$/.test(name)
    || name.length > 63) {
    throw new Error("boundary probe Worker name is invalid");
  }
  return {
    name,
    main: resolve(mainPath),
    compatibility_date: "2026-07-29",
    compatibility_flags: ["nodejs_compat"],
    workers_dev: true,
    preview_urls: false,
    minify: true,
    observability: { enabled: false },
    services: [{ binding: "NANOCODEX", service: BROKER_NAME }],
    vars: {
      DEPLOYMENT_SHA: productionRevision(revision),
    },
  };
}

export function buildWebProductionConfig(baseConfig, {
  artifactDirectory,
  currentWebRoot = webRoot,
  d1DatabaseIds,
} = {}) {
  assertRecord(baseConfig, "website artifact config");
  if (baseConfig.name !== WEB_NAME) {
    throw new Error("production website artifact has an unexpected Worker name");
  }
  const backend = baseConfig.services?.filter(
    (candidate) => candidate?.binding === "NANOCODEX_BACKEND",
  );
  if (backend?.length !== 1 || backend[0].service !== MANAGED_NAME) {
    throw new Error("production website requires NANOCODEX_BACKEND bound to nanocodex-durable-agent");
  }
  const connectDialog = baseConfig.services?.filter(
    (candidate) => candidate?.binding === "NANOCODEX_CONNECT_DIALOG",
  );
  if (connectDialog?.length !== 1 || connectDialog[0].service !== "nanocodex-connect-dialog") {
    throw new Error("production website requires NANOCODEX_CONNECT_DIALOG bound to nanocodex-connect-dialog");
  }
  const connectApi = baseConfig.services?.filter(
    (candidate) => candidate?.binding === "NANOCODEX_CONNECT_API",
  );
  if (connectApi?.length !== 1 || connectApi[0].service !== "nanocodex-connect-api") {
    throw new Error("production website requires NANOCODEX_CONNECT_API bound to nanocodex-connect-api");
  }
  if (baseConfig.keep_vars !== true) {
    throw new Error("production website must retain its unrelated server-side bindings");
  }
  assertNoProviderConfiguration(baseConfig, "website artifact config");
  const configDirectory = resolve(artifactDirectory);
  if (typeof baseConfig.main !== "string" || isAbsolute(baseConfig.main)) {
    throw new Error("website artifact main must be relative to its generated config");
  }
  if (typeof baseConfig.assets?.directory !== "string"
    || isAbsolute(baseConfig.assets.directory)) {
    throw new Error("website artifact assets must be relative to its generated config");
  }
  if (!Array.isArray(baseConfig.containers) || baseConfig.containers.length !== 1
    || baseConfig.containers[0]?.class_name !== "ChatGptEgress") {
    throw new Error("website artifact must retain its one ChatGptEgress container");
  }

  const { configPath: _configPath, userConfigPath: _userConfigPath, ...portable } = baseConfig;
  return {
    ...portable,
    services: [
      { binding: "NANOCODEX_BACKEND", service: MANAGED_NAME },
      { binding: "NANOCODEX_CONNECT_API", service: "nanocodex-connect-api" },
      { binding: "NANOCODEX_CONNECT_DIALOG", service: "nanocodex-connect-dialog" },
    ],
    main: resolve(configDirectory, baseConfig.main),
    assets: {
      ...baseConfig.assets,
      directory: resolve(configDirectory, baseConfig.assets.directory),
    },
    containers: [{
      ...baseConfig.containers[0],
      image: resolve(currentWebRoot, "container/Dockerfile"),
      image_build_context: resolve(currentWebRoot, "container"),
    }],
    d1_databases: (baseConfig.d1_databases ?? []).map((database) => ({
      ...database,
      ...(d1DatabaseIds === undefined
        ? {}
        : { database_id: resolvedD1DatabaseId(database, d1DatabaseIds) }),
      ...(typeof database.migrations_dir === "string"
        ? { migrations_dir: resolve(configDirectory, database.migrations_dir) }
        : {}),
    })),
  };
}

export function buildWebBootstrapConfig(baseConfig, options = {}) {
  const production = buildWebProductionConfig(baseConfig, options);
  return {
    ...production,
    services: production.services.filter(
      ({ binding }) => binding === "NANOCODEX_CONNECT_DIALOG",
    ),
  };
}

export async function withPrivateRolloutFiles(values, callback, {
  parentDirectory = tmpdir(),
} = {}) {
  const directory = await mkdtemp(join(parentDirectory, "nanocodex-production-rollout-"));
  const paths = { directory };
  try {
    for (const [name, value] of Object.entries(values)) {
      if (!/^[a-z][a-z0-9-]*\.json$/.test(name)) {
        throw new Error(`invalid private rollout filename ${JSON.stringify(name)}`);
      }
      const path = join(directory, name);
      await writeFile(path, `${JSON.stringify(value)}\n`, {
        encoding: "utf8",
        flag: "wx",
        mode: 0o600,
      });
      if (((await stat(path)).mode & 0o777) !== 0o600) {
        throw new Error("private rollout file mode is not 0600");
      }
      paths[name] = path;
    }
    return await callback(paths);
  } finally {
    await rm(directory, { force: true, recursive: true });
  }
}

export async function preflightProductionRollout(environment = process.env) {
  const selection = assertProductionPreflight(environment);
  verifyProductionCheckout(selection.revision);
  const [brokerBase, managedBase, webBase, webAttestation, webSourceConfig] = await Promise.all([
    readJson(brokerConfigPath),
    readJson(managedConfigPath),
    readJson(webArtifactConfigPath),
    readJson(webBuildAttestationPath),
    readFile(webSourceConfigPath),
  ]);
  await verifyManagedWasmArtifact(selection.revision);
  assertWebBuildAttestation(webAttestation, selection.revision, webSourceConfig);
  const broker = buildProductionBrokerConfig(brokerBase, { mainPath: brokerMainPath });
  if (broker.name !== BROKER_NAME || broker.workers_dev !== false || broker.routes !== undefined) {
    throw new Error("production broker must remain the private nanocodex-egress Worker");
  }
  buildManagedProductionConfig(managedBase);
  buildWebProductionConfig(webBase, {
    artifactDirectory: dirname(webArtifactConfigPath),
  });
  const result = {
    components: ["private-broker", "private-managed", "website"],
    revision: selection.revision,
    status: "ready",
  };
  process.stdout.write(`${JSON.stringify(result)}\n`);
  return result;
}

export async function deployProductionManaged(environment = process.env) {
  const cloudflare = cloudflareCredentials(environment);
  const revision = productionRevision(environment.TARGET_SHA);
  verifyProductionCheckout(revision);
  await verifyManagedWasmArtifact(revision);
  const adminToken = requiredSecret(environment, "NANOCODEX_ADMIN_TOKEN");
  const baseConfig = await readJson(managedConfigPath);
  const config = buildManagedProductionConfig(baseConfig);
  const secrets = managedSecretPayload(adminToken);
  const redactions = [cloudflare.apiToken, adminToken];

  await withPrivateRolloutFiles({
    "managed-config.json": config,
    "managed-secrets.json": secrets,
  }, async (paths) => {
    await runWrangler([
      "deploy",
      "--config",
      paths["managed-config.json"],
      "--strict",
      "--tag",
      revision,
      "--message",
      `gakonst/nanocodex@${revision}`,
      "--var",
      `DEPLOYMENT_SHA:${revision}`,
      "--secrets-file",
      paths["managed-secrets.json"],
    ], {
      environment: productionWranglerEnvironment(environment, cloudflare),
      redactions,
    });
  });

  const result = {
    component: "private-managed",
    migrations: MANAGED_DURABLE_OBJECT_MIGRATIONS.map(([tag]) => tag),
    revision,
    status: "deployed",
  };
  process.stdout.write(`${JSON.stringify(result)}\n`);
  return result;
}

export async function verifyProductionBoundary(environment = process.env, {
  fetchImpl = globalThis.fetch,
} = {}) {
  const cloudflare = cloudflareCredentials(environment);
  const revision = productionRevision(environment.TARGET_SHA);
  verifyProductionCheckout(revision);
  const brokerProbeToken = requiredBrokerProbeToken(environment);
  const probeToken = randomBytes(32).toString("base64url");
  const name = `nanocodex-boundary-${revision.slice(0, 12)}-${randomBytes(5).toString("hex")}`;
  const config = buildBoundaryProbeConfig({ name, revision });
  const redactions = [cloudflare.apiToken, brokerProbeToken, probeToken];
  const childEnvironment = productionWranglerEnvironment(environment, cloudflare);
  let deploymentIntent = false;
  let failure;
  let verified;

  try {
    verified = await withPrivateRolloutFiles({
      "probe-config.json": config,
      "probe-secrets.json": {
        NANOCODEX_BROKER_PROBE_TOKEN: brokerProbeToken,
        NANOCODEX_BOUNDARY_PROBE_TOKEN: probeToken,
      },
    }, async (paths) => {
      deploymentIntent = true;
      const output = await runWrangler([
        "deploy",
        "--config",
        paths["probe-config.json"],
        "--strict",
        "--tag",
        revision,
        "--message",
        `gakonst/nanocodex@${revision} private boundary probe`,
        "--secrets-file",
        paths["probe-secrets.json"],
      ], {
        environment: childEnvironment,
        redactions,
      });
      const origin = output.match(/https:\/\/[a-z0-9.-]+\.workers\.dev/i)?.[0];
      if (!origin) throw new Error("Wrangler did not report the boundary probe origin");
      let response;
      let body;
      for (let attempt = 0; attempt < 5; attempt += 1) {
        response = await fetchImpl(new URL("/verify", origin), {
          method: "POST",
          headers: { authorization: `Bearer ${probeToken}` },
          signal: AbortSignal.any([lifecycleAbort.signal, AbortSignal.timeout(30_000)]),
        });
        try {
          body = await boundedJson(response, 8 * 1024);
          break;
        } catch (error) {
          if (response.status !== 404 || attempt === 4) throw error;
          await delay((attempt + 1) * 1_000, undefined, { signal: lifecycleAbort.signal });
        }
      }
      if (response.status !== 200
        || body?.status !== "ok"
        || body?.boundary !== "private-service-binding"
        || body?.broker_ready !== true) {
        throw new Error(`private boundary probe failed with HTTP ${response.status}`);
      }
      return body;
    });
  } catch (error) {
    failure = error;
  } finally {
    if (deploymentIntent) {
      try {
        await runWrangler(["delete", name, "--force"], {
          cleanup: true,
          environment: childEnvironment,
          redactions,
        });
      } catch (error) {
        if (!isMissingWorkerDeleteError(error)) {
          failure = failure
            ? new AggregateError([failure, error], "boundary verification and cleanup failed")
            : error;
        }
      }
    }
  }
  if (failure) throw failure;

  const result = {
    boundary: verified.boundary,
    broker_ready: verified.broker_ready,
    component: "private-broker",
    revision,
    status: "verified",
  };
  process.stdout.write(`${JSON.stringify(result)}\n`);
  return result;
}

export async function deployProductionWeb(environment = process.env, {
  bootstrap = false,
  containersRollout = "none",
  d1DatabaseIds,
} = {}) {
  if (!new Set(["immediate", "none"]).has(containersRollout)) {
    throw new Error("production web container rollout must be immediate or none");
  }
  const cloudflare = cloudflareCredentials(environment);
  const revision = productionRevision(environment.TARGET_SHA);
  verifyProductionCheckout(revision);
  const [baseConfig, attestation, sourceConfig] = await Promise.all([
    readJson(webArtifactConfigPath),
    readJson(webBuildAttestationPath),
    readFile(webSourceConfigPath),
  ]);
  assertWebBuildAttestation(attestation, revision, sourceConfig);
  const configBuilder = bootstrap ? buildWebBootstrapConfig : buildWebProductionConfig;
  const config = configBuilder(baseConfig, {
    artifactDirectory: dirname(webArtifactConfigPath),
    d1DatabaseIds: d1DatabaseIds ?? {},
  });
  const gitMirrorToken = requiredSecret(environment, "NANOCODEX_GIT_TOKEN");
  const redactions = [cloudflare.apiToken, gitMirrorToken];

  await withPrivateRolloutFiles({
    "web-config.json": config,
    "web-secrets.json": webSecretPayload(gitMirrorToken),
  }, async (paths) => {
    await runWrangler([
      "deploy",
      "--config",
      paths["web-config.json"],
      "--strict",
      "--tag",
      revision,
      "--message",
      `gakonst/nanocodex@${revision}`,
      "--containers-rollout",
      containersRollout,
      "--var",
      `DEPLOYMENT_SHA:${revision}`,
      "--secrets-file",
      paths["web-secrets.json"],
    ], {
      cwd: webRoot,
      environment: productionWranglerEnvironment(environment, cloudflare),
      executable: webWranglerPath,
      redactions,
    });
  });

  const result = {
    component: bootstrap ? "website-bootstrap" : "website",
    containers_rollout: containersRollout,
    revision,
    status: "deployed",
  };
  process.stdout.write(`${JSON.stringify(result)}\n`);
  return result;
}

function resolvedD1DatabaseId(database, d1DatabaseIds) {
  const name = database?.database_name;
  if (typeof name !== "string" || name.length === 0) {
    throw new Error("production D1 binding must declare database_name");
  }
  const id = d1DatabaseIds?.[name];
  if (typeof id !== "string"
    || !/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(id)) {
    throw new Error(`production D1 database ${name} must have a reconciled database ID`);
  }
  return id.toLowerCase();
}

function assertTokenStrength(value, name) {
  if (typeof value !== "string" || Buffer.byteLength(value, "utf8") < 32) {
    throw new Error(`${name} must contain at least 32 bytes`);
  }
  if (value.includes("\n") || value.includes("\r")) {
    throw new Error(`${name} must be one line`);
  }
}

function cloudflareCredentials(environment) {
  return {
    accountId: requiredEnvironment(environment, "CLOUDFLARE_ACCOUNT_ID"),
    apiToken: environment.CLOUDFLARE_API_TOKEN === undefined
      ? undefined
      : requiredSecret(environment, "CLOUDFLARE_API_TOKEN"),
  };
}

export function productionWranglerEnvironment(environment, cloudflare) {
  const child = { ...environment };
  for (const name of [
    ...PROVIDER_NAMES,
    ...APPLICATION_SECRET_NAMES,
  ]) delete child[name];
  delete child.CLOUDFLARE_ENV;
  child.CLOUDFLARE_ACCOUNT_ID = cloudflare.accountId;
  if (cloudflare.apiToken) child.CLOUDFLARE_API_TOKEN = cloudflare.apiToken;
  else delete child.CLOUDFLARE_API_TOKEN;
  return child;
}

function runWrangler(arguments_, {
  cleanup = false,
  cwd = workersRoot,
  environment,
  executable = wranglerPath,
  redactions,
}) {
  return runBoundedProcess(process.execPath, [executable, ...arguments_], {
    cwd,
    env: environment,
    label: `production Wrangler ${arguments_[0] ?? "command"}`,
    maxOutputBytes: 64 * 1024,
    redact: (value) => redactSecrets(value, redactions),
    signal: cleanup ? undefined : lifecycleAbort.signal,
    timeoutMs: cleanup ? 60_000 : 180_000,
  });
}

async function boundedJson(response, limit) {
  const encoded = await response.text();
  if (Buffer.byteLength(encoded) > limit) {
    throw new Error(
      `boundary probe returned oversized HTTP ${response.status} ${response.headers.get("content-type") ?? "unknown content"}`,
    );
  }
  try {
    return JSON.parse(encoded);
  } catch {
    throw new Error(
      `boundary probe returned non-JSON HTTP ${response.status} ${response.headers.get("content-type") ?? "unknown content"}`,
    );
  }
}

async function readJson(path) {
  try {
    return JSON.parse(await readFile(path, "utf8"));
  } catch (error) {
    throw new Error(`required production config ${path} is missing or invalid`, { cause: error });
  }
}

function productionRevision(value) {
  const revision = value?.trim();
  if (!revision || !/^[0-9a-f]{40}$/.test(revision)) {
    throw new Error("TARGET_SHA must be the full lowercase production commit SHA");
  }
  return revision;
}

function verifyProductionCheckout(revision) {
  assertProductionCheckout(revision, {
    dirty: git("status", "--porcelain", "--untracked-files=no").length > 0,
    head: git("rev-parse", "HEAD"),
    originMaster: git("rev-parse", "origin/master"),
  });
}

function git(...arguments_) {
  return execFileSync("git", arguments_, {
    cwd: repositoryRoot,
    encoding: "utf8",
  }).trim();
}

function requiredEnvironment(environment, name) {
  const value = environment[name]?.trim();
  if (!value) throw new Error(`${name} is required for production rollout`);
  return value;
}

function requiredSecret(environment, name) {
  const value = environment[name];
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new Error(`${name} is required for production rollout`);
  }
  return value.trim();
}

function requiredBrokerProbeToken(environment) {
  const token = environment.NANOCODEX_BROKER_PROBE_TOKEN;
  if (typeof token !== "string" || token.length < 32 || token.length > 512
    || token.trim() !== token || /[\u0000-\u0020\u007f]/.test(token)) {
    throw new Error(
      "NANOCODEX_BROKER_PROBE_TOKEN must be 32-512 non-whitespace characters without controls",
    );
  }
  return token;
}

function requireConfigured(environment, name) {
  if (environment[name] !== "true") {
    throw new Error(`${name.replace(/_CONFIGURED$/, "")} is required for production rollout`);
  }
}

function requireOptionalConfiguredPair(environment, left, right, label) {
  const leftConfigured = environment[left] === "true";
  const rightConfigured = environment[right] === "true";
  if (leftConfigured !== rightConfigured) {
    throw new Error(`${label} must be configured together for production rollout`);
  }
}

function assertRecord(value, label) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
}

function assertExactService(services, binding, service, label) {
  const matches = services?.filter((candidate) => candidate?.binding === binding);
  if (services?.length !== 1 || matches?.length !== 1 || matches[0].service !== service) {
    throw new Error(`${label} requires ${binding} bound to ${service}`);
  }
}

function assertNoProviderConfiguration(value, label) {
  const encoded = JSON.stringify(value);
  if (/OPENAI_API_KEY|CODEX_OAUTH_BOOTSTRAP|CODEX_RELAY_URL/.test(encoded)) {
    throw new Error(`${label} must not contain provider secret configuration`);
  }
}

const commands = new Map([
  ["preflight", preflightProductionRollout],
  ["deploy-managed", deployProductionManaged],
  ["verify-boundary", verifyProductionBoundary],
  ["deploy-web", deployProductionWeb],
]);
const invoked = process.argv[1]
  ? pathToFileURL(resolve(process.argv[1])).href
  : undefined;
if (invoked === import.meta.url) {
  const arguments_ = process.argv.slice(2);
  const command = commands.get(arguments_[0]);
  if (!command || arguments_.length !== 1) {
    throw new Error("production rollout requires exactly one supported command");
  }
  let termination;
  const terminate = (signal) => {
    if (termination) return;
    termination = signal;
    lifecycleAbort.abort(new Error(`production rollout received ${signal}`));
  };
  const onInterrupt = () => terminate("SIGINT");
  const onTerminate = () => terminate("SIGTERM");
  process.on("SIGINT", onInterrupt);
  process.on("SIGTERM", onTerminate);
  try {
    await command();
  } catch (error) {
    if (!termination) throw error;
  } finally {
    process.off("SIGINT", onInterrupt);
    process.off("SIGTERM", onTerminate);
  }
  if (termination) {
    process.stderr.write(`Production rollout stopped by ${termination}; cleanup completed.\n`);
    process.exitCode = termination === "SIGINT" ? 130 : 143;
  }
}
