import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import test from "node:test";

import { redactSecrets } from "../services/managed/scripts/child-process.mjs";
import {
  assertLiveResponse,
  assertOneCommandPreflight,
  assertPinnedWrangler,
  assertProductionServiceBindings,
  cloudflareAccountId,
  executeProductionMutations,
  finalContainerRollout,
  normalizeDeploymentEnvironment,
  parseAiSearchList,
  parseD1DatabaseList,
  parseR2BucketList,
  preflightEnvironment,
  prepareProductionResources,
  productionMutationPlan,
  productionProbeHeaders,
  productionProbeMaxBytes,
  productionResourceTopology,
  runProductionPhases,
} from "./deploy-cloudflare.mjs";

const revision = "a".repeat(40);
const databaseId = "11111111-2222-4333-8444-555555555555";
const productionWranglerDefinitions = [
  ["website", "web/wrangler.jsonc", true, false],
  ["managed-agent", "services/managed/wrangler.jsonc", true, true],
  ["egress-broker", "services/egress/wrangler.broker.jsonc", true, true],
  ["connect-api", "services/connect-api/wrangler.jsonc", true, false],
  ["connect-dialog", "web/connect-dialog/wrangler.jsonc", false, true],
  ["connect-playground", "web/connect-playground/wrangler.jsonc", false, true],
];

function productionEnvironment() {
  return {
    CLOUDFLARE_ACCOUNT_ID: "cloudflare-account",
    NANOCODEX_ADMIN_TOKEN: "admin-" + "a".repeat(32),
    NANOCODEX_BROKER_PROBE_TOKEN: "probe-" + "b".repeat(32),
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY: "c".repeat(43),
    NANOCODEX_GIT_TOKEN: "git-token",
    NANOCODEX_GITHUB_OAUTH_CLIENT_ID: "github-client",
    NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET: "github-secret",
    NANOCODEX_GOOGLE_OAUTH_CLIENT_ID: "google-client",
    NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET: "google-secret",
    NANOCODEX_WHOOP_OAUTH_CLIENT_ID: "whoop-client",
    NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET: "whoop-secret",
    TARGET_SHA: revision,
  };
}

const cleanCheckout = Object.freeze({
  dirty: false,
  head: revision,
  originMaster: revision,
});

test("production mutation plan is dependency-safe with and without root bootstrap", () => {
  assert.deepEqual(productionMutationPlan(false), [
    "connect-dialog",
    "root-bootstrap",
    "egress-broker",
    "managed-worker",
    "broker-boundary",
    "connect-api",
    "connect-playground",
    "root-final",
    "repository-publication",
  ]);
  assert.deepEqual(productionMutationPlan(true), [
    "connect-dialog",
    "egress-broker",
    "managed-worker",
    "broker-boundary",
    "connect-api",
    "connect-playground",
    "root-final",
    "repository-publication",
  ]);
  assert.equal(finalContainerRollout(false), "none");
  assert.equal(finalContainerRollout(true), "immediate");
});

test("production actions execute once in the declared order", async () => {
  const observed = [];
  const actions = Object.fromEntries(
    productionMutationPlan(false).map((component) => [component, async () => observed.push(component)]),
  );
  const plan = await executeProductionMutations(false, actions);
  assert.deepEqual(observed, plan);
});

test("a preflight failure prevents preparation and every remote mutation", async () => {
  const observed = [];
  await assert.rejects(runProductionPhases({
    preflight: async () => {
      observed.push("preflight");
      throw new Error("missing production secret");
    },
    prepare: async () => observed.push("prepare"),
    rootExists: async () => false,
    actions: {},
    health: async () => observed.push("health"),
  }), /missing production secret/);
  assert.deepEqual(observed, ["preflight"]);
});

test("preflight accepts either a token or authenticated local Wrangler OAuth", () => {
  const oauth = productionEnvironment();
  assert.equal(assertOneCommandPreflight(oauth, cleanCheckout), revision);
  assert.equal(preflightEnvironment(oauth, revision).CLOUDFLARE_OAUTH_CONFIGURED, "true");

  const token = { ...productionEnvironment(), CLOUDFLARE_API_TOKEN: "cloudflare-token" };
  assert.equal(assertOneCommandPreflight(token, cleanCheckout), revision);
  const tokenPreflight = preflightEnvironment(token, revision);
  assert.equal(tokenPreflight.CLOUDFLARE_API_TOKEN_CONFIGURED, "true");
  assert.equal(tokenPreflight.CLOUDFLARE_OAUTH_CONFIGURED, "false");
});

test("one-command deploy discovers the sole account from authenticated Wrangler", () => {
  const discovered = cloudflareAccountId(undefined, {
    loggedIn: true,
    accounts: [{ id: "16ce0442a940f01beefdb15a196a43ea" }],
  });
  assert.equal(discovered, "16ce0442a940f01beefdb15a196a43ea");
  assert.equal(cloudflareAccountId(" explicit-account ", {}), "explicit-account");
  assert.throws(() => cloudflareAccountId(undefined, {
    loggedIn: true,
    accounts: [{ id: "a".repeat(32) }, { id: "b".repeat(32) }],
  }), /multiple Cloudflare accounts/);
  assert.throws(() => cloudflareAccountId(undefined, { loggedIn: false, accounts: [] }),
    /did not expose a Cloudflare account/);
});

test("legacy private env names normalize and rollout secrets derive stably", () => {
  const legacy = {
    GH_CLIENT_ID: "github-client",
    GH_CLIENT_SECRETS: "github-secret",
    GOOGLE_CLIENT_ID: "google-client",
    GOOGLE_CLIENT_SECRET: "google-secret",
    WHOOP_CLIENT_ID: "whoop-client",
    WHOOP_CLIENT_SECRET: "whoop-secret",
    GIT_MIRROR_TOKEN: "git-token",
    SESSION_CREDENTIAL_KEY: "s".repeat(43),
  };
  const first = normalizeDeploymentEnvironment(legacy);
  const second = normalizeDeploymentEnvironment(legacy);
  assert.equal(first.NANOCODEX_GITHUB_OAUTH_CLIENT_ID, legacy.GH_CLIENT_ID);
  assert.equal(first.NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET, legacy.GH_CLIENT_SECRETS);
  assert.equal(first.NANOCODEX_GOOGLE_OAUTH_CLIENT_ID, legacy.GOOGLE_CLIENT_ID);
  assert.equal(first.NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET, legacy.GOOGLE_CLIENT_SECRET);
  assert.equal(first.NANOCODEX_WHOOP_OAUTH_CLIENT_ID, legacy.WHOOP_CLIENT_ID);
  assert.equal(first.NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET, legacy.WHOOP_CLIENT_SECRET);
  assert.equal(first.NANOCODEX_GIT_TOKEN, legacy.GIT_MIRROR_TOKEN);
  for (const name of [
    "NANOCODEX_ADMIN_TOKEN",
    "NANOCODEX_BROKER_PROBE_TOKEN",
    "NANOCODEX_CREDENTIAL_ENCRYPTION_KEY",
  ]) {
    assert.match(first[name], /^[A-Za-z0-9_-]{43}$/);
    assert.equal(first[name], second[name]);
  }
  assert.notEqual(first.NANOCODEX_ADMIN_TOKEN, first.NANOCODEX_BROKER_PROBE_TOKEN);
});

test("preflight rejects dirty, stale, and incomplete production inputs", () => {
  assert.throws(
    () => assertOneCommandPreflight(productionEnvironment(), { ...cleanCheckout, dirty: true }),
    /tracked changes/,
  );
  assert.throws(
    () => assertOneCommandPreflight(productionEnvironment(), {
      ...cleanCheckout,
      originMaster: "b".repeat(40),
    }),
    /origin\/master/,
  );
  const incomplete = productionEnvironment();
  delete incomplete.NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET;
  assert.throws(
    () => assertOneCommandPreflight(incomplete, cleanCheckout),
    /NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET/,
  );
});

test("each local Wrangler executable must match its checked-in lock", () => {
  const packageJson = { devDependencies: { wrangler: "^4.115.0" } };
  const packageLock = { packages: { "node_modules/wrangler": { version: "4.125.0" } } };
  assert.equal(
    assertPinnedWrangler(packageJson, packageLock, { version: "4.125.0" }, "fixture"),
    "4.125.0",
  );
  assert.throws(
    () => assertPinnedWrangler(packageJson, packageLock, { version: "4.124.0" }, "fixture"),
    /does not match package-lock/,
  );
});

test("production resource topology is derived from binding configs", () => {
  const topology = resourceTopology();
  assert.deepEqual(topology.r2Buckets, [
    "nanocodex-evals",
    "nanocodex-git",
    "nanocodex-managed-history",
  ]);
  assert.deepEqual(topology.d1DatabaseNames, ["nanocodex-evals"]);
  assert.deepEqual(topology.aiSearchInstances, [{
    name: "nanocodex-history-dev-20260824",
    namespace: "default",
    source: "nanocodex-managed-history",
    type: "r2",
  }]);
  assert.equal(topology.d1Migrations.length, 1);
  assert.equal(topology.d1Migrations[0].binding, "EVALS_DB");
  assert.equal(topology.d1Migrations[0].migrationsDir, "/repository/web/migrations");
});

test("production resource topology covers the real checked-in Wrangler configs", async () => {
  const topology = productionResourceTopology(await Promise.all([
    ["website", "web", "web/wrangler.jsonc"],
    ["managed agent", "services/managed", "services/managed/wrangler.jsonc"],
  ].map(async ([label, directory, path]) => ({
    config: JSON.parse(await readFile(new URL(`../${path}`, import.meta.url), "utf8")),
    directory: resolve(directory),
    label,
  }))));
  assert.deepEqual(topology.r2Buckets, [
    "nanocodex-evals",
    "nanocodex-git",
    "nanocodex-managed-history",
  ]);
  assert.deepEqual(topology.d1DatabaseNames, ["nanocodex-evals"]);
  assert.deepEqual(topology.aiSearchInstances, [{
    name: "nanocodex-history-dev-20260824",
    namespace: "default",
    source: "nanocodex-managed-history",
    type: "r2",
  }]);
  assert.equal(topology.d1Migrations.length, 1);
});

test("production service bindings form the complete deployment graph", async () => {
  const configurations = await Promise.all(productionWranglerDefinitions.map(async ([label, path]) => ({
    config: JSON.parse(await readFile(new URL(`../${path}`, import.meta.url), "utf8")),
    label,
  })));
  assert.doesNotThrow(() => assertProductionServiceBindings(configurations));

  const broken = structuredClone(configurations);
  broken.find(({ label }) => label === "connect-api").config.services[1].service = "wrong-worker";
  assert.throws(
    () => assertProductionServiceBindings(broken),
    /connect-api service bindings/,
  );
});

test("production Workers persist native telemetry only outside credential-bearing callback ingress", async () => {
  const expectedObservability = {
    enabled: true,
    logs: {
      enabled: true,
      head_sampling_rate: 1,
      invocation_logs: true,
      persist: true,
    },
    traces: {
      enabled: true,
      head_sampling_rate: 1,
      persist: true,
    },
  };
  for (const [label, path, hasServerSource, persistsNativeTelemetry] of productionWranglerDefinitions) {
    const config = JSON.parse(await readFile(new URL(`../${path}`, import.meta.url), "utf8"));
    assert.deepEqual(
      config.observability,
      persistsNativeTelemetry ? expectedObservability : { enabled: false },
      `${label} observability`,
    );
    assert.equal(config.upload_source_maps, hasServerSource ? true : undefined, `${label} source maps`);
  }
});

test("every production code deploy projects the exact Git revision into Worker logs", async () => {
  const [rootDeploy, managedDeploy, brokerDeploy] = await Promise.all([
    readFile(new URL("./deploy-cloudflare.mjs", import.meta.url), "utf8"),
    readFile(new URL("../services/managed/scripts/production-rollout.mjs", import.meta.url), "utf8"),
    readFile(new URL("../services/egress/scripts/production-broker.mjs", import.meta.url), "utf8"),
  ]);
  for (const source of [rootDeploy, managedDeploy, brokerDeploy]) {
    assert.match(source, /"--var",\s*`DEPLOYMENT_SHA:\$\{revision\}`/);
  }
});

test("Wrangler resource list formats are parsed fail-closed", () => {
  assert.deepEqual([...parseR2BucketList(`Listing buckets...\nname:  first-bucket\ncreation_date: now\n\nname:  second-bucket\ncreation_date: later\n`)], [
    "first-bucket",
    "second-bucket",
  ]);
  assert.deepEqual([...parseD1DatabaseList(JSON.stringify([
    { name: "nanocodex-evals", uuid: databaseId },
  ])).entries()], [[
    "nanocodex-evals",
    { id: databaseId, name: "nanocodex-evals" },
  ]]);
  assert.deepEqual([...parseAiSearchList(JSON.stringify([
    {
      id: "nanocodex-history-dev-20260824",
      namespace: "default",
      source: "nanocodex-managed-history",
      type: "r2",
    },
  ])).values()], [{
    name: "nanocodex-history-dev-20260824",
    namespace: "default",
    source: "nanocodex-managed-history",
    type: "r2",
  }]);
  assert.throws(
    () => parseD1DatabaseList("Wrangler warning before JSON\n[]"),
    /clean JSON/,
  );
  assert.throws(
    () => parseD1DatabaseList(JSON.stringify([
      { name: "duplicate", uuid: databaseId },
      { name: "duplicate", uuid: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee" },
    ])),
    /duplicate name/,
  );
});

test("blank-account resources are created, re-adopted, migrated, then Workers deploy", async () => {
  const fixture = resourceCommandFixture();
  const temporaryConfigs = [];
  const options = {
    run: fixture.run,
    withConfig: async (config, callback) => {
      temporaryConfigs.push(config);
      return callback("/private/d1-migrations.json");
    },
  };
  const topology = resourceTopology();
  const resources = await prepareProductionResources(topology, options);
  const workerActions = [];
  await executeProductionMutations(true, Object.fromEntries(
    productionMutationPlan(true).map((component) => [component, async () => {
      fixture.calls.push(["worker-deploy", component]);
      workerActions.push(component);
    }]),
  ));

  assert.deepEqual(resources.created, {
    aiSearch: ["nanocodex-history-dev-20260824"],
    d1: ["nanocodex-evals"],
    r2: ["nanocodex-evals", "nanocodex-git", "nanocodex-managed-history"],
  });
  assert.deepEqual(resources.d1DatabaseIds, { "nanocodex-evals": databaseId });
  for (const command of [
    ["r2", "bucket", "list"],
    ["d1", "list", "--json"],
    [
      "ai-search", "list", "--namespace", "default", "--page", "1",
      "--per-page", "100", "--json",
    ],
    [
      "ai-search", "create", "nanocodex-history-dev-20260824",
      "--namespace", "default", "--type", "r2", "--source",
      "nanocodex-managed-history", "--json",
    ],
    ["r2", "bucket", "create", "nanocodex-git"],
    ["d1", "create", "nanocodex-evals"],
  ]) {
    assert.equal(fixture.calls.some((arguments_) => (
      JSON.stringify(arguments_) === JSON.stringify(command)
    )), true, `missing mocked Wrangler command ${command.join(" ")}`);
  }
  assert.deepEqual(temporaryConfigs[0].d1_databases, [{
    binding: "EVALS_DB",
    database_id: databaseId,
    database_name: "nanocodex-evals",
    migrations_dir: "/repository/web/migrations",
  }]);
  const migrationIndex = fixture.calls.findIndex((arguments_) => (
    arguments_[0] === "d1" && arguments_[1] === "migrations"
  ));
  const sourceBucketIndex = fixture.calls.findIndex((arguments_) => (
    arguments_[0] === "r2" && arguments_[2] === "create"
      && arguments_[3] === "nanocodex-managed-history"
  ));
  const aiSearchCreateIndex = fixture.calls.findIndex((arguments_) => (
    arguments_[0] === "ai-search" && arguments_[1] === "create"
  ));
  const firstWorkerIndex = fixture.calls.findIndex((arguments_) => arguments_[0] === "worker-deploy");
  assert.ok(sourceBucketIndex >= 0 && sourceBucketIndex < aiSearchCreateIndex);
  assert.ok(migrationIndex >= 0 && migrationIndex < firstWorkerIndex);
  assert.deepEqual(fixture.calls[migrationIndex], [
    "d1",
    "migrations",
    "apply",
    "EVALS_DB",
    "--remote",
    "--config",
    "/private/d1-migrations.json",
  ]);
  assert.deepEqual(workerActions, productionMutationPlan(true));

  const createsBeforeRerun = fixture.calls.filter(isResourceCreate).length;
  const rerun = await prepareProductionResources(topology, options);
  assert.deepEqual(rerun.created, { aiSearch: [], d1: [], r2: [] });
  assert.equal(fixture.calls.filter(isResourceCreate).length, createsBeforeRerun);
});

test("existing resources are adopted without mutation and retain the live D1 ID", async () => {
  const fixture = resourceCommandFixture({ populated: true });
  const configs = [];
  const resources = await prepareProductionResources(resourceTopology(), {
    run: fixture.run,
    withConfig: async (config, callback) => {
      configs.push(config);
      return callback("/private/d1-migrations.json");
    },
  });
  assert.deepEqual(resources.created, { aiSearch: [], d1: [], r2: [] });
  assert.deepEqual(resources.d1DatabaseIds, { "nanocodex-evals": databaseId });
  assert.equal(configs[0].d1_databases[0].database_id, databaseId);
  assert.equal(fixture.calls.filter(isResourceCreate).length, 0);
});

test("an existing AI Search instance with the wrong source fails closed", async () => {
  const fixture = resourceCommandFixture({
    aiSearchSource: "another-history-bucket",
    populated: true,
  });
  await assert.rejects(prepareProductionResources(resourceTopology(), {
    run: fixture.run,
    withConfig: async (_config, callback) => callback("/private/d1-migrations.json"),
  }), /with source nanocodex-managed-history/);
  assert.equal(fixture.calls.filter(isResourceCreate).length, 0);
  assert.equal(fixture.calls.some((arguments_) => arguments_[0] === "d1"
    && arguments_[1] === "migrations"), false);
});

test("an unreconciled AI Search prerequisite stops before every Worker mutation", async () => {
  const fixture = resourceCommandFixture({
    rejectAiSearchCreate: true,
  });
  const observed = [];
  await assert.rejects((async () => {
    await prepareProductionResources(resourceTopology(), {
      run: fixture.run,
      withConfig: async (_config, callback) => callback("/private/d1-migrations.json"),
    });
    observed.push("worker-deploy");
  })(), /could not be provisioned or adopted/);
  assert.deepEqual(observed, []);
  assert.equal(fixture.calls.filter((arguments_) => arguments_[0] === "r2"
    && arguments_[2] === "create").length, 3);
  assert.equal(fixture.calls.some((arguments_) => arguments_[0] === "d1"
    && arguments_[1] === "create"), false);
  assert.equal(fixture.calls.some((arguments_) => arguments_[0] === "d1"
    && arguments_[1] === "migrations"), false);
});

test("rollout diagnostics redact deployment and application secrets", () => {
  const secrets = ["cloudflare-token", "oauth-client-secret", "admin-secret"];
  const diagnostic = redactSecrets(
    `failed cloudflare-token oauth-client-secret admin-secret`,
    secrets,
  );
  assert.equal(diagnostic, "failed [redacted] [redacted] [redacted]");
  for (const secret of secrets) assert.doesNotMatch(diagnostic, new RegExp(secret));
});

test("live checks reject the managed-service-unavailable deployment state", () => {
  const unavailable = Response.json({ error: "managed_service_unavailable" }, { status: 503 });
  assert.throws(
    () => assertLiveResponse(
      "managed-binding",
      unavailable,
      { error: "managed_service_unavailable" },
      revision,
    ),
    /managed account boundary|Service Binding is unavailable/,
  );

  const available = Response.json({
    user: { id: "browser-account" },
    organization: { id: "browser-organization" },
  });
  assert.doesNotThrow(() => assertLiveResponse(
    "managed-binding",
    available,
    {
      user: { id: "browser-account" },
      organization: { id: "browser-organization" },
    },
    revision,
  ));
});

test("root health requires the exact production SHA", () => {
  const response = Response.json({ status: "ok", deployment_sha: revision });
  assert.doesNotThrow(() => assertLiveResponse(
    "root-health",
    response,
    { status: "ok", deployment_sha: revision },
    revision,
  ));
  assert.throws(() => assertLiveResponse(
    "root-health",
    response,
    { status: "ok", deployment_sha: "b".repeat(40) },
    revision,
  ), /deployed SHA/);
});

test("repository health requires the exact production generation", () => {
  const response = Response.json({ repository: { head: revision } });
  assert.doesNotThrow(() => assertLiveResponse(
    "repository",
    response,
    { repository: { head: revision } },
    revision,
  ));
  assert.throws(() => assertLiveResponse(
    "repository",
    response,
    { repository: { head: "b".repeat(40) } },
    revision,
  ), /deployed SHA/);
});

test("repository health requires stock Git advertisement for the exact SHA", () => {
  const response = new Response(`003d${revision} refs/heads/master\n0000`, {
    headers: { "content-type": "application/x-git-upload-pack-result" },
  });
  assert.doesNotThrow(() => assertLiveResponse(
    "repository-git",
    response,
    `003d${revision} refs/heads/master\n0000`,
    revision,
  ));
  assert.throws(() => assertLiveResponse(
    "repository-git",
    response,
    `003d${"b".repeat(40)} refs/heads/master\n0000`,
    revision,
  ), /deployed SHA/);
});

test("repository health permits the bounded full-tree snapshot", () => {
  assert.equal(productionProbeMaxBytes("root-health"), 64 * 1024);
  assert.equal(productionProbeMaxBytes("repository"), 8 * 1024 * 1024);
});

test("Connect device deep-link health requires an HTML document", () => {
  const response = new Response("<!doctype html><title>Nanocodex</title>", {
    headers: { "content-type": "text/html; charset=utf-8" },
  });
  assert.doesNotThrow(() => assertLiveResponse(
    "root-connect-device",
    response,
    "<!doctype html><title>Nanocodex</title>",
    revision,
  ));
  assert.throws(() => assertLiveResponse(
    "root-connect-device",
    new Response("Not found", { status: 404 }),
    "Not found",
    revision,
  ), /HTTP 200/);
});

test("Connect device health reproduces a browser document navigation", () => {
  assert.deepEqual(productionProbeHeaders("root-connect-device", "text"), {
    accept: "text/html",
    "sec-fetch-dest": "document",
    "sec-fetch-mode": "navigate",
  });
  assert.deepEqual(productionProbeHeaders("root-health", "json"), {
    accept: "application/json",
  });
});

function resourceTopology() {
  return productionResourceTopology([
    {
      config: {
        compatibility_date: "2026-05-22",
        r2_buckets: [
          { binding: "GIT_OBJECTS", bucket_name: "nanocodex-git" },
          { binding: "EVALS_ARTIFACTS", bucket_name: "nanocodex-evals" },
        ],
        d1_databases: [{
          binding: "EVALS_DB",
          database_name: "nanocodex-evals",
          database_id: "00000000-0000-0000-0000-000000000000",
          migrations_dir: "migrations",
        }],
      },
      directory: "/repository/web",
      label: "website",
    },
    {
      config: {
        compatibility_date: "2026-07-29",
        r2_buckets: [{
          binding: "NANOCODEX_HISTORY",
          bucket_name: "nanocodex-managed-history",
        }],
        ai_search: [{
          binding: "HISTORY_AI_SEARCH",
          instance_name: "nanocodex-history-dev-20260824",
        }],
      },
      directory: "/repository/services/managed",
      label: "managed agent",
    },
  ]);
}

function resourceCommandFixture({
  aiSearchSource = "nanocodex-managed-history",
  populated = false,
  rejectAiSearchCreate = false,
} = {}) {
  const calls = [];
  const r2 = new Set(populated ? [
    "nanocodex-evals",
    "nanocodex-git",
    "nanocodex-managed-history",
  ] : []);
  const d1 = new Map(populated ? [["nanocodex-evals", databaseId]] : []);
  const aiSearch = new Set(populated ? ["nanocodex-history-dev-20260824"] : []);
  return {
    calls,
    async run(arguments_) {
      calls.push([...arguments_]);
      if (arguments_[0] === "r2" && arguments_[2] === "list") {
        return [...r2].map((name) => `name:  ${name}\ncreation_date: now`).join("\n\n");
      }
      if (arguments_[0] === "r2" && arguments_[2] === "create") {
        r2.add(arguments_[3]);
        return "created";
      }
      if (arguments_[0] === "d1" && arguments_[1] === "list") {
        return JSON.stringify([...d1].map(([name, uuid]) => ({ name, uuid })));
      }
      if (arguments_[0] === "d1" && arguments_[1] === "create") {
        d1.set(arguments_[2], databaseId);
        return "created";
      }
      if (arguments_[0] === "ai-search" && arguments_[1] === "list") {
        return JSON.stringify([...aiSearch].map((id) => ({
          id,
          namespace: "default",
          source: aiSearchSource,
          type: "r2",
        })));
      }
      if (arguments_[0] === "ai-search" && arguments_[1] === "create") {
        if (rejectAiSearchCreate) throw new Error("No AI Search API token found");
        aiSearch.add(arguments_[2]);
        return JSON.stringify({
          id: arguments_[2],
          source: "nanocodex-managed-history",
          type: "r2",
        });
      }
      if (arguments_[0] === "d1" && arguments_[1] === "migrations") return "migrated";
      throw new Error(`unexpected fixture command ${arguments_.join(" ")}`);
    },
  };
}

function isResourceCreate(arguments_) {
  return (arguments_[0] === "r2" && arguments_[2] === "create")
    || (arguments_[0] === "d1" && arguments_[1] === "create")
    || (arguments_[0] === "ai-search" && arguments_[1] === "create");
}
