import { describe, expect, it } from "vitest";

import { decodeHeaders } from "../src/browser-egress";

const VAULT_ID = "v".repeat(22);

describe("Browser egress vault headers", () => {
  it("accepts only closed vault placeholder forms with a valid vault reference", () => {
    const basic = decodeHeaders({
      "x-nanocodex-vault-id": VAULT_ID,
      authorization: "Basic {{NANOCODEX_VAULT_BASIC}}",
      "x-api-key": "{{NANOCODEX_VAULT_PASSWORD}}",
    });
    expect(basic && Object.fromEntries(basic)).toEqual({
      authorization: "Basic {{NANOCODEX_VAULT_BASIC}}",
      "x-api-key": "{{NANOCODEX_VAULT_PASSWORD}}",
      "x-nanocodex-vault-id": VAULT_ID,
    });

    expect(decodeHeaders({
      "x-nanocodex-vault-id": VAULT_ID,
      authorization: "Bearer {{NANOCODEX_VAULT_PASSWORD}}",
    })).toBeInstanceOf(Headers);
    expect(decodeHeaders({
      authorization: "Bearer {{NANOCODEX_VAULT_PASSWORD}}",
    })).toBeUndefined();
    expect(decodeHeaders({
      "x-nanocodex-vault-id": "invalid",
      authorization: "Bearer {{NANOCODEX_VAULT_PASSWORD}}",
    })).toBeUndefined();
  });

  it("rejects raw authorization, cookie, credential, and subject headers", () => {
    const cases: Array<Record<string, string>> = [
      { "x-nanocodex-vault-id": VAULT_ID, authorization: "Bearer raw-secret" },
      { "x-nanocodex-vault-id": VAULT_ID, cookie: "session=raw-secret" },
      { "x-nanocodex-vault-id": VAULT_ID, cookie: "{{NANOCODEX_VAULT_PASSWORD}}" },
      { "x-nanocodex-vault-id": VAULT_ID, "x-api-key": "raw-secret" },
      { "x-nanocodex-vault-id": VAULT_ID, "x-nanocodex-subject": "s".repeat(43) },
    ];
    for (const headers of cases) {
      expect(decodeHeaders(headers)).toBeUndefined();
    }
  });

  it("keeps ordinary credential-free headers unchanged", () => {
    expect(Object.fromEntries(decodeHeaders({
      accept: "application/json",
      "x-request-id": "public-id",
    })!)).toEqual({ accept: "application/json", "x-request-id": "public-id" });
  });
});
