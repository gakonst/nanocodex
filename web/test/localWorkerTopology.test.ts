import assert from "node:assert/strict";
import test from "node:test";

import { localManagedAuxiliaryWorkers } from "../vite/localWorkerTopology.ts";

test("local development mirrors the private Workers and same-session Connect API", () => {
  const [egress, managed, connectApi] = localManagedAuxiliaryWorkers({
    NANOCODEX_LOCAL_ADMIN_TOKEN: "signing-key",
    NANOCODEX_LOCAL_AGENT_IDLE_TIMEOUT_MS: "750",
    NANOCODEX_LOCAL_CHATGPT_BOOTSTRAP: "local-secret-document",
    NANOCODEX_LOCAL_CODEX_RELAY_URL: "http://127.0.0.1:49152/",
    NANOCODEX_LOCAL_GITHUB_OAUTH_CLIENT_ID: "github-client",
    NANOCODEX_LOCAL_GITHUB_OAUTH_CLIENT_SECRET: "github-secret",
    NANOCODEX_LOCAL_GOOGLE_OAUTH_CLIENT_ID: "google-client",
    NANOCODEX_LOCAL_GOOGLE_OAUTH_CLIENT_SECRET: "google-secret",
    NANOCODEX_LOCAL_X_OAUTH_CLIENT_ID: "x-client",
    NANOCODEX_LOCAL_X_OAUTH_CLIENT_SECRET: "x-secret",
    NANOCODEX_LOCAL_SLACK_OAUTH_CLIENT_ID: "slack-client",
    NANOCODEX_LOCAL_SLACK_OAUTH_CLIENT_SECRET: "slack-secret",
    OPENAI_API_KEY: "must-not-enter-managed-worker",
  });
  assert.equal(egress?.configPath, "../services/egress/wrangler.broker.jsonc");
  assert.deepEqual(egress?.config({ vars: { EXISTING: "kept" } }), {
    name: "nanocodex-egress",
    vars: {
      EXISTING: "kept",
      ENVIRONMENT: "development",
      ALLOW_LOCAL_CREDENTIAL_CLAIM: "true",
      ALLOW_INSECURE_LOOPBACK_RELAY: "true",
      CODEX_RELAY_URL: "http://127.0.0.1:49152/",
      LOCAL_CHATGPT_BOOTSTRAP: "local-secret-document",
      GITHUB_OAUTH_CLIENT_ID: "github-client",
      GITHUB_OAUTH_CLIENT_SECRET: "github-secret",
      GOOGLE_OAUTH_CLIENT_ID: "google-client",
      GOOGLE_OAUTH_CLIENT_SECRET: "google-secret",
      X_OAUTH_CLIENT_ID: "x-client",
      X_OAUTH_CLIENT_SECRET: "x-secret",
      SLACK_OAUTH_CLIENT_ID: "slack-client",
      SLACK_OAUTH_CLIENT_SECRET: "slack-secret",
    },
  });
  assert.equal(managed?.configPath, "../services/managed/wrangler.jsonc");
  assert.deepEqual(managed?.config({ vars: { EXISTING: "kept" } }), {
    name: "nanocodex-durable-agent",
    vars: {
      EXISTING: "kept",
      AGENT_IDLE_TIMEOUT_MS: "750",
      NANOCODEX_ADMIN_TOKEN: "signing-key",
      NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY: "nanocodex-local-oauth-relay-hmac-v1-only",
      NANOCODEX_LOCAL_WEBAUTHN_HMAC_KEY: "nanocodex-local-passkey-portability-v1",
    },
  });
  assert.equal(connectApi?.configPath, "../services/connect-api/wrangler.jsonc");
  assert.deepEqual(connectApi?.config({
    services: [
      { binding: "ACCOUNTS", service: "nanocodex-durable-agent" },
      { binding: "NANOCODEX", service: "nanocodex" },
    ],
    vars: { EXISTING: "kept" },
  }), {
    compatibility_date: "2026-08-18",
    name: "nanocodex-connect-api",
    services: [
      { binding: "ACCOUNTS", service: "nanocodex-durable-agent", remote: false },
      { binding: "EGRESS", service: "nanocodex-egress", remote: false },
      { binding: "NANOCODEX", service: "nanocodex-development", remote: false },
    ],
    vars: {
      EXISTING: "kept",
      NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY: "nanocodex-local-oauth-relay-hmac-v1-only",
    },
  });
});

test("passkey portability and OAuth routing use independent shared development keys", () => {
  const [, managed, connectApi] = localManagedAuxiliaryWorkers({
    NANOCODEX_LOCAL_WEBAUTHN_HMAC_KEY: "passkey-key-with-at-least-thirty-two-characters",
    NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY: "oauth-key-with-at-least-thirty-two-characters",
  });
  const managedVars = managed?.config({}).vars;
  const connectVars = connectApi?.config({}).vars;
  assert.equal(
    managedVars?.NANOCODEX_LOCAL_WEBAUTHN_HMAC_KEY,
    "passkey-key-with-at-least-thirty-two-characters",
  );
  assert.equal(
    managedVars?.NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY,
    "oauth-key-with-at-least-thirty-two-characters",
  );
  assert.equal(
    connectVars?.NANOCODEX_LOCAL_OAUTH_RELAY_HMAC_KEY,
    "oauth-key-with-at-least-thirty-two-characters",
  );
  assert.equal("NANOCODEX_LOCAL_WEBAUTHN_HMAC_KEY" in (connectVars ?? {}), false);
});

test("local managed defaults are immediately runnable and validate only policy", () => {
  assert.equal(localManagedAuxiliaryWorkers({}).length, 3);
  assert.throws(
    () => localManagedAuxiliaryWorkers({ NANOCODEX_LOCAL_AGENT_IDLE_TIMEOUT_MS: "0" }),
    /positive integer/,
  );
  assert.throws(
    () => localManagedAuxiliaryWorkers({ NANOCODEX_LOCAL_GOOGLE_OAUTH_CLIENT_ID: "incomplete" }),
    /Google OAuth client ID and secret must be configured together/,
  );
  assert.throws(
    () => localManagedAuxiliaryWorkers({ NANOCODEX_LOCAL_X_OAUTH_CLIENT_ID: "incomplete" }),
    /X OAuth client ID and secret must be configured together/,
  );
});
