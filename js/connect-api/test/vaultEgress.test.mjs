import assert from "node:assert/strict";
import test from "node:test";

import { vaultEgressEnvelope } from "../src/vaultEgress.mjs";

const vaultId = "v".repeat(32);

test("Vault egress extracts only an exact opaque reference and strips its reserved header", () => {
  assert.deepEqual(vaultEgressEnvelope({
    thread_id: "00000000-0000-4000-8000-000000000000",
    url: "https://example.test/session",
    method: "post",
    headers: {
      "x-nanocodex-vault-id": vaultId,
      authorization: "Basic {{NANOCODEX_VAULT_BASIC}}",
      accept: "application/json",
    },
    body: "username={{NANOCODEX_VAULT_USERNAME}}",
    ignored: "not forwarded",
  }), {
    vault_id: vaultId,
    url: "https://example.test/session",
    method: "POST",
    headers: {
      accept: "application/json",
      authorization: "Basic {{NANOCODEX_VAULT_BASIC}}",
    },
    body: "username={{NANOCODEX_VAULT_USERNAME}}",
  });
});

test("Vault egress leaves ordinary envelopes alone and recognizes no reserved-header aliases", () => {
  assert.equal(vaultEgressEnvelope({ url: "https://example.test", headers: { accept: "*/*" } }), undefined);
  assert.equal(vaultEgressEnvelope({
    url: "https://example.test",
    headers: { "X-Nanocodex-Vault-Id": vaultId },
  }), undefined);
});

test("Vault egress rejects invalid references, request bounds, and caller-supplied secrets", () => {
  const request = (overrides = {}) => ({
    url: "https://example.test/session",
    method: "POST",
    headers: {
      "x-nanocodex-vault-id": vaultId,
      authorization: "Bearer {{NANOCODEX_VAULT_PASSWORD}}",
    },
    ...overrides,
  });
  const invalid = [
    request({ headers: { "x-nanocodex-vault-id": "short", authorization: "Bearer {{NANOCODEX_VAULT_PASSWORD}}" } }),
    request({ url: `https://example.test/${"x".repeat(8_193)}` }),
    request({ method: "TRACE" }),
    request({ method: "GET", body: "{{NANOCODEX_VAULT_PASSWORD}}" }),
    request({ headers: { "x-nanocodex-vault-id": vaultId, authorization: "Bearer plaintext-secret" } }),
    request({ headers: { "x-nanocodex-vault-id": vaultId, cookie: "{{NANOCODEX_VAULT_PASSWORD}}" } }),
    request({ headers: { "x-nanocodex-vault-id": vaultId, "x-nanocodex-subject": "forged", accept: "{{NANOCODEX_VAULT_PASSWORD}}" } }),
    request({ headers: { "x-nanocodex-vault-id": vaultId, accept: "application/json" } }),
  ];
  for (const value of invalid) assert.throws(() => vaultEgressEnvelope(value));
});

test("API key placeholders pass through bearer and custom headers", () => {
  const headers = { authorization: "Bearer {{NANOCODEX_VAULT_API_KEY}}", "x-api-key": "{{NANOCODEX_VAULT_API_KEY}}" };
  assert.deepEqual(vaultEgressEnvelope({ url: "https://example.com/api", headers: { ...headers, "x-nanocodex-vault-id": vaultId } }),
    { vault_id: vaultId, url: "https://example.com/api", method: "GET", headers });
});
