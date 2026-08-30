import assert from "node:assert/strict";
import { access, readFile, stat } from "node:fs/promises";
import test from "node:test";

import {
  assertProductionCheckout,
  assertProductionPreflight,
  assertWebBuildAttestation,
  buildBoundaryProbeConfig,
  buildManagedProductionConfig,
  buildWebBootstrapConfig,
  buildWebProductionConfig,
  managedSecretPayload,
  productionWranglerEnvironment,
  webSecretPayload,
  withPrivateRolloutFiles,
} from "../scripts/production-rollout.mjs";
import {
  assertCachedManagedWasmAttestation,
  assertManagedWasmAttestation,
} from "../../../js/bindings/scripts/check-managed-wasm.mjs";

const revision = "a".repeat(40);
const adminToken = "admin-" + "a".repeat(32);

function preflightEnvironment() {
  return {
    CLOUDFLARE_ACCOUNT_ID: "account-id",
    CLOUDFLARE_API_TOKEN_CONFIGURED: "true",
    NANOCODEX_ADMIN_TOKEN: adminToken,
    NANOCODEX_BROKER_PROBE_TOKEN_CONFIGURED: "true",
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY_CONFIGURED: "true",
    NANOCODEX_GIT_TOKEN_CONFIGURED: "true",
    NANOCODEX_GITHUB_OAUTH_CLIENT_ID_CONFIGURED: "true",
    NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET_CONFIGURED: "true",
    NANOCODEX_GOOGLE_OAUTH_CLIENT_ID_CONFIGURED: "true",
    NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET_CONFIGURED: "true",
    NANOCODEX_X_OAUTH_CLIENT_ID_CONFIGURED: "true",
    NANOCODEX_X_OAUTH_CLIENT_SECRET_CONFIGURED: "true",
    NANOCODEX_WHOOP_OAUTH_CLIENT_ID_CONFIGURED: "true",
    NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET_CONFIGURED: "true",
    TARGET_SHA: revision,
  };
}

test("production preflight requires only deployment and application boundary inputs", () => {
  assert.deepEqual(assertProductionPreflight(preflightEnvironment()), {
    adminToken,
    revision,
  });
  for (const name of [
    "CLOUDFLARE_API_TOKEN_CONFIGURED",
    "NANOCODEX_BROKER_PROBE_TOKEN_CONFIGURED",
    "NANOCODEX_CREDENTIAL_ENCRYPTION_KEY_CONFIGURED",
    "NANOCODEX_GIT_TOKEN_CONFIGURED",
    "NANOCODEX_GITHUB_OAUTH_CLIENT_ID_CONFIGURED",
    "NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET_CONFIGURED",
    "NANOCODEX_GOOGLE_OAUTH_CLIENT_ID_CONFIGURED",
    "NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET_CONFIGURED",
  ]) {
    const missing = preflightEnvironment();
    delete missing[name];
    assert.throws(() => assertProductionPreflight(missing), /required for production rollout/);
  }
  const weak = preflightEnvironment();
  weak.NANOCODEX_ADMIN_TOKEN = "short";
  assert.throws(() => assertProductionPreflight(weak), /at least 32 bytes/);
});

test("production preflight treats X OAuth as an optional atomic pair", () => {
  const absent = preflightEnvironment();
  absent.NANOCODEX_X_OAUTH_CLIENT_ID_CONFIGURED = "false";
  absent.NANOCODEX_X_OAUTH_CLIENT_SECRET_CONFIGURED = "false";
  assert.doesNotThrow(() => assertProductionPreflight(absent));

  for (const configured of [
    ["true", "false"],
    ["false", "true"],
  ]) {
    const partial = preflightEnvironment();
    [
      partial.NANOCODEX_X_OAUTH_CLIENT_ID_CONFIGURED,
      partial.NANOCODEX_X_OAUTH_CLIENT_SECRET_CONFIGURED,
    ] = configured;
    assert.throws(
      () => assertProductionPreflight(partial),
      /X OAuth application credentials must be configured together/,
    );
  }
});

test("production preflight treats WHOOP OAuth as an optional atomic pair", () => {
  const absent = preflightEnvironment();
  absent.NANOCODEX_WHOOP_OAUTH_CLIENT_ID_CONFIGURED = "false";
  absent.NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET_CONFIGURED = "false";
  assert.doesNotThrow(() => assertProductionPreflight(absent));

  for (const configured of [
    ["true", "false"],
    ["false", "true"],
  ]) {
    const partial = preflightEnvironment();
    [
      partial.NANOCODEX_WHOOP_OAUTH_CLIENT_ID_CONFIGURED,
      partial.NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET_CONFIGURED,
    ] = configured;
    assert.throws(
      () => assertProductionPreflight(partial),
      /WHOOP OAuth application credentials must be configured together/,
    );
  }
});

test("production rollout accepts only a clean checkout of the selected master revision", () => {
  const checkout = { dirty: false, head: revision, originMaster: revision };
  assert.doesNotThrow(() => assertProductionCheckout(revision, checkout));
  assert.throws(
    () => assertProductionCheckout(revision, { ...checkout, head: "b".repeat(40) }),
    /match TARGET_SHA/,
  );
  assert.throws(
    () => assertProductionCheckout(revision, { ...checkout, originMaster: "b".repeat(40) }),
    /fetched origin\/master/,
  );
  assert.throws(
    () => assertProductionCheckout(revision, { ...checkout, dirty: true }),
    /tracked changes/,
  );
});

test("managed production rejects stale, dirty, or modified WASM artifacts", () => {
  const artifacts = {
    "nanocodex.js": "1".repeat(64),
    "nanocodex.d.ts": "2".repeat(64),
    "nanocodex_bg.js": "3".repeat(64),
    "nanocodex_bg.wasm": "4".repeat(64),
    "nanocodex_worker.js": "5".repeat(64),
    "package.json": "6".repeat(64),
  };
  const attestation = {
    schema: 1,
    revision,
    dirty: false,
    sourceWasmSha256: "0".repeat(64),
    artifacts,
  };
  assert.doesNotThrow(() => assertManagedWasmAttestation(attestation, revision, artifacts));
  assert.doesNotThrow(() => assertCachedManagedWasmAttestation(attestation, {
    artifacts,
    sourceWasmSha256: attestation.sourceWasmSha256,
  }));
  assert.throws(() => assertCachedManagedWasmAttestation(attestation, {
    artifacts,
    sourceWasmSha256: "8".repeat(64),
  }), /source bytes/);
  assert.throws(
    () => assertManagedWasmAttestation(attestation, "b".repeat(40), artifacts),
    /exact production revision/,
  );
  assert.throws(
    () => assertManagedWasmAttestation({ ...attestation, dirty: true }, revision, artifacts),
    /clean source/,
  );
  assert.throws(
    () => assertManagedWasmAttestation(attestation, revision, {
      ...artifacts,
      "nanocodex_bg.wasm": "7".repeat(64),
    }),
    /artifact bytes/,
  );
});

test("website production rejects stale or mismatched build artifacts", () => {
  const config = Buffer.from('{"name":"nanocodex"}\n');
  const attestation = {
    revision,
    wranglerConfigSha256: "9600b209414abcc5d304884f6ff5f1e1bcee00a1c3487dc6253b9f7a4b1f0de2",
  };
  assert.doesNotThrow(() => assertWebBuildAttestation(attestation, revision, config));
  assert.throws(
    () => assertWebBuildAttestation(attestation, "b".repeat(40), config),
    /exact production revision/,
  );
  assert.throws(
    () => assertWebBuildAttestation(attestation, revision, Buffer.from("modified")),
    /production Wrangler config/,
  );
  assert.throws(
    () => assertWebBuildAttestation(undefined, revision, config),
    /website build attestation/,
  );
});

test("production Wrangler environment excludes every secret and stale provider input", () => {
  const child = productionWranglerEnvironment({
    CLOUDFLARE_ENV: "staging",
    NANOCODEX_ADMIN_TOKEN: "admin-secret",
    NANOCODEX_BROKER_PROBE_TOKEN: "probe-secret",
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY: "encryption-secret",
    NANOCODEX_GITHUB_OAUTH_CLIENT_ID: "github-client-id",
    NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET: "github-client-secret",
    NANOCODEX_GOOGLE_OAUTH_CLIENT_ID: "google-client-id",
    NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET: "google-client-secret",
    NANOCODEX_X_OAUTH_CLIENT_ID: "x-client-id",
    NANOCODEX_X_OAUTH_CLIENT_SECRET: "x-client-secret",
    NANOCODEX_MANAGED_AUTH_MODE: "chatgpt",
    NANOCODEX_MANAGED_CODEX_RELAY_URL: "relay-secret",
    OPENAI_API_KEY: "provider-secret",
    PATH: "/usr/bin",
  }, { accountId: "account-id", apiToken: "api-token" });
  assert.deepEqual(child, {
    CLOUDFLARE_ACCOUNT_ID: "account-id",
    CLOUDFLARE_API_TOKEN: "api-token",
    PATH: "/usr/bin",
  });
});

test("managed production config retains the exact private eight-DO topology", async () => {
  const base = JSON.parse(await readFile(new URL("../wrangler.jsonc", import.meta.url), "utf8"));
  assert.equal(base.name, "nanocodex-managed-development");
  const config = buildManagedProductionConfig(base, { mainPath: "/fixed/managed.ts" });
  assert.equal(config.name, "nanocodex-durable-agent");
  assert.equal(config.workers_dev, false);
  assert.equal(config.main, "/fixed/managed.ts");
  assert.equal(config.upload_source_maps, true);
  assert.deepEqual(config.observability, base.observability);
  assert.deepEqual(config.compatibility_flags, ["nodejs_compat", "global_fetch_strictly_public"]);
  assert.equal(config.worker_loaders, undefined);
  assert.deepEqual(config.services, [
    { binding: "NANOCODEX", service: "nanocodex-egress" },
  ]);
  assert.equal(config.durable_objects.bindings.length, 8);
  assert.deepEqual(config.migrations.map(({ tag }) => tag), ["v1", "v2", "v3", "v4"]);
  assert.doesNotMatch(JSON.stringify(config), /NANOCODEX_AUTH_MODE|OPENAI_API_KEY|CODEX_OAUTH_BOOTSTRAP|CODEX_RELAY_URL/);
  assert.deepEqual(managedSecretPayload(adminToken), { NANOCODEX_ADMIN_TOKEN: adminToken });
  assert.deepEqual(webSecretPayload("g".repeat(43)), {
    GIT_MIRROR_TOKEN: "g".repeat(43),
  });
  assert.throws(
    () => buildManagedProductionConfig({ ...base, name: "nanocodex-durable-agent" }),
    /non-production template name/,
  );
});

test("boundary probe and website configs preserve the private service chain", () => {
  const probe = buildBoundaryProbeConfig({
    name: "nanocodex-boundary-aaaaaaaaaaaa-bbbbbbbbbb",
    revision,
    mainPath: "/fixed/probe.mjs",
  });
  assert.deepEqual(probe.services, [{ binding: "NANOCODEX", service: "nanocodex-egress" }]);
  assert.equal(probe.durable_objects, undefined);
  assert.deepEqual(probe.vars, { DEPLOYMENT_SHA: revision });

  const website = buildWebProductionConfig({
    name: "nanocodex",
    keep_vars: true,
    main: "index.js",
    upload_source_maps: true,
    observability: { enabled: false },
    assets: { directory: "../client" },
    services: [
      { binding: "EGRESS", service: "nanocodex-egress" },
      { binding: "NANOCODEX_BACKEND", service: "nanocodex-durable-agent" },
      { binding: "NANOCODEX_CONNECT_API", service: "nanocodex-connect-api" },
      { binding: "NANOCODEX_CONNECT_DIALOG", service: "nanocodex-connect-dialog" },
    ],
    containers: [{ class_name: "ChatGptEgress", image: "/stale/Dockerfile" }],
    d1_databases: [{
      binding: "EVALS_DB",
      database_name: "nanocodex-evals",
      database_id: "00000000-0000-0000-0000-000000000000",
      migrations_dir: "../../migrations",
    }],
    vars: { ENVIRONMENT: "production" },
  }, {
    artifactDirectory: "/artifact/nanocodex",
    currentWebRoot: "/current/web",
    d1DatabaseIds: {
      "nanocodex-evals": "11111111-2222-4333-8444-555555555555",
    },
  });
  assert.equal(website.main, "/artifact/nanocodex/index.js");
  assert.equal(website.upload_source_maps, true);
  assert.deepEqual(website.observability, { enabled: false });
  assert.equal(website.assets.directory, "/artifact/client");
  assert.equal(website.containers[0].image, "/current/web/container/Dockerfile");
  assert.equal(
    website.d1_databases[0].database_id,
    "11111111-2222-4333-8444-555555555555",
  );
  assert.equal(website.d1_databases[0].migrations_dir, "/migrations");
  assert.deepEqual(website.vars, { ENVIRONMENT: "production" });
  assert.deepEqual(website.services, [
    { binding: "NANOCODEX_BACKEND", service: "nanocodex-durable-agent" },
    { binding: "NANOCODEX_CONNECT_API", service: "nanocodex-connect-api" },
    { binding: "NANOCODEX_CONNECT_DIALOG", service: "nanocodex-connect-dialog" },
  ]);
  assert.deepEqual(buildWebBootstrapConfig({
    name: "nanocodex",
    keep_vars: true,
    main: "index.js",
    assets: { directory: "../client" },
    services: website.services,
    containers: [{ class_name: "ChatGptEgress", image: "/stale/Dockerfile" }],
  }, { artifactDirectory: "/artifact/nanocodex", currentWebRoot: "/current/web" }).services, [
    { binding: "NANOCODEX_CONNECT_DIALOG", service: "nanocodex-connect-dialog" },
  ]);
  assert.throws(() => buildWebProductionConfig({
    ...website,
    main: "index.js",
    assets: { directory: "../client" },
    services: [{ binding: "MULTIPLAYER_BACKEND", service: "nanocodex-durable-agent" }],
  }, { artifactDirectory: "/artifact" }), /requires NANOCODEX_BACKEND/);
  assert.throws(() => buildWebProductionConfig({
    ...website,
    main: "index.js",
    assets: { directory: "../client" },
    containers: [{ class_name: "ChatGptEgress", image: "/stale/Dockerfile" }],
  }, {
    artifactDirectory: "/artifact",
    d1DatabaseIds: {},
  }), /must have a reconciled database ID/);
});

test("temporary rollout files are mode 0600 and removed in finally", async () => {
  let directory;
  await assert.rejects(withPrivateRolloutFiles({
    "managed-config.json": { workers_dev: false },
    "managed-secrets.json": { NANOCODEX_ADMIN_TOKEN: adminToken },
  }, async (paths) => {
    directory = paths.directory;
    assert.equal((await stat(paths["managed-config.json"])).mode & 0o777, 0o600);
    assert.equal((await stat(paths["managed-secrets.json"])).mode & 0o777, 0o600);
    throw new Error("fixture failure");
  }), /fixture failure/);
  await assert.rejects(access(directory), { code: "ENOENT" });
});

test("boundary probe verifies only private broker readiness", async () => {
  const source = await readFile(new URL("../scripts/production-boundary-probe-worker.mjs", import.meta.url), "utf8");
  assert.match(source, /env\.NANOCODEX/);
  assert.match(source, /\.well-known\/nanocodex\/broker-readiness/);
  assert.match(source, /method: "POST"/);
  assert.match(source, /Object\.keys\(ready\)\.length !== 1/);
  assert.doesNotMatch(source, /room|allocator|api[_ -]?key|EXPECTED_AUTH_MODE/i);
});

test("website deployment leaves the existing container rollout untouched", async () => {
  const source = await readFile(new URL("../scripts/production-rollout.mjs", import.meta.url), "utf8");
  assert.match(source, /containersRollout = "none"/);
});

test("CI orders the credential-neutral production rollout and keeps freshness gates", async () => {
  const workflow = await readFile(new URL("../../../.github/workflows/ci.yml", import.meta.url), "utf8");
  assert.match(workflow, /npm run check --prefix services\/egress/);
  assert.match(workflow, /npm run check --prefix services\/managed/);
  const productionJob = workflow.slice(workflow.indexOf("  production:"));
  const orderedSteps = [
    "Select the current production revision",
    "Validate the complete production rollout",
    "Deploy the private credential broker",
    "Require master before managed rollout",
    "Deploy the private managed Worker and migrations",
    "Verify private broker readiness",
    "Require master before website rollout",
    "Deploy the attested Cloudflare Worker",
    "Verify the active Worker revision",
    "Require master to remain on the deployed revision",
    "Publish the matching repository generation",
  ];
  let previous = -1;
  for (const step of orderedSteps) {
    const index = productionJob.indexOf(`name: ${step}`);
    assert.ok(index > previous, `${step} is missing or out of order`);
    previous = index;
  }
  assert.equal(productionJob.split("name: Deploy the private credential broker").length - 1, 1);
  const broker = workflowSection(productionJob, "Deploy the private credential broker", "Require master before managed rollout");
  const managed = workflowSection(productionJob, "Deploy the private managed Worker and migrations", "Verify private broker readiness");
  const website = workflowSection(productionJob, "Deploy the attested Cloudflare Worker", "Verify the active Worker revision");
  assert.match(broker, /secrets\.NANOCODEX_CREDENTIAL_ENCRYPTION_KEY/);
  assert.match(broker, /secrets\.NANOCODEX_BROKER_PROBE_TOKEN/);
  assert.match(broker, /secrets\.NANOCODEX_GITHUB_OAUTH_CLIENT_ID/);
  assert.match(broker, /secrets\.NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET/);
  assert.match(broker, /secrets\.NANOCODEX_GOOGLE_OAUTH_CLIENT_ID/);
  assert.match(broker, /secrets\.NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET/);
  assert.match(managed, /secrets\.NANOCODEX_ADMIN_TOKEN/);
  assert.doesNotMatch(managed, /BROKER_PROBE_TOKEN|CREDENTIAL_ENCRYPTION_KEY|OAUTH_CLIENT/);
  assert.doesNotMatch(website, /NANOCODEX_ADMIN_TOKEN|BROKER_PROBE_TOKEN|CREDENTIAL_ENCRYPTION_KEY|OAUTH_CLIENT/);
  assert.doesNotMatch(productionJob, /MANAGED_AUTH_MODE|MANAGED_OPENAI|MANAGED_CODEX|ROOM_ALLOCATOR|MULTIPLAYER_BACKEND/);
});

function workflowSection(workflow, start, end) {
  const startIndex = workflow.indexOf(`name: ${start}`);
  const endIndex = workflow.indexOf(`name: ${end}`, startIndex + 1);
  assert.ok(startIndex >= 0 && endIndex > startIndex, `workflow section ${start} is missing`);
  return workflow.slice(startIndex, endIndex);
}
