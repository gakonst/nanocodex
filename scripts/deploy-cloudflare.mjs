import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHmac } from "node:crypto";
import { readFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { stripVTControlCharacters } from "node:util";

import {
  deployProductionBroker,
  productionBrokerSecrets,
} from "../services/egress/scripts/production-broker.mjs";
import {
  redactSecrets,
  runBoundedProcess,
} from "../services/managed/scripts/child-process.mjs";
import {
  assertProductionCheckout,
  assertProductionPreflight,
  deployProductionManaged,
  deployProductionWeb,
  preflightProductionRollout,
  productionWranglerEnvironment,
  verifyProductionBoundary,
  withPrivateRolloutFiles,
} from "../services/managed/scripts/production-rollout.mjs";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const productionOrigin = JSON.parse(
  await readFile(new URL("../web/production.json", import.meta.url), "utf8"),
).origin;
const abortController = new AbortController();
const AI_SEARCH_NAMESPACE = "default";
const AI_SEARCH_PAGE_SIZE = 100;
const AI_SEARCH_RESOURCE_POLICIES = Object.freeze({
  "nanocodex-history-dev-20260824": Object.freeze({
    namespace: AI_SEARCH_NAMESPACE,
    source: "nanocodex-managed-history",
    type: "r2",
  }),
});

const INSTALL_DIRECTORIES = Object.freeze([
  "js/bindings",
  "js/react",
  "js/artifacts",
  "js/terminal",
  "services/egress",
  "services/managed",
  "web",
  "services/connect-api",
  "web/connect-dialog",
  "web/connect-playground",
]);

const WRANGLER_DIRECTORIES = Object.freeze([
  "services/egress",
  "services/managed",
  "web",
  "services/connect-api",
  "web/connect-dialog",
  "web/connect-playground",
]);

const PRODUCTION_SERVICE_BINDINGS = Object.freeze({
  "connect-api": Object.freeze([
    ["ACCOUNTS", "nanocodex-durable-agent"],
    ["EGRESS", "nanocodex-egress"],
    ["NANOCODEX", "nanocodex"],
  ]),
  "connect-dialog": Object.freeze([]),
  "connect-playground": Object.freeze([]),
  "egress-broker": Object.freeze([]),
  "managed-agent": Object.freeze([["NANOCODEX", "nanocodex-egress"]]),
  website: Object.freeze([
    ["NANOCODEX_BACKEND", "nanocodex-durable-agent"],
    ["NANOCODEX_CONNECT_API", "nanocodex-connect-api"],
    ["NANOCODEX_CONNECT_DIALOG", "nanocodex-connect-dialog"],
  ]),
});

const DEPLOYMENT_SECRET_NAMES = Object.freeze([
  "CLOUDFLARE_API_TOKEN",
  "NANOCODEX_ADMIN_TOKEN",
  "NANOCODEX_BROKER_PROBE_TOKEN",
  "NANOCODEX_CREDENTIAL_ENCRYPTION_KEY",
  "NANOCODEX_CREDENTIAL_ENCRYPTION_KEY_PREVIOUS",
  "NANOCODEX_GIT_TOKEN",
  "NANOCODEX_GITHUB_OAUTH_CLIENT_ID",
  "NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET",
  "NANOCODEX_GOOGLE_OAUTH_CLIENT_ID",
  "NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET",
  "NANOCODEX_X_OAUTH_CLIENT_ID",
  "NANOCODEX_X_OAUTH_CLIENT_SECRET",
  "NANOCODEX_WHOOP_OAUTH_CLIENT_ID",
  "NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET",
]);

export const PRODUCTION_ORIGINS = Object.freeze({
  connectApi: "https://nanocodex-connect-api.gakonst.workers.dev",
  playground: "https://nanocodex-connect-playground.gakonst.workers.dev",
  root: productionOrigin,
});

export function productionMutationPlan(rootExists) {
  assert.equal(typeof rootExists, "boolean", "root existence must be known before mutation");
  return Object.freeze([
    "connect-dialog",
    ...(rootExists ? [] : ["root-bootstrap"]),
    "egress-broker",
    "managed-worker",
    "broker-boundary",
    "connect-api",
    "connect-playground",
    "root-final",
    "repository-publication",
  ]);
}

export function finalContainerRollout(rootExists) {
  assert.equal(typeof rootExists, "boolean", "root existence must be known before deployment");
  return rootExists ? "immediate" : "none";
}

export function preflightEnvironment(environment, revision, {
  oauthAuthenticated = environment.CLOUDFLARE_API_TOKEN === undefined,
} = {}) {
  const configured = (name) => typeof environment[name] === "string"
    && environment[name].trim().length > 0;
  return {
    ...environment,
    TARGET_SHA: revision,
    CLOUDFLARE_API_TOKEN_CONFIGURED: configured("CLOUDFLARE_API_TOKEN") ? "true" : "false",
    CLOUDFLARE_OAUTH_CONFIGURED: oauthAuthenticated ? "true" : "false",
    NANOCODEX_BROKER_PROBE_TOKEN_CONFIGURED: configured("NANOCODEX_BROKER_PROBE_TOKEN") ? "true" : "false",
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY_CONFIGURED: configured("NANOCODEX_CREDENTIAL_ENCRYPTION_KEY") ? "true" : "false",
    NANOCODEX_GIT_TOKEN_CONFIGURED: configured("NANOCODEX_GIT_TOKEN") ? "true" : "false",
    NANOCODEX_GITHUB_OAUTH_CLIENT_ID_CONFIGURED: configured("NANOCODEX_GITHUB_OAUTH_CLIENT_ID") ? "true" : "false",
    NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET_CONFIGURED: configured("NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET") ? "true" : "false",
    NANOCODEX_GOOGLE_OAUTH_CLIENT_ID_CONFIGURED: configured("NANOCODEX_GOOGLE_OAUTH_CLIENT_ID") ? "true" : "false",
    NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET_CONFIGURED: configured("NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET") ? "true" : "false",
    NANOCODEX_X_OAUTH_CLIENT_ID_CONFIGURED: configured("NANOCODEX_X_OAUTH_CLIENT_ID") ? "true" : "false",
    NANOCODEX_X_OAUTH_CLIENT_SECRET_CONFIGURED: configured("NANOCODEX_X_OAUTH_CLIENT_SECRET") ? "true" : "false",
    NANOCODEX_WHOOP_OAUTH_CLIENT_ID_CONFIGURED: configured("NANOCODEX_WHOOP_OAUTH_CLIENT_ID") ? "true" : "false",
    NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET_CONFIGURED: configured("NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET") ? "true" : "false",
  };
}

export function normalizeDeploymentEnvironment(environment) {
  const normalized = { ...environment };
  const aliases = {
    NANOCODEX_GIT_TOKEN: "GIT_MIRROR_TOKEN",
    NANOCODEX_GITHUB_OAUTH_CLIENT_ID: "GH_CLIENT_ID",
    NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET: "GH_CLIENT_SECRETS",
    NANOCODEX_GOOGLE_OAUTH_CLIENT_ID: "GOOGLE_CLIENT_ID",
    NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET: "GOOGLE_CLIENT_SECRET",
    NANOCODEX_X_OAUTH_CLIENT_ID: "X_CLIENT_ID",
    NANOCODEX_X_OAUTH_CLIENT_SECRET: "X_CLIENT_SECRET",
    NANOCODEX_WHOOP_OAUTH_CLIENT_ID: "WHOOP_CLIENT_ID",
    NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET: "WHOOP_CLIENT_SECRET",
  };
  for (const [canonical, alias] of Object.entries(aliases)) {
    if (!configured(normalized[canonical]) && configured(normalized[alias])) {
      normalized[canonical] = normalized[alias];
    }
  }

  const master = normalized.SESSION_CREDENTIAL_KEY;
  const derived = [
    ["NANOCODEX_ADMIN_TOKEN", "admin-token-v1"],
    ["NANOCODEX_BROKER_PROBE_TOKEN", "broker-probe-token-v1"],
    ["NANOCODEX_CREDENTIAL_ENCRYPTION_KEY", "credential-encryption-key-v1"],
  ];
  if (derived.some(([name]) => !configured(normalized[name]))) {
    if (!/^[A-Za-z0-9_-]{43}$/.test(master ?? "")
      || Buffer.from(master, "base64url").length !== 32) {
      throw new Error(
        "set the missing Nanocodex production secrets or provide a 32-byte SESSION_CREDENTIAL_KEY",
      );
    }
    for (const [name, scope] of derived) {
      if (!configured(normalized[name])) {
        normalized[name] = createHmac("sha256", Buffer.from(master, "base64url"))
          .update(`nanocodex-production-rollout:${scope}`)
          .digest("base64url");
      }
    }
  }
  return normalized;
}

export function cloudflareAccountId(configuredAccountId, whoami) {
  if (configured(configuredAccountId)) return configuredAccountId.trim();
  const accounts = whoami?.loggedIn === true && Array.isArray(whoami.accounts)
    ? whoami.accounts.filter(({ id } = {}) => typeof id === "string" && /^[0-9a-f]{32}$/i.test(id))
    : [];
  if (accounts.length !== 1) {
    throw new Error(
      accounts.length === 0
        ? "authenticated Wrangler did not expose a Cloudflare account"
        : "authenticated Wrangler has multiple Cloudflare accounts; set CLOUDFLARE_ACCOUNT_ID",
    );
  }
  return accounts[0].id;
}

export function assertOneCommandPreflight(environment, checkout) {
  const revision = environment.TARGET_SHA;
  assertProductionCheckout(revision, checkout);
  productionBrokerSecrets(environment);
  assertProductionPreflight(preflightEnvironment(environment, revision));
  return revision;
}

export function assertPinnedWrangler(packageJson, packageLock, installedPackage, label) {
  const declared = packageJson?.devDependencies?.wrangler;
  const locked = packageLock?.packages?.["node_modules/wrangler"]?.version;
  const installed = installedPackage?.version;
  if (typeof declared !== "string" || typeof locked !== "string" || typeof installed !== "string") {
    throw new Error(`${label} must have a checked-in and installed Wrangler dependency`);
  }
  if (locked !== installed) {
    throw new Error(`${label} installed Wrangler ${installed} does not match package-lock ${locked}`);
  }
  return installed;
}

export function productionResourceTopology(configurations) {
  if (!Array.isArray(configurations) || configurations.length === 0) {
    throw new Error("production resource topology requires at least one Wrangler config");
  }
  const r2Buckets = new Set();
  const d1DatabaseNames = new Set();
  const d1Migrations = [];
  const aiSearchInstances = new Map();

  for (const configuration of configurations) {
    const label = requiredText(configuration?.label, "Wrangler config label");
    const directory = resolve(requiredText(
      configuration?.directory,
      `${label} Wrangler config directory`,
    ));
    const config = configuration?.config;
    if (!config || typeof config !== "object" || Array.isArray(config)) {
      throw new Error(`${label} Wrangler config must be an object`);
    }

    for (const bucket of optionalArray(config.r2_buckets, `${label} r2_buckets`)) {
      requiredBinding(bucket?.binding, `${label} R2 binding`);
      r2Buckets.add(requiredResourceName(bucket?.bucket_name, `${label} R2 bucket`));
    }
    for (const database of optionalArray(config.d1_databases, `${label} d1_databases`)) {
      const binding = requiredBinding(database?.binding, `${label} D1 binding`);
      const databaseName = requiredResourceName(
        database?.database_name,
        `${label} D1 database`,
      );
      if (database.database_id !== undefined) {
        requiredD1DatabaseId(database.database_id, `${label} D1 database_id`);
      }
      d1DatabaseNames.add(databaseName);
      if (database.migrations_dir !== undefined) {
        const migrationsDir = resolve(directory, requiredText(
          database.migrations_dir,
          `${label} D1 migrations_dir`,
        ));
        const migrationKey = `${databaseName}\0${migrationsDir}`;
        if (!d1Migrations.some((migration) => migration.key === migrationKey)) {
          d1Migrations.push({
            binding,
            compatibilityDate: requiredText(
              config.compatibility_date,
              `${label} compatibility_date`,
            ),
            database: { ...database },
            databaseName,
            key: migrationKey,
            label,
            migrationsDir,
          });
        }
      }
    }
    for (const instance of optionalArray(config.ai_search, `${label} ai_search`)) {
      requiredBinding(instance?.binding, `${label} AI Search binding`);
      const name = requiredResourceName(
        instance?.instance_name,
        `${label} AI Search instance`,
      );
      const policy = AI_SEARCH_RESOURCE_POLICIES[name];
      if (!policy) {
        throw new Error(`${label} AI Search instance ${name} has no production source policy`);
      }
      aiSearchInstances.set(name, Object.freeze({ name, ...policy }));
    }
  }

  for (const instance of aiSearchInstances.values()) {
    if (instance.type === "r2" && !r2Buckets.has(instance.source)) {
      throw new Error(
        `production AI Search instance ${instance.name} references missing R2 bucket ${instance.source}`,
      );
    }
  }

  return Object.freeze({
    aiSearchInstances: Object.freeze(
      [...aiSearchInstances.values()].sort((left, right) => left.name.localeCompare(right.name)),
    ),
    d1DatabaseNames: Object.freeze([...d1DatabaseNames].sort()),
    d1Migrations: Object.freeze(d1Migrations.map(({ key: _key, ...migration }) => (
      Object.freeze(migration)
    ))),
    r2Buckets: Object.freeze([...r2Buckets].sort()),
  });
}

/**
 * Ensures the checked-in Worker configs describe the same deployable service
 * graph as the rollout. Wrangler only discovers a bad binding when that
 * individual Worker deploys, which is too late for a one-command preflight.
 */
export function assertProductionServiceBindings(configurations) {
  if (!Array.isArray(configurations) || configurations.length === 0) {
    throw new Error("production service binding topology requires Wrangler configs");
  }
  const seen = new Set();
  for (const configuration of configurations) {
    const label = requiredText(configuration?.label, "Worker config label");
    if (seen.has(label)) throw new Error(`production service binding topology repeats ${label}`);
    seen.add(label);
    const expected = PRODUCTION_SERVICE_BINDINGS[label];
    if (!expected) throw new Error(`production service binding topology does not recognize ${label}`);
    const actual = optionalArray(configuration?.config?.services, `${label} services`)
      .map((service) => {
        const binding = requiredBinding(service?.binding, `${label} service binding`);
        const target = requiredResourceName(service?.service, `${label} service target`);
        return [binding, target];
      })
      .sort(([left], [right]) => left.localeCompare(right));
    const wanted = [...expected].sort(([left], [right]) => left.localeCompare(right));
    if (JSON.stringify(actual) !== JSON.stringify(wanted)) {
      throw new Error(
        `production ${label} service bindings must be ${JSON.stringify(wanted)}`,
      );
    }
  }
  for (const label of Object.keys(PRODUCTION_SERVICE_BINDINGS)) {
    if (!seen.has(label)) throw new Error(`production service binding topology is missing ${label}`);
  }
}

export function parseR2BucketList(output) {
  const clean = stripVTControlCharacters(String(output));
  const names = [...clean.matchAll(/(?:^|\n)name:\s*([^\r\n]+)(?=\r?\n|$)/g)]
    .map((match) => requiredResourceName(match[1].trim(), "listed R2 bucket"));
  return uniqueListedResources(names, "R2 bucket");
}

export function parseD1DatabaseList(output) {
  const databases = parseJsonArray(output, "D1 database list");
  const byName = new Map();
  for (const database of databases) {
    const name = requiredListedName(database?.name, "listed D1 database");
    const id = requiredD1DatabaseId(database?.uuid, `listed D1 database ${name}`);
    if (byName.has(name)) throw new Error(`D1 database list contains duplicate name ${name}`);
    byName.set(name, Object.freeze({ id, name }));
  }
  return byName;
}

export function parseAiSearchList(output) {
  const instances = parseJsonArray(output, "AI Search instance list");
  const byName = new Map();
  for (const instance of instances) {
    const name = requiredListedName(instance?.id, "listed AI Search instance");
    if (byName.has(name)) {
      throw new Error(`AI Search instance list contains duplicate name ${name}`);
    }
    byName.set(name, Object.freeze({
      name,
      namespace: instance.namespace ?? AI_SEARCH_NAMESPACE,
      source: instance.source,
      type: instance.type ?? "builtin",
    }));
  }
  return byName;
}

export async function reconcileProductionResources(topology, { run }) {
  if (typeof run !== "function") throw new Error("production resource runner is required");
  const created = { aiSearch: [], d1: [], r2: [] };
  let [r2Buckets, d1Databases, aiSearchInstances] = await Promise.all([
    listR2Buckets(run),
    listD1Databases(run),
    listAiSearchInstances(run),
  ]);

  for (const name of topology.r2Buckets) {
    if (r2Buckets.has(name)) continue;
    const createdHere = await createAndAdopt({
      create: () => run(["r2", "bucket", "create", name], {
        label: `create production R2 bucket ${name}`,
      }),
      inspect: () => listR2Buckets(run),
      kind: "R2 bucket",
      name,
      select: (resources) => resources.has(name),
    });
    r2Buckets = createdHere.resources;
    if (createdHere.created) created.r2.push(name);
  }

  for (const expected of topology.aiSearchInstances) {
    const existing = aiSearchInstances.get(expected.name);
    if (existing) {
      assertExpectedAiSearch(existing, expected);
      continue;
    }
    if (expected.type === "r2" && !r2Buckets.has(expected.source)) {
      throw new Error(
        `production AI Search source bucket ${expected.source} was not reconciled`,
      );
    }
    const createdHere = await createAndAdopt({
      create: () => run([
        "ai-search",
        "create",
        expected.name,
        "--namespace",
        expected.namespace,
        "--type",
        expected.type,
        "--source",
        expected.source,
        "--json",
      ], { label: `create production AI Search instance ${expected.name}` }),
      inspect: () => listAiSearchInstances(run),
      kind: "AI Search instance",
      name: expected.name,
      select: (resources) => {
        const instance = resources.get(expected.name);
        if (!instance) return false;
        assertExpectedAiSearch(instance, expected);
        return true;
      },
    });
    aiSearchInstances = createdHere.resources;
    if (createdHere.created) created.aiSearch.push(expected.name);
  }

  for (const name of topology.d1DatabaseNames) {
    if (d1Databases.has(name)) continue;
    const createdHere = await createAndAdopt({
      create: () => run(["d1", "create", name], {
        label: `create production D1 database ${name}`,
      }),
      inspect: () => listD1Databases(run),
      kind: "D1 database",
      name,
      select: (resources) => resources.has(name),
    });
    d1Databases = createdHere.resources;
    if (createdHere.created) created.d1.push(name);
  }

  const d1DatabaseIds = {};
  for (const name of topology.d1DatabaseNames) {
    const database = d1Databases.get(name);
    if (!database) throw new Error(`production D1 database ${name} was not reconciled`);
    d1DatabaseIds[name] = database.id;
  }
  return Object.freeze({
    created: Object.freeze({
      aiSearch: Object.freeze(created.aiSearch),
      d1: Object.freeze(created.d1),
      r2: Object.freeze(created.r2),
    }),
    d1DatabaseIds: Object.freeze(d1DatabaseIds),
  });
}

export function productionD1MigrationConfig(migration, d1DatabaseIds) {
  const databaseId = requiredD1DatabaseId(
    d1DatabaseIds?.[migration.databaseName],
    `resolved D1 database ${migration.databaseName}`,
  );
  return {
    name: "nanocodex-production-d1-migrations",
    compatibility_date: migration.compatibilityDate,
    d1_databases: [{
      ...migration.database,
      database_id: databaseId,
      migrations_dir: migration.migrationsDir,
    }],
  };
}

export async function applyProductionD1Migrations(topology, d1DatabaseIds, {
  run,
  withConfig,
}) {
  if (typeof run !== "function") throw new Error("production resource runner is required");
  if (typeof withConfig !== "function") throw new Error("temporary D1 config writer is required");
  for (const migration of topology.d1Migrations) {
    const config = productionD1MigrationConfig(migration, d1DatabaseIds);
    await withConfig(config, (configPath) => run([
      "d1",
      "migrations",
      "apply",
      migration.binding,
      "--remote",
      "--config",
      configPath,
    ], {
      label: `apply production D1 migrations for ${migration.databaseName}`,
      timeoutMs: 10 * 60_000,
    }));
  }
}

export async function prepareProductionResources(topology, options) {
  const resources = await reconcileProductionResources(topology, options);
  await applyProductionD1Migrations(topology, resources.d1DatabaseIds, options);
  return resources;
}

async function listR2Buckets(run) {
  const output = await run(["r2", "bucket", "list"], {
    label: "list production R2 buckets",
  });
  return parseR2BucketList(output);
}

async function listD1Databases(run) {
  const output = await run(["d1", "list", "--json"], {
    label: "list production D1 databases",
  });
  return parseD1DatabaseList(output);
}

async function listAiSearchInstances(run) {
  const instances = new Map();
  for (let page = 1; page <= 1_000; page += 1) {
    const output = await run([
      "ai-search",
      "list",
      "--namespace",
      AI_SEARCH_NAMESPACE,
      "--page",
      String(page),
      "--per-page",
      String(AI_SEARCH_PAGE_SIZE),
      "--json",
    ], { label: `list production AI Search instances page ${page}` });
    const pageInstances = parseAiSearchList(output);
    for (const [name, instance] of pageInstances) {
      if (instances.has(name)) {
        throw new Error(`AI Search instance list contains duplicate name ${name}`);
      }
      instances.set(name, instance);
    }
    if (pageInstances.size < AI_SEARCH_PAGE_SIZE) return instances;
  }
  throw new Error("AI Search instance listing exceeded 1000 pages");
}

async function createAndAdopt({ create, inspect, kind, name, select }) {
  let creationFailure;
  try {
    await create();
  } catch (error) {
    creationFailure = error;
  }
  let resources;
  try {
    resources = await inspect();
  } catch (inspectionFailure) {
    if (creationFailure) {
      throw new AggregateError(
        [creationFailure, inspectionFailure],
        `could not provision or inspect production ${kind} ${name}`,
      );
    }
    throw inspectionFailure;
  }
  if (!select(resources)) {
    throw new Error(`production ${kind} ${name} could not be provisioned or adopted`, {
      cause: creationFailure,
    });
  }
  return { created: creationFailure === undefined, resources };
}

function assertExpectedAiSearch(instance, expected) {
  if (instance.namespace !== expected.namespace
    || instance.type !== expected.type
    || instance.source !== expected.source) {
    throw new Error(
      `production AI Search instance ${instance.name} must be ${expected.type}`
        + ` in namespace ${expected.namespace} with source ${expected.source}`,
    );
  }
}

function parseJsonArray(output, label) {
  let value;
  try {
    value = JSON.parse(stripVTControlCharacters(String(output)).trim());
  } catch (error) {
    throw new Error(`${label} did not return clean JSON`, { cause: error });
  }
  if (!Array.isArray(value)) throw new Error(`${label} must be a JSON array`);
  return value;
}

function uniqueListedResources(names, label) {
  const unique = new Set();
  for (const name of names) {
    if (unique.has(name)) throw new Error(`${label} list contains duplicate name ${name}`);
    unique.add(name);
  }
  return unique;
}

function optionalArray(value, label) {
  if (value === undefined) return [];
  if (!Array.isArray(value)) throw new Error(`${label} must be an array`);
  return value;
}

function requiredText(value, label) {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new Error(`${label} must be a non-empty string`);
  }
  return value.trim();
}

function requiredBinding(value, label) {
  const binding = requiredText(value, label);
  if (!/^[A-Za-z_$][A-Za-z0-9_$]*$/.test(binding)) {
    throw new Error(`${label} must be a JavaScript identifier`);
  }
  return binding;
}

function requiredResourceName(value, label) {
  const name = requiredText(value, label);
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/.test(name)) {
    throw new Error(`${label} has an unsupported name`);
  }
  return name;
}

function requiredListedName(value, label) {
  const name = requiredText(value, label);
  if (name.length > 512 || /[\u0000-\u001f\u007f]/.test(name)) {
    throw new Error(`${label} has an unsupported name`);
  }
  return name;
}

function requiredD1DatabaseId(value, label) {
  const id = requiredText(value, label);
  if (!/^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/i.test(id)) {
    throw new Error(`${label} must be a D1 database UUID`);
  }
  return id.toLowerCase();
}

export async function executeProductionMutations(rootExists, actions) {
  const plan = productionMutationPlan(rootExists);
  for (const component of plan) {
    const action = actions[component];
    if (typeof action !== "function") throw new Error(`missing rollout action for ${component}`);
    try {
      await action();
    } catch (error) {
      throw new Error(
        `Cloudflare rollout stopped at ${component}; no later component was deployed`,
        { cause: error },
      );
    }
  }
  return plan;
}

export async function runProductionPhases({
  preflight,
  prepare,
  rootExists,
  actions,
  health,
}) {
  await preflight();
  await prepare();
  const exists = await rootExists();
  const plan = await executeProductionMutations(exists, actions);
  await health();
  return plan;
}

export function assertLiveResponse(probe, response, body, revision) {
  if (probe === "root-health") {
    assert.equal(response.status, 200, "production root health must return HTTP 200");
    assert.equal(body?.status, "ok", "production root health must report ok");
    assert.equal(body?.deployment_sha, revision, "production root must report the deployed SHA");
    return;
  }
  if (probe === "managed-binding") {
    assert.equal(response.status, 200, "production managed account boundary must return HTTP 200");
    assert.notEqual(body?.error, "managed_service_unavailable", "production managed Service Binding is unavailable");
    assert.equal(typeof body?.user?.id, "string", "production managed account boundary must return an account");
    assert.equal(typeof body?.organization?.id, "string", "production managed account boundary must return an organization");
    return;
  }
  if (probe === "connect-api") {
    assert.equal(response.status, 200, "production Connect API health must return HTTP 200");
    assert.deepEqual(body, { status: "ok", mode: "live" });
    return;
  }
  if (probe === "repository") {
    assert.equal(response.status, 200, "production repository snapshot must return HTTP 200");
    assert.equal(
      body?.repository?.head,
      revision,
      "production repository snapshot must report the deployed SHA",
    );
    return;
  }
  if (probe === "repository-git") {
    assert.equal(response.status, 200, "production Git smart HTTP must return HTTP 200");
    assert.match(
      response.headers.get("content-type") ?? "",
      /^application\/x-git-upload-pack-result\b/,
      "production Git smart HTTP must return an upload-pack result",
    );
    assert.match(
      body,
      new RegExp(`${revision} refs/heads/master`),
      "production Git smart HTTP must advertise the deployed SHA",
    );
    return;
  }
  if (probe === "root-connect-dialog" || probe === "root-connect-device" || probe === "connect-playground") {
    assert.equal(response.status, 200, `${probe} must return HTTP 200`);
    assert.match(
      response.headers.get("content-type") ?? "",
      /^text\/html\b/,
      `${probe} must return HTML`,
    );
    return;
  }
  throw new Error(`unknown production health probe ${probe}`);
}

export function productionProbeMaxBytes(probe) {
  return probe === "repository" ? 8 * 1024 * 1024 : 64 * 1024;
}

export function productionProbeHeaders(probe, encoding, existing = {}) {
  return {
    ...existing,
    accept: probe === "repository-git"
      ? "application/x-git-upload-pack-result"
      : encoding === "json" ? "application/json" : "text/html",
    ...(probe === "root-connect-device" ? {
      "sec-fetch-dest": "document",
      "sec-fetch-mode": "navigate",
    } : {}),
  };
}

async function main(environment = process.env) {
  requireLocalTool("node", ["--version"]);
  requireLocalTool("npm", ["--version"]);
  requireLocalTool("cargo", ["--version"]);
  requireLocalTool("wasm-bindgen", ["--version"]);

  git("fetch", "--quiet", "origin", "master");
  const revision = git("rev-parse", "HEAD");
  const target = environment.TARGET_SHA?.trim() || revision;
  let rolloutEnvironment = normalizeDeploymentEnvironment({
    ...environment,
    TARGET_SHA: target,
  });
  const resourceTopology = await loadProductionResourceTopology();

  await installPinnedDependencies(rolloutEnvironment);
  await assertPinnedWranglerInstallations();
  rolloutEnvironment = await resolveCloudflareAccountEnvironment(rolloutEnvironment);
  assertOneCommandPreflight(rolloutEnvironment, checkoutState());
  await verifyCloudflareAuthentication(rolloutEnvironment);
  await buildProductionArtifacts(rolloutEnvironment);
  assertOneCommandPreflight(rolloutEnvironment, checkoutState());
  await preflightProductionRollout(preflightEnvironment(rolloutEnvironment, target));

  const cloudflare = {
    accountId: rolloutEnvironment.CLOUDFLARE_ACCOUNT_ID,
    apiToken: rolloutEnvironment.CLOUDFLARE_API_TOKEN,
  };
  const childEnvironment = {
    ...productionWranglerEnvironment(rolloutEnvironment, cloudflare),
    CI: "true",
    NO_COLOR: "1",
  };
  const redactions = deploymentSecrets(rolloutEnvironment);
  const runResourceCommand = (arguments_, {
    label,
    timeoutMs = 60_000,
  }) => runWrangler("web", arguments_, {
    environment: childEnvironment,
    label,
    redactions,
    timeoutMs,
  });
  const resources = await prepareProductionResources(resourceTopology, {
    run: runResourceCommand,
    withConfig: (config, callback) => withPrivateRolloutFiles({
      "d1-migrations.json": config,
    }, (paths) => callback(paths["d1-migrations.json"])),
  });
  assertOneCommandPreflight(rolloutEnvironment, checkoutState());
  const rootExists = await productionWorkerExists(childEnvironment, redactions);

  const actions = {
    "connect-dialog": () => deployConfiguredWorker({
      component: "connect-dialog",
      config: "wrangler.jsonc",
      directory: "web/connect-dialog",
      environment: childEnvironment,
      redactions,
      revision: target,
      extraArguments: ["--autoconfig=false"],
    }),
    "root-bootstrap": () => deployProductionWeb(rolloutEnvironment, {
      bootstrap: true,
      containersRollout: "immediate",
      d1DatabaseIds: resources.d1DatabaseIds,
    }),
    "egress-broker": () => deployProductionBroker(rolloutEnvironment),
    "managed-worker": () => deployProductionManaged(rolloutEnvironment),
    "broker-boundary": () => verifyProductionBoundary(rolloutEnvironment),
    "connect-api": () => deployConfiguredWorker({
      component: "connect-api",
      config: "wrangler.jsonc",
      directory: "services/connect-api",
      environment: childEnvironment,
      redactions,
      revision: target,
    }),
    "connect-playground": () => deployConfiguredWorker({
      component: "connect-playground",
      config: "wrangler.jsonc",
      directory: "web/connect-playground",
      environment: childEnvironment,
      redactions,
      revision: target,
    }),
    "root-final": () => deployProductionWeb(rolloutEnvironment, {
      containersRollout: finalContainerRollout(rootExists),
      d1DatabaseIds: resources.d1DatabaseIds,
    }),
    "repository-publication": () => runLocal(
      process.execPath,
      [resolve(repositoryRoot, "web/scripts/publish-repository.mjs")],
      {
        environment: {
          ...rolloutEnvironment,
          NANOCODEX_GIT_ORIGIN: PRODUCTION_ORIGINS.root,
          NANOCODEX_REPO: repositoryRoot,
        },
        label: "publish production repository generation",
        timeoutMs: 10 * 60_000,
      },
    ),
  };

  const plan = await executeProductionMutations(rootExists, actions);
  await waitForProductionHealth(target);
  process.stdout.write(`${JSON.stringify({
    plan,
    resources_created: resources.created,
    revision: target,
    root_bootstrapped: !rootExists,
    status: "healthy",
  })}\n`);
}

async function loadProductionResourceTopology() {
  const definitions = [
    ["website", "web", "web/wrangler.jsonc"],
    ["managed-agent", "services/managed", "services/managed/wrangler.jsonc"],
    ["egress-broker", "services/egress", "services/egress/wrangler.broker.jsonc"],
    ["connect-api", "services/connect-api", "services/connect-api/wrangler.jsonc"],
    ["connect-dialog", "web/connect-dialog", "web/connect-dialog/wrangler.jsonc"],
    ["connect-playground", "web/connect-playground", "web/connect-playground/wrangler.jsonc"],
  ];
  const configurations = await Promise.all(definitions.map(async ([label, directory, path]) => ({
    config: await readJson(resolve(repositoryRoot, path)),
    directory: resolve(repositoryRoot, directory),
    label,
  })));
  assertProductionServiceBindings(configurations);
  return productionResourceTopology(configurations.filter(
    ({ label }) => label === "website" || label === "managed-agent",
  ));
}

async function installPinnedDependencies(environment) {
  const child = buildEnvironment(environment);
  for (const directory of INSTALL_DIRECTORIES) {
    await runLocal("npm", ["ci", "--ignore-scripts", "--prefix", directory], {
      environment: child,
      label: `install ${directory}`,
      timeoutMs: 10 * 60_000,
    });
  }
}

async function assertPinnedWranglerInstallations() {
  for (const directory of WRANGLER_DIRECTORIES) {
    const [packageJson, packageLock, installed] = await Promise.all([
      readJson(resolve(repositoryRoot, directory, "package.json")),
      readJson(resolve(repositoryRoot, directory, "package-lock.json")),
      readJson(resolve(repositoryRoot, directory, "node_modules/wrangler/package.json")),
    ]);
    assertPinnedWrangler(packageJson, packageLock, installed, directory);
  }
}

async function verifyCloudflareAuthentication(environment) {
  const cloudflare = {
    accountId: environment.CLOUDFLARE_ACCOUNT_ID,
    apiToken: environment.CLOUDFLARE_API_TOKEN,
  };
  await runWrangler("web", ["whoami", "--account", cloudflare.accountId, "--json"], {
    environment: productionWranglerEnvironment(environment, cloudflare),
    label: "Cloudflare authentication preflight",
    redactions: deploymentSecrets(environment),
    timeoutMs: 60_000,
  });
}

async function resolveCloudflareAccountEnvironment(environment) {
  if (configured(environment.CLOUDFLARE_ACCOUNT_ID)) return environment;
  const output = await runWrangler("web", ["whoami", "--json"], {
    environment: buildEnvironment(environment),
    label: "Cloudflare account discovery",
    redactions: deploymentSecrets(environment),
    timeoutMs: 60_000,
  });
  let whoami;
  try { whoami = JSON.parse(stripVTControlCharacters(output)); } catch (error) {
    throw new Error("authenticated Wrangler returned invalid account metadata", { cause: error });
  }
  return {
    ...environment,
    CLOUDFLARE_ACCOUNT_ID: cloudflareAccountId(undefined, whoami),
  };
}

async function buildProductionArtifacts(environment) {
  const child = buildEnvironment(environment);
  await runLocal("./scripts/build-js-package.sh", [], {
    environment: child,
    label: "build production WASM",
    timeoutMs: 30 * 60_000,
  });
  await runLocal("npm", ["run", "build", "--prefix", "js/artifacts"], {
    environment: child,
    label: "build artifacts package",
  });
  await runLocal("npm", ["run", "build", "--prefix", "js/terminal"], {
    environment: child,
    label: "build terminal package",
  });
  await runLocal("npm", ["run", "prepare:code-evaluator", "--prefix", "services/managed"], {
    environment: child,
    label: "build managed evaluator",
  });
  await runLocal("npm", ["run", "build:from-wasm", "--prefix", "web"], {
    environment: child,
    label: "build production website",
    timeoutMs: 10 * 60_000,
  });
  await runLocal("npm", ["run", "build", "--prefix", "web/connect-dialog"], {
    environment: child,
    label: "build Connect dialog",
    timeoutMs: 10 * 60_000,
  });
  await runLocal("npm", ["run", "build", "--prefix", "web/connect-playground"], {
    environment: child,
    label: "build Connect playground",
    timeoutMs: 10 * 60_000,
  });
  await runLocal("npm", [
    "exec",
    "--prefix",
    "services/connect-api",
    "--",
    "tsc",
    "--noEmit",
    "--project",
    "services/connect-api/tsconfig.json",
  ], {
    environment: child,
    label: "type-check Connect API",
  });
}

async function productionWorkerExists(environment, redactions) {
  try {
    await runWrangler("web", ["deployments", "status", "--name", "nanocodex", "--json"], {
      environment,
      label: "inspect production root Worker",
      redactions,
      timeoutMs: 60_000,
    });
    return true;
  } catch (error) {
    if (/\[(?:code: )?10007\]/i.test(String(error))) {
      return false;
    }
    throw new Error("could not determine whether the production root Worker needs bootstrap", {
      cause: error,
    });
  }
}

async function deployConfiguredWorker({
  component,
  config,
  directory,
  environment,
  extraArguments = [],
  redactions,
  revision,
}) {
  assert.match(revision, /^[0-9a-f]{40}$/, "deployment revision must be a full Git SHA");
  assertProductionCheckout(revision, checkoutState());
  await runWrangler(directory, [
    "deploy",
    "--config",
    config,
    "--strict",
    "--tag",
    revision,
    "--message",
    `gakonst/nanocodex@${revision}`,
    "--var",
    `DEPLOYMENT_SHA:${revision}`,
    ...extraArguments,
  ], {
    environment,
    label: `deploy production ${component}`,
    redactions,
    timeoutMs: 180_000,
  });
}

async function waitForProductionHealth(revision, fetchImpl = globalThis.fetch) {
  const attempts = 30;
  let failure;
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    try {
      const probes = [
        ["root-health", new URL("/api/health", PRODUCTION_ORIGINS.root), "json"],
        ["managed-binding", new URL("/v1/me", PRODUCTION_ORIGINS.root), "json"],
        ["connect-api", new URL("/healthz", PRODUCTION_ORIGINS.connectApi), "json"],
        ["repository", new URL("/api/repository/snapshot", PRODUCTION_ORIGINS.root), "json"],
        [
          "repository-git",
          new URL(`/git/${revision}/git-upload-pack`, PRODUCTION_ORIGINS.root),
          "text",
          {
            method: "POST",
            headers: {
              "content-type": "application/x-git-upload-pack-request",
              "git-protocol": "version=2",
            },
            body: Buffer.concat([
              gitPacketLine("command=ls-refs\n"),
              Buffer.from("0001"),
              gitPacketLine("ref-prefix refs/heads/\n"),
              Buffer.from("0000"),
            ]),
          },
        ],
        ["root-connect-device", new URL("/connect/device", PRODUCTION_ORIGINS.root), "text"],
        ["root-connect-dialog", new URL("/connect-dialog/", PRODUCTION_ORIGINS.root), "text"],
        ["connect-playground", new URL("/", PRODUCTION_ORIGINS.playground), "text"],
      ];
      await Promise.all(probes.map(async ([probe, url, encoding, init]) => {
        url.searchParams.set("revision", revision);
        url.searchParams.set("rollout_attempt", String(attempt));
        const response = await fetchImpl(url, {
          ...init,
          cache: "no-store",
          headers: productionProbeHeaders(probe, encoding, init?.headers),
          signal: AbortSignal.any([abortController.signal, AbortSignal.timeout(5_000)]),
        });
        const encoded = await response.text();
        const maxBytes = productionProbeMaxBytes(probe);
        if (Buffer.byteLength(encoded) > maxBytes) {
          throw new Error(`${probe} returned more than ${maxBytes} bytes`);
        }
        let body = encoded;
        if (encoding === "json") {
          try {
            body = JSON.parse(encoded);
          } catch {
            throw new Error(`${probe} returned non-JSON HTTP ${response.status}`);
          }
        }
        assertLiveResponse(probe, response, body, revision);
      }));
      return;
    } catch (error) {
      failure = error;
      if (attempt + 1 < attempts) await new Promise((resolvePromise) => setTimeout(resolvePromise, 2_000));
    }
  }
  throw new Error(`production health did not converge for ${revision}`, { cause: failure });
}

function gitPacketLine(payload) {
  const body = Buffer.from(payload);
  return Buffer.concat([
    Buffer.from((body.length + 4).toString(16).padStart(4, "0")),
    body,
  ]);
}

function checkoutState() {
  return {
    dirty: git("status", "--porcelain", "--untracked-files=normal").length > 0,
    head: git("rev-parse", "HEAD"),
    originMaster: git("rev-parse", "origin/master"),
  };
}

function buildEnvironment(environment) {
  const child = { ...environment };
  for (const name of DEPLOYMENT_SECRET_NAMES) delete child[name];
  for (const name of [
    "OPENAI_API_KEY",
    "CODEX_OAUTH_BOOTSTRAP",
    "LOCAL_CHATGPT_BOOTSTRAP",
    "NANOCODEX_MANAGED_CODEX_OAUTH_BOOTSTRAP",
    "NANOCODEX_MANAGED_OPENAI_API_KEY",
  ]) delete child[name];
  return child;
}

function deploymentSecrets(environment) {
  return DEPLOYMENT_SECRET_NAMES.map((name) => environment[name]).filter(Boolean);
}

function configured(value) {
  return typeof value === "string" && value.trim().length > 0;
}

function runWrangler(directory, arguments_, options) {
  const executable = resolve(repositoryRoot, directory, "node_modules/wrangler/bin/wrangler.js");
  return runBoundedProcess(process.execPath, [executable, ...arguments_], {
    cwd: resolve(repositoryRoot, directory),
    env: options.environment,
    label: options.label,
    maxOutputBytes: 64 * 1024,
    redact: (value) => redactSecrets(value, options.redactions),
    signal: abortController.signal,
    timeoutMs: options.timeoutMs,
  });
}

function runLocal(executable, arguments_, {
  environment,
  label,
  timeoutMs = 180_000,
}) {
  return runBoundedProcess(executable, arguments_, {
    cwd: repositoryRoot,
    env: environment,
    label,
    maxOutputBytes: 64 * 1024,
    signal: abortController.signal,
    timeoutMs,
  });
}

function requireLocalTool(executable, arguments_) {
  try {
    execFileSync(executable, arguments_, { cwd: repositoryRoot, stdio: "ignore" });
  } catch (error) {
    throw new Error(`${executable} is required before the Cloudflare rollout can begin`, {
      cause: error,
    });
  }
}

function git(...arguments_) {
  return execFileSync("git", arguments_, {
    cwd: repositoryRoot,
    encoding: "utf8",
  }).trim();
}

async function readJson(path) {
  try {
    return JSON.parse(await readFile(path, "utf8"));
  } catch (error) {
    throw new Error(`required rollout file ${path} is missing or invalid`, { cause: error });
  }
}

const invoked = process.argv[1] ? pathToFileURL(resolve(process.argv[1])).href : undefined;
if (invoked === import.meta.url) {
  let signal;
  const terminate = (value) => {
    if (signal) return;
    signal = value;
    abortController.abort(new Error(`Cloudflare rollout received ${value}`));
  };
  const interrupt = () => terminate("SIGINT");
  const terminateSignal = () => terminate("SIGTERM");
  process.on("SIGINT", interrupt);
  process.on("SIGTERM", terminateSignal);
  try {
    await main();
  } catch (error) {
    if (!signal) throw error;
  } finally {
    process.off("SIGINT", interrupt);
    process.off("SIGTERM", terminateSignal);
  }
  if (signal) {
    process.stderr.write(`Cloudflare rollout stopped by ${signal}.\n`);
    process.exitCode = signal === "SIGINT" ? 130 : 143;
  }
}
