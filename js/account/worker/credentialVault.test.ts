import assert from "node:assert/strict";
import { test } from "node:test";

import { CredentialVault } from "./credentialVault.ts";

const CURRENT_KEY = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const PREVIOUS_KEY = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE";

test("credential vault round-trips without storing plaintext", async () => {
  const vault = new CredentialVault(
    { ENVIRONMENT: "production", SESSION_CREDENTIAL_KEY: CURRENT_KEY },
    "chatgpt/session-1",
  );
  const envelope = await vault.seal({ accessToken: "access-secret", refreshToken: "refresh-secret" });

  assert.equal(envelope.version, 1);
  assert.doesNotMatch(JSON.stringify(envelope), /access-secret|refresh-secret/);
  assert.deepEqual((await vault.open(envelope)).value, {
    accessToken: "access-secret",
    refreshToken: "refresh-secret",
  });
});

test("credential vault binds ciphertext to one Durable Object and supports key rotation", async () => {
  const oldVault = new CredentialVault(
    { ENVIRONMENT: "production", SESSION_CREDENTIAL_KEY: PREVIOUS_KEY },
    "byok/session-1",
  );
  const envelope = await oldVault.seal({ apiKey: "sk-user" });
  const rotatedVault = new CredentialVault(
    {
      ENVIRONMENT: "production",
      SESSION_CREDENTIAL_KEY: CURRENT_KEY,
      SESSION_CREDENTIAL_KEY_PREVIOUS: PREVIOUS_KEY,
    },
    "byok/session-1",
  );

  const opened = await rotatedVault.open<{ apiKey: string }>(envelope);
  assert.deepEqual(opened, { value: { apiKey: "sk-user" }, reseal: true });
  await assert.rejects(
    new CredentialVault(
      { ENVIRONMENT: "production", SESSION_CREDENTIAL_KEY: PREVIOUS_KEY },
      "byok/session-2",
    ).open(envelope),
    /failed authentication/,
  );
});

test("credential vault fails closed without a production encryption key", () => {
  assert.throws(
    () => new CredentialVault({ ENVIRONMENT: "production" }, "byok/session-1"),
    /SESSION_CREDENTIAL_KEY is required/,
  );
});
