import assert from "node:assert/strict";
import { access, readFile, stat } from "node:fs/promises";
import test from "node:test";

import {
  brokerWranglerEnvironment,
  buildProductionBrokerConfig,
  deployProductionBroker,
  productionBrokerSecrets,
  productionRevision,
  withPrivateBrokerFiles,
} from "../scripts/production-broker.mjs";

const encryptionKey = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY";
const probeToken = "probe-" + "p".repeat(32);
const connectorSecrets = {
  NANOCODEX_GITHUB_OAUTH_CLIENT_ID: "github-client-id",
  NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET: "github-client-secret",
  NANOCODEX_GOOGLE_OAUTH_CLIENT_ID: "google-client-id",
  NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET: "google-client-secret",
  NANOCODEX_X_OAUTH_CLIENT_ID: "x-client-id",
  NANOCODEX_X_OAUTH_CLIENT_SECRET: "x-client-secret",
  NANOCODEX_WHOOP_OAUTH_CLIENT_ID: "whoop-client-id",
  NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET: "whoop-client-secret",
};

test("production deployment selects only owned broker and connector secrets", () => {
  assert.deepEqual(productionBrokerSecrets({
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY: encryptionKey,
    NANOCODEX_BROKER_PROBE_TOKEN: probeToken,
    OPENAI_API_KEY: "must-not-be-selected",
    CODEX_OAUTH_BOOTSTRAP: "must-not-be-selected",
    ...connectorSecrets,
  }), {
    CREDENTIAL_ENCRYPTION_KEY: encryptionKey,
    NANOCODEX_BROKER_PROBE_TOKEN: probeToken,
    GITHUB_OAUTH_CLIENT_ID: "github-client-id",
    GITHUB_OAUTH_CLIENT_SECRET: "github-client-secret",
    GOOGLE_OAUTH_CLIENT_ID: "google-client-id",
    GOOGLE_OAUTH_CLIENT_SECRET: "google-client-secret",
    X_OAUTH_CLIENT_ID: "x-client-id",
    X_OAUTH_CLIENT_SECRET: "x-client-secret",
    WHOOP_OAUTH_CLIENT_ID: "whoop-client-id",
    WHOOP_OAUTH_CLIENT_SECRET: "whoop-client-secret",
  });
  assert.throws(() => productionBrokerSecrets({}), /CREDENTIAL_ENCRYPTION_KEY/);
});

test("production deployment accepts an optional previous encryption key for rotation", () => {
  assert.deepEqual(productionBrokerSecrets({
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY: encryptionKey,
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY_PREVIOUS: encryptionKey,
    NANOCODEX_BROKER_PROBE_TOKEN: probeToken,
    ...connectorSecrets,
  }), {
    CREDENTIAL_ENCRYPTION_KEY: encryptionKey,
    CREDENTIAL_ENCRYPTION_KEY_PREVIOUS: encryptionKey,
    NANOCODEX_BROKER_PROBE_TOKEN: probeToken,
    GITHUB_OAUTH_CLIENT_ID: "github-client-id",
    GITHUB_OAUTH_CLIENT_SECRET: "github-client-secret",
    GOOGLE_OAUTH_CLIENT_ID: "google-client-id",
    GOOGLE_OAUTH_CLIENT_SECRET: "google-client-secret",
    X_OAUTH_CLIENT_ID: "x-client-id",
    X_OAUTH_CLIENT_SECRET: "x-client-secret",
    WHOOP_OAUTH_CLIENT_ID: "whoop-client-id",
    WHOOP_OAUTH_CLIENT_SECRET: "whoop-client-secret",
  });
  assert.throws(() => productionBrokerSecrets({
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY: encryptionKey,
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY_PREVIOUS: "invalid",
    NANOCODEX_BROKER_PROBE_TOKEN: probeToken,
    ...connectorSecrets,
  }), /PREVIOUS must be a 32-byte base64url value/);
});

test("production deployment treats X OAuth as an optional atomic pair", () => {
  const {
    NANOCODEX_X_OAUTH_CLIENT_ID: _xId,
    NANOCODEX_X_OAUTH_CLIENT_SECRET: _xSecret,
    ...withoutX
  } = connectorSecrets;
  const secrets = productionBrokerSecrets({
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY: encryptionKey,
    NANOCODEX_BROKER_PROBE_TOKEN: probeToken,
    ...withoutX,
  });
  assert.equal("X_OAUTH_CLIENT_ID" in secrets, false);
  assert.equal("X_OAUTH_CLIENT_SECRET" in secrets, false);
  assert.throws(() => productionBrokerSecrets({
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY: encryptionKey,
    NANOCODEX_BROKER_PROBE_TOKEN: probeToken,
    ...withoutX,
    NANOCODEX_X_OAUTH_CLIENT_ID: "x-client-id",
  }), /X OAuth application credentials must be configured together/);
  assert.throws(() => productionBrokerSecrets({
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY: encryptionKey,
    NANOCODEX_BROKER_PROBE_TOKEN: probeToken,
    ...withoutX,
    NANOCODEX_X_OAUTH_CLIENT_SECRET: "x-client-secret",
  }), /X OAuth application credentials must be configured together/);
});

test("production deployment treats WHOOP OAuth as an optional atomic pair", () => {
  const {
    NANOCODEX_WHOOP_OAUTH_CLIENT_ID: _whoopId,
    NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET: _whoopSecret,
    ...withoutWhoop
  } = connectorSecrets;
  const secrets = productionBrokerSecrets({
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY: encryptionKey,
    NANOCODEX_BROKER_PROBE_TOKEN: probeToken,
    ...withoutWhoop,
  });
  assert.equal("WHOOP_OAUTH_CLIENT_ID" in secrets, false);
  assert.equal("WHOOP_OAUTH_CLIENT_SECRET" in secrets, false);
  assert.throws(() => productionBrokerSecrets({
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY: encryptionKey,
    NANOCODEX_BROKER_PROBE_TOKEN: probeToken,
    ...withoutWhoop,
    NANOCODEX_WHOOP_OAUTH_CLIENT_ID: "whoop-client-id",
  }), /WHOOP OAuth application credentials must be configured together/);
  assert.throws(() => productionBrokerSecrets({
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY: encryptionKey,
    NANOCODEX_BROKER_PROBE_TOKEN: probeToken,
    ...withoutWhoop,
    NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET: "whoop-client-secret",
  }), /WHOOP OAuth application credentials must be configured together/);
});

test("production deployment requires a full immutable Git revision", () => {
  assert.equal(productionRevision({ TARGET_SHA: "a".repeat(40) }), "a".repeat(40));
  assert.throws(() => productionRevision({ TARGET_SHA: "abc123" }), /full lowercase Git commit SHA/);
  assert.throws(() => productionRevision({ TARGET_SHA: "A".repeat(40) }), /full lowercase Git commit SHA/);
});

test("Wrangler child environment strips all provider and source deployment secrets", () => {
  const environment = brokerWranglerEnvironment({
    OPENAI_API_KEY: "provider",
    CODEX_OAUTH_BOOTSTRAP: "provider",
    LOCAL_CHATGPT_BOOTSTRAP: "provider",
    NANOCODEX_MANAGED_AUTH_MODE: "chatgpt",
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY: encryptionKey,
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY_PREVIOUS: encryptionKey,
    ...connectorSecrets,
    PATH: "/usr/bin",
  }, "account", "token");
  assert.deepEqual(environment, {
    PATH: "/usr/bin",
    CLOUDFLARE_ACCOUNT_ID: "account",
    CLOUDFLARE_API_TOKEN: "token",
  });
});

test("Wrangler child environment can use the local OAuth session", () => {
  assert.deepEqual(brokerWranglerEnvironment({ PATH: "/usr/bin" }, "account"), {
    PATH: "/usr/bin",
    CLOUDFLARE_ACCOUNT_ID: "account",
  });
});

test("the base and generated production configs keep every required DO binding", async () => {
  const base = JSON.parse(await readFile(new URL("../wrangler.broker.jsonc", import.meta.url)));
  const config = buildProductionBrokerConfig(base, { mainPath: "/fixed/egress.ts" });
  assert.equal(config.upload_source_maps, true);
  assert.deepEqual(config.observability, base.observability);
  assert.equal(config.workers_dev, false);
  assert.equal(config.routes, undefined);
  assert.deepEqual(config.vars, { ENVIRONMENT: "production" });
  assert.equal(config.main, "/fixed/egress.ts");
  const relay = {
    name: "CHATGPT_EGRESS",
    class_name: "ChatGptEgress",
    script_name: "nanocodex",
  };
  assert.deepEqual(base.durable_objects.bindings.at(-1), relay);
  assert.equal(config.durable_objects.bindings.filter((binding) => (
    binding.name === "CHATGPT_EGRESS"
  )).length, 1);
  assert.deepEqual(config.durable_objects.bindings.at(-1), relay);
  assert.ok(config.durable_objects.bindings.some((binding) => (
    binding.name === "USER_CONNECTORS" && binding.class_name === "UserConnectorBroker"
  )));
  assert.ok(config.durable_objects.bindings.some((binding) => (
    binding.name === "MCP_CONNECTIONS" && binding.class_name === "McpConnectionDirectory"
  )));
  assert.ok(config.migrations.some((migration) => (
    migration.tag === "v5"
      && migration.new_sqlite_classes?.includes("McpConnectionDirectory")
  )));
  assert.throws(
    () => buildProductionBrokerConfig({
      ...base,
      durable_objects: {
        ...base.durable_objects,
        bindings: base.durable_objects.bindings.filter((binding) => (
          binding.name !== "CHATGPT_EGRESS"
        )),
      },
    }, { mainPath: "/fixed/egress.ts" }),
    /ChatGPT relay DO bindings/,
  );
  assert.throws(
    () => buildProductionBrokerConfig({ ...base, migrations: [] }, { mainPath: "/fixed/egress.ts" }),
    /v2\/v3\/v4\/v5 DO migration chain/,
  );
});

test("temporary config and secret files are mode 0600 and removed", async () => {
  let temporary;
  await assert.rejects(withPrivateBrokerFiles({ "secret.json": { value: "secret" } }, async (paths) => {
    temporary = paths.directory;
    assert.equal((await stat(paths["secret.json"])).mode & 0o777, 0o600);
    throw new Error("fixture failure");
  }), /fixture failure/);
  await assert.rejects(access(temporary), { code: "ENOENT" });
});

test("production broker deploy uses the injected pinned-runner boundary with redactions", async () => {
  const environment = {
    CLOUDFLARE_ACCOUNT_ID: "account",
    CLOUDFLARE_API_TOKEN: "cloudflare-token",
    NANOCODEX_CREDENTIAL_ENCRYPTION_KEY: encryptionKey,
    NANOCODEX_BROKER_PROBE_TOKEN: probeToken,
    TARGET_SHA: "a".repeat(40),
    ...connectorSecrets,
  };
  let invocation;
  await deployProductionBroker(environment, {
    run: async (arguments_, options) => {
      invocation = { arguments_, options };
      assert.equal(JSON.parse(await readFile(arguments_[2], "utf8")).name, "nanocodex-egress");
      assert.equal(
        JSON.parse(await readFile(arguments_.at(-1), "utf8")).CREDENTIAL_ENCRYPTION_KEY,
        encryptionKey,
      );
    },
  });
  assert.equal(invocation.arguments_[0], "deploy");
  assert.ok(invocation.arguments_.includes("--strict"));
  assert.ok(invocation.arguments_.includes("a".repeat(40)));
  assert.deepEqual(
    invocation.arguments_.slice(invocation.arguments_.indexOf("--var"), invocation.arguments_.indexOf("--var") + 2),
    ["--var", `DEPLOYMENT_SHA:${"a".repeat(40)}`],
  );
  assert.equal(invocation.options.environment.CLOUDFLARE_API_TOKEN, "cloudflare-token");
  for (const secret of ["cloudflare-token", encryptionKey, probeToken, ...Object.values(connectorSecrets)]) {
    assert.ok(invocation.options.redactions.includes(secret));
  }
});
