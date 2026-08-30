type WorkerConfiguration = {
  compatibility_date?: string;
  name?: string;
  services?: Array<{ binding: string; service: string; remote?: boolean }>;
  vars?: Record<string, unknown>;
};

type AuxiliaryWorker = {
  configPath: string;
  devOnly: true;
  config: (configuration: WorkerConfiguration) => WorkerConfiguration;
};

const LOCAL_MANAGED_WORKER = "nanocodex-durable-agent";
const LOCAL_EGRESS_WORKER = "nanocodex-egress";
const LOCAL_CONNECT_API_WORKER = "nanocodex-connect-api";
const LOCAL_CONNECT_API_COMPATIBILITY_DATE = "2026-08-18";
const LOCAL_WEBSITE_WORKER = "nanocodex-development";
const DEVELOPMENT_SIGNING_KEY = "nanocodex-local-room-signing-key";
const DEVELOPMENT_WEBAUTHN_HMAC_KEY = "nanocodex-local-passkey-portability-v1";
const DEVELOPMENT_OAUTH_RELAY_HMAC_KEY = "nanocodex-local-oauth-relay-hmac-v1-only";

/**
 * Cloudflare requires Workers that share external Durable Objects or upgraded
 * Service Binding responses to run in one local multi-Worker session. Keep the
 * provider credential broker and managed Worker in the same local session so
 * account, credential, agent, and room routes use the production topology.
 */
export function localManagedAuxiliaryWorkers(
  environment: NodeJS.ProcessEnv = process.env,
): AuxiliaryWorker[] {
  const signingKey = environment.NANOCODEX_LOCAL_ADMIN_TOKEN?.trim()
    || DEVELOPMENT_SIGNING_KEY;
  const idleTimeout = environment.NANOCODEX_LOCAL_AGENT_IDLE_TIMEOUT_MS?.trim() || "1000";
  const relayUrl = environment.NANOCODEX_LOCAL_CODEX_RELAY_URL?.trim();
  const webAuthnHmacKey = environment.NANOCODEX_LOCAL_WEBAUTHN_HMAC_KEY?.trim()
    || DEVELOPMENT_WEBAUTHN_HMAC_KEY;
  const oauthRelayHmacKey = environment.NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY?.trim()
    || DEVELOPMENT_OAUTH_RELAY_HMAC_KEY;
  const connectorVars = localConnectorVars(environment);
  if (!/^[1-9][0-9]*$/.test(idleTimeout)) {
    throw new Error("local managed Worker idle timeout must be a positive integer");
  }

  return [
    {
      configPath: "../services/egress/wrangler.broker.jsonc",
      devOnly: true,
      config: (configuration) => ({
        name: LOCAL_EGRESS_WORKER,
        vars: {
          ...configuration.vars,
          ENVIRONMENT: "development",
          ALLOW_LOCAL_CREDENTIAL_CLAIM: "true",
          ...(relayUrl
            ? {
                ALLOW_INSECURE_LOOPBACK_RELAY: "true",
                CODEX_RELAY_URL: relayUrl,
              }
            : {}),
          ...(environment.NANOCODEX_LOCAL_CHATGPT_BOOTSTRAP
            ? { LOCAL_CHATGPT_BOOTSTRAP: environment.NANOCODEX_LOCAL_CHATGPT_BOOTSTRAP }
            : {}),
          ...connectorVars,
        },
      }),
    },
    {
      configPath: "../services/managed/wrangler.jsonc",
      devOnly: true,
      config: (configuration) => ({
        // CLOUDFLARE_ENV applies to every Worker in the Vite session. Pin the
        // auxiliary name so Service Bindings resolve the production names.
        name: LOCAL_MANAGED_WORKER,
        vars: {
          ...configuration.vars,
          AGENT_IDLE_TIMEOUT_MS: idleTimeout,
          NANOCODEX_ADMIN_TOKEN: signingKey,
          // This development-only MAC key is intentionally stable across
          // isolated worktrees. It authenticates public passkey metadata in a
          // shared browser cookie; it is not a provider credential or a
          // production trust root.
          NANOCODEX_LOCAL_WEBAUTHN_HMAC_KEY: webAuthnHmacKey,
          NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY: oauthRelayHmacKey,
        },
      }),
    },
    {
      configPath: "../services/connect-api/wrangler.jsonc",
      devOnly: true,
      config: (configuration) => ({
        compatibility_date: LOCAL_CONNECT_API_COMPATIBILITY_DATE,
        name: LOCAL_CONNECT_API_WORKER,
        services: [
          { binding: "ACCOUNTS", service: LOCAL_MANAGED_WORKER, remote: false },
          { binding: "EGRESS", service: LOCAL_EGRESS_WORKER, remote: false },
          { binding: "NANOCODEX", service: LOCAL_WEBSITE_WORKER, remote: false },
        ],
        vars: {
          ...configuration.vars,
          NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY: oauthRelayHmacKey,
        },
      }),
    },
  ];
}

function localConnectorVars(environment: NodeJS.ProcessEnv): Record<string, string> {
  return {
    ...credentialPair(environment, {
      id: "NANOCODEX_LOCAL_GITHUB_OAUTH_CLIENT_ID",
      secret: "NANOCODEX_LOCAL_GITHUB_OAUTH_CLIENT_SECRET",
      targetId: "GITHUB_OAUTH_CLIENT_ID",
      targetSecret: "GITHUB_OAUTH_CLIENT_SECRET",
      label: "GitHub",
    }),
    ...credentialPair(environment, {
      id: "NANOCODEX_LOCAL_GOOGLE_OAUTH_CLIENT_ID",
      secret: "NANOCODEX_LOCAL_GOOGLE_OAUTH_CLIENT_SECRET",
      targetId: "GOOGLE_OAUTH_CLIENT_ID",
      targetSecret: "GOOGLE_OAUTH_CLIENT_SECRET",
      label: "Google",
    }),
    ...credentialPair(environment, {
      id: "NANOCODEX_LOCAL_X_OAUTH_CLIENT_ID",
      secret: "NANOCODEX_LOCAL_X_OAUTH_CLIENT_SECRET",
      targetId: "X_OAUTH_CLIENT_ID",
      targetSecret: "X_OAUTH_CLIENT_SECRET",
      label: "X",
    }),
    ...credentialPair(environment, {
      id: "NANOCODEX_LOCAL_WHOOP_OAUTH_CLIENT_ID",
      secret: "NANOCODEX_LOCAL_WHOOP_OAUTH_CLIENT_SECRET",
      targetId: "WHOOP_OAUTH_CLIENT_ID",
      targetSecret: "WHOOP_OAUTH_CLIENT_SECRET",
      label: "WHOOP",
    }),
  };
}

function credentialPair(
  environment: NodeJS.ProcessEnv,
  names: { id: string; secret: string; targetId: string; targetSecret: string; label: string },
): Record<string, string> {
  const id = environment[names.id]?.trim();
  const secret = environment[names.secret]?.trim();
  if (Boolean(id) !== Boolean(secret)) {
    throw new Error(`local ${names.label} OAuth client ID and secret must be configured together`);
  }
  return id && secret ? { [names.targetId]: id, [names.targetSecret]: secret } : {};
}
