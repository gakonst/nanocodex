import assert from "node:assert/strict";
import test from "node:test";

import {
  chatGptCredentialImportDigest,
  chatGptCredentialImportResource,
  credentialImportDigestFromResources,
  parseChatGptCredentialImport,
} from "../src/chatGptCredentialImport.mts";

const vectors = [
  {
    credential: {
      access_token: "access",
      refresh_token: "refresh",
      account_id: "acct_123",
      expires_at: 1_700_000_000_123,
      fedramp: false,
    },
    digest: "vo_PpDlpaEWBzcjBCi0CpMQsPYiutjEMtb6HsNBjhng",
  },
  {
    credential: {
      access_token: "eyJα.雪.sig",
      refresh_token: "refresh-🔐",
      account_id: "org_é",
      expires_at: Number.MAX_SAFE_INTEGER,
      fedramp: true,
    },
    digest: "NfVf_UJoMpn0w_fNtHoO7wyCd-23xEXOkmFhM2uLv-M",
  },
];

test("ChatGPT credential commitments match fixed big-endian UTF-8 vectors", async () => {
  for (const vector of vectors) {
    assert.equal(await chatGptCredentialImportDigest(vector.credential), vector.digest);
    assert.equal(
      await chatGptCredentialImportResource(vector.credential),
      `urn:nanocodex:credential-import:chatgpt:codex-auth-v1:sha256:${vector.digest}`,
    );
  }
});

test("ChatGPT credential import accepts exactly five bounded fields", () => {
  assert.deepEqual(parseChatGptCredentialImport(vectors[0].credential), vectors[0].credential);
  for (const invalid of [
    { ...vectors[0].credential, extra: "no" },
    { ...vectors[0].credential, refresh_token: "" },
    { ...vectors[0].credential, account_id: "a".repeat(257) },
    { ...vectors[0].credential, access_token: "a".repeat(32 * 1024 + 1) },
    { ...vectors[0].credential, expires_at: 1.5 },
    { ...vectors[0].credential, fedramp: 0 },
    { ...vectors[0].credential, account_id: "\ud800" },
  ]) {
    assert.throws(() => parseChatGptCredentialImport(invalid));
  }
});

test("credential import resources reject malformed and duplicate commitments", () => {
  const resource = `urn:nanocodex:credential-import:chatgpt:codex-auth-v1:sha256:${vectors[0].digest}`;
  assert.equal(credentialImportDigestFromResources([resource]), vectors[0].digest);
  assert.equal(credentialImportDigestFromResources(["urn:nanocodex:agent:run"]), undefined);
  assert.throws(() => credentialImportDigestFromResources([
    "urn:nanocodex:credential-import:chatgpt:codex-auth-v1:sha256:short",
  ]));
  assert.throws(() => credentialImportDigestFromResources([resource, resource]));
  assert.throws(() => credentialImportDigestFromResources([
    `urn:nanocodex:credential-import:other:sha256:${vectors[0].digest}`,
  ]));
});
