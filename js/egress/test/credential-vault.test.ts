import { describe, expect, it, vi } from "vitest";

import { CredentialVault, type SecretsStoreSecret } from "../src/credential-vault";

const TEST_KEY = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY";

describe("credential vault Secrets Store key binding", () => {
  it("gets the broker encryption key without persisting or returning its value", async () => {
    const get = vi.fn(async () => TEST_KEY);
    const binding: SecretsStoreSecret = { get };
    const vault = new CredentialVault({
      ENVIRONMENT: "production",
      CREDENTIAL_ENCRYPTION_KEY: binding,
    }, "user/test");

    const envelope = await vault.seal({ secret: "user-supplied-api-key" });
    expect(get).toHaveBeenCalledOnce();
    expect(JSON.stringify(envelope)).not.toContain(TEST_KEY);
    expect(JSON.stringify(envelope)).not.toContain("user-supplied-api-key");

    const opened = await vault.open<{ secret: string }>(envelope);
    expect(opened.value).toEqual({ secret: "user-supplied-api-key" });
    expect(get).toHaveBeenCalledTimes(2);
  });

  it("fails closed when the bound key cannot be read", async () => {
    const binding: SecretsStoreSecret = {
      get: async () => { throw new Error("binding unavailable"); },
    };
    const vault = new CredentialVault({
      ENVIRONMENT: "production",
      CREDENTIAL_ENCRYPTION_KEY: binding,
    }, "user/test");

    await expect(vault.seal({ secret: "user-supplied-api-key" }))
      .rejects.toThrow("binding unavailable");
  });
});
