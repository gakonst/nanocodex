import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  redactSecrets,
  runBoundedProcess,
} from "../../managed/scripts/child-process.mjs";

const directory = resolve(fileURLToPath(new URL("..", import.meta.url)));
const wranglerPath = join(directory, "node_modules/wrangler/bin/wrangler.js");

export function productionBrokerSecrets(environment) {
  const encryptionKey = required(environment.NANOCODEX_CREDENTIAL_ENCRYPTION_KEY,
    "NANOCODEX_CREDENTIAL_ENCRYPTION_KEY");
  const probeToken = required(environment.NANOCODEX_BROKER_PROBE_TOKEN,
    "NANOCODEX_BROKER_PROBE_TOKEN");
  const githubClientId = required(environment.NANOCODEX_GITHUB_OAUTH_CLIENT_ID,
    "NANOCODEX_GITHUB_OAUTH_CLIENT_ID");
  const githubClientSecret = required(environment.NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET,
    "NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET");
  const googleClientId = required(environment.NANOCODEX_GOOGLE_OAUTH_CLIENT_ID,
    "NANOCODEX_GOOGLE_OAUTH_CLIENT_ID");
  const googleClientSecret = required(environment.NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET,
    "NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET");
  const xClientId = optional(environment.NANOCODEX_X_OAUTH_CLIENT_ID,
    "NANOCODEX_X_OAUTH_CLIENT_ID");
  const xClientSecret = optional(environment.NANOCODEX_X_OAUTH_CLIENT_SECRET,
    "NANOCODEX_X_OAUTH_CLIENT_SECRET");
  const whoopClientId = optional(environment.NANOCODEX_WHOOP_OAUTH_CLIENT_ID,
    "NANOCODEX_WHOOP_OAUTH_CLIENT_ID");
  const whoopClientSecret = optional(environment.NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET,
    "NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET");
  if ((xClientId === undefined) !== (xClientSecret === undefined)) {
    throw new Error("X OAuth application credentials must be configured together");
  }
  if ((whoopClientId === undefined) !== (whoopClientSecret === undefined)) {
    throw new Error("WHOOP OAuth application credentials must be configured together");
  }
  if (!/^[A-Za-z0-9_-]{43}$/.test(encryptionKey)) {
    throw new Error("NANOCODEX_CREDENTIAL_ENCRYPTION_KEY must be a 32-byte base64url value");
  }
  if (probeToken.length < 32 || probeToken.length > 512) {
    throw new Error("NANOCODEX_BROKER_PROBE_TOKEN must be 32-512 characters");
  }
  const secrets = {
    CREDENTIAL_ENCRYPTION_KEY: encryptionKey,
    NANOCODEX_BROKER_PROBE_TOKEN: probeToken,
    GITHUB_OAUTH_CLIENT_ID: githubClientId,
    GITHUB_OAUTH_CLIENT_SECRET: githubClientSecret,
    GOOGLE_OAUTH_CLIENT_ID: googleClientId,
    GOOGLE_OAUTH_CLIENT_SECRET: googleClientSecret,
  };
  if (xClientId !== undefined && xClientSecret !== undefined) {
    secrets.X_OAUTH_CLIENT_ID = xClientId;
    secrets.X_OAUTH_CLIENT_SECRET = xClientSecret;
  }
  if (whoopClientId !== undefined && whoopClientSecret !== undefined) {
    secrets.WHOOP_OAUTH_CLIENT_ID = whoopClientId;
    secrets.WHOOP_OAUTH_CLIENT_SECRET = whoopClientSecret;
  }
  const previousEncryptionKey = environment.NANOCODEX_CREDENTIAL_ENCRYPTION_KEY_PREVIOUS;
  if (previousEncryptionKey !== undefined) {
    const previous = required(previousEncryptionKey,
      "NANOCODEX_CREDENTIAL_ENCRYPTION_KEY_PREVIOUS");
    if (!/^[A-Za-z0-9_-]{43}$/.test(previous)) {
      throw new Error(
        "NANOCODEX_CREDENTIAL_ENCRYPTION_KEY_PREVIOUS must be a 32-byte base64url value",
      );
    }
    secrets.CREDENTIAL_ENCRYPTION_KEY_PREVIOUS = previous;
  }
  return secrets;
}

export function productionRevision(environment) {
  const revision = required(environment.TARGET_SHA, "TARGET_SHA");
  if (!/^[0-9a-f]{40}$/.test(revision)) {
    throw new Error("TARGET_SHA must be a full lowercase Git commit SHA");
  }
  return revision;
}

export function brokerWranglerEnvironment(environment, accountId, apiToken) {
  const clean = { ...environment };
  for (const name of [
    "OPENAI_API_KEY",
    "CODEX_OAUTH_BOOTSTRAP",
    "LOCAL_CHATGPT_BOOTSTRAP",
    "NANOCODEX_MANAGED_AUTH_MODE",
    "NANOCODEX_MANAGED_OPENAI_API_KEY",
    "NANOCODEX_MANAGED_CODEX_OAUTH_BOOTSTRAP",
    "NANOCODEX_CREDENTIAL_ENCRYPTION_KEY",
    "NANOCODEX_CREDENTIAL_ENCRYPTION_KEY_PREVIOUS",
    "NANOCODEX_BROKER_PROBE_TOKEN",
    "NANOCODEX_GITHUB_OAUTH_CLIENT_ID",
    "NANOCODEX_GITHUB_OAUTH_CLIENT_SECRET",
    "NANOCODEX_GOOGLE_OAUTH_CLIENT_ID",
    "NANOCODEX_GOOGLE_OAUTH_CLIENT_SECRET",
    "NANOCODEX_X_OAUTH_CLIENT_ID",
    "NANOCODEX_X_OAUTH_CLIENT_SECRET",
    "NANOCODEX_WHOOP_OAUTH_CLIENT_ID",
    "NANOCODEX_WHOOP_OAUTH_CLIENT_SECRET",
  ]) delete clean[name];
  clean.CLOUDFLARE_ACCOUNT_ID = accountId;
  if (apiToken) clean.CLOUDFLARE_API_TOKEN = apiToken;
  else delete clean.CLOUDFLARE_API_TOKEN;
  return clean;
}

export function buildProductionBrokerConfig(base, { mainPath }) {
  const bindings = base.durable_objects?.bindings ?? [];
  if (!bindings.some((binding) => binding.name === "USER_CREDENTIALS"
      && binding.class_name === "UserCredentialBroker")
    || !bindings.some((binding) => binding.name === "AGENT_SUBJECTS"
      && binding.class_name === "AgentSubjectDirectory")
    || !bindings.some((binding) => binding.name === "USER_CONNECTORS"
      && binding.class_name === "UserConnectorBroker")
    || !bindings.some((binding) => binding.name === "MCP_CONNECTIONS"
      && binding.class_name === "McpConnectionDirectory")
    || !bindings.some((binding) => binding.name === "CHATGPT_EGRESS"
      && binding.class_name === "ChatGptEgress"
      && binding.script_name === "nanocodex")) {
    throw new Error("production broker requires credential, subject, and ChatGPT relay DO bindings");
  }
  const creates = base.migrations?.find((migration) => migration.tag === "v2");
  const deletes = base.migrations?.find((migration) => migration.tag === "v3");
  const connectorCreates = base.migrations?.find((migration) => migration.tag === "v4");
  const mcpCreates = base.migrations?.find((migration) => migration.tag === "v5");
  if (!creates?.new_sqlite_classes?.includes("UserCredentialBroker")
    || !creates.new_sqlite_classes.includes("AgentSubjectDirectory")
    || !deletes?.deleted_classes?.includes("CodexOAuthBroker")
    || !connectorCreates?.new_sqlite_classes?.includes("UserConnectorBroker")
    || !mcpCreates?.new_sqlite_classes?.includes("McpConnectionDirectory")) {
    throw new Error("production broker requires the current v2/v3/v4/v5 DO migration chain");
  }
  return {
    ...base,
    main: mainPath,
    workers_dev: false,
    routes: undefined,
    vars: { ENVIRONMENT: "production" },
  };
}

export async function withPrivateBrokerFiles(files, callback) {
  const temporary = await mkdtemp(join(tmpdir(), "nanocodex-egress-"));
  try {
    const paths = {};
    for (const [name, value] of Object.entries(files)) {
      const path = join(temporary, name);
      await writeFile(path, `${JSON.stringify(value)}\n`, { mode: 0o600 });
      paths[name] = path;
    }
    return await callback({ ...paths, directory: temporary });
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

export async function deployProductionBroker(environment = process.env, {
  run = runWrangler,
} = {}) {
  const accountId = required(environment.CLOUDFLARE_ACCOUNT_ID, "CLOUDFLARE_ACCOUNT_ID");
  const apiToken = environment.CLOUDFLARE_API_TOKEN === undefined
    ? undefined
    : required(environment.CLOUDFLARE_API_TOKEN, "CLOUDFLARE_API_TOKEN");
  const revision = productionRevision(environment);
  const secrets = productionBrokerSecrets(environment);
  const base = JSON.parse(await readFile(join(directory, "wrangler.broker.jsonc"), "utf8"));
  const config = buildProductionBrokerConfig(base, { mainPath: join(directory, "src/egress.ts") });
  await withPrivateBrokerFiles({ "wrangler.json": config, "secrets.json": secrets }, async (paths) => {
    const childEnv = brokerWranglerEnvironment(environment, accountId, apiToken);
    await run([
      "deploy",
      "--config",
      paths["wrangler.json"],
      "--strict",
      "--tag",
      revision,
      "--message",
      `gakonst/nanocodex@${revision}`,
      "--var",
      `DEPLOYMENT_SHA:${revision}`,
      "--secrets-file",
      paths["secrets.json"],
    ], {
      environment: childEnv,
      redactions: [apiToken, ...Object.values(secrets)],
    });
  });
}

function required(value, name) {
  if (!value || value.trim() !== value || /[\u0000-\u001f\u007f]/.test(value)) {
    throw new Error(`${name} is required and must be one line`);
  }
  return value;
}

function optional(value, name) {
  if (value === undefined || value === "") return undefined;
  return required(value, name);
}

function runWrangler(arguments_, { environment, redactions }) {
  return runBoundedProcess(process.execPath, [wranglerPath, ...arguments_], {
    cwd: directory,
    env: environment,
    label: `production broker Wrangler ${arguments_[0] ?? "command"}`,
    maxOutputBytes: 64 * 1024,
    redact: (value) => redactSecrets(value, redactions),
    timeoutMs: 180_000,
  });
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  deployProductionBroker().catch((error) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
