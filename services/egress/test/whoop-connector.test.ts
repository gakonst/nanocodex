import { describe, expect, it } from "vitest";

import {
  buildWhoopAuthorizationParams,
  buildWhoopIdentityRequest,
  buildWhoopRefreshRequest,
  buildWhoopRevocationRequest,
  buildWhoopTokenRequest,
  decodeWhoopIdentity,
  decodeWhoopTokenResponse,
  WHOOP_PROVIDER,
} from "../src/connectors/whoop";

describe("WHOOP OAuth connector", () => {
  it("requests durable, read-only health scopes", () => {
    expect(WHOOP_PROVIDER.authorizationUrl).toBe(
      "https://api.prod.whoop.com/oauth/oauth2/auth",
    );
    expect(WHOOP_PROVIDER.tokenUrl).toBe(
      "https://api.prod.whoop.com/oauth/oauth2/token",
    );
    expect(WHOOP_PROVIDER.identityUrl).toBe(
      "https://api.prod.whoop.com/developer/v2/user/profile/basic",
    );
    expect(WHOOP_PROVIDER.revocationUrl).toBe(
      "https://api.prod.whoop.com/developer/v2/user/access",
    );
    expect(WHOOP_PROVIDER.scopes).toEqual([
      "offline",
      "read:profile",
      "read:body_measurement",
      "read:cycles",
      "read:recovery",
      "read:sleep",
      "read:workout",
    ]);
  });

  it("builds a state-bound authorization request without unsupported PKCE fields", () => {
    const params = buildWhoopAuthorizationParams({
      clientId: "whoop-client",
      redirectUri: "https://nanocodex.example/v1/connectors/whoop/callback",
      state: "opaque-state",
    });
    expect(Object.fromEntries(params)).toEqual({
      response_type: "code",
      client_id: "whoop-client",
      redirect_uri: "https://nanocodex.example/v1/connectors/whoop/callback",
      scope: WHOOP_PROVIDER.scopes.join(" "),
      state: "opaque-state",
    });
    expect(params.has("code_challenge")).toBe(false);
  });

  it("exchanges and rotates confidential-client tokens without URL credentials", async () => {
    const exchange = buildWhoopTokenRequest({
      clientId: "whoop-client",
      clientSecret: "whoop-secret",
      code: "authorization-code",
      redirectUri: "https://nanocodex.example/v1/connectors/whoop/callback",
    });
    expect(exchange.url).toBe(WHOOP_PROVIDER.tokenUrl);
    expect(exchange.url).not.toContain("whoop-secret");
    expect(Object.fromEntries(await exchange.formData())).toEqual({
      grant_type: "authorization_code",
      code: "authorization-code",
      client_id: "whoop-client",
      client_secret: "whoop-secret",
      redirect_uri: "https://nanocodex.example/v1/connectors/whoop/callback",
    });

    const refresh = buildWhoopRefreshRequest(
      "whoop-client",
      "whoop-secret",
      "whoop-refresh",
    );
    expect(Object.fromEntries(await refresh.formData())).toEqual({
      grant_type: "refresh_token",
      refresh_token: "whoop-refresh",
      client_id: "whoop-client",
      client_secret: "whoop-secret",
      scope: "offline",
    });
  });

  it("decodes rotating tokens and the authenticated member", () => {
    expect(decodeWhoopTokenResponse({
      access_token: "whoop-access",
      refresh_token: "whoop-refresh",
      expires_in: 3_600,
      scope: WHOOP_PROVIDER.scopes.join(" "),
      token_type: "bearer",
    })).toEqual({
      accessToken: "whoop-access",
      refreshToken: "whoop-refresh",
      expiresIn: 3_600,
      scopes: [...WHOOP_PROVIDER.scopes],
      tokenType: "bearer",
    });

    const identity = buildWhoopIdentityRequest("whoop-access");
    expect(identity.headers.get("authorization")).toBe("Bearer whoop-access");
    expect(decodeWhoopIdentity({
      user_id: 10_129,
      email: "member@example.test",
      first_name: "Nano",
      last_name: "Athlete",
    })).toEqual({
      accountId: "10129",
      displayLabel: "Nano Athlete (member@example.test)",
    });
  });

  it("revokes only through the authenticated WHOOP access endpoint", () => {
    const request = buildWhoopRevocationRequest("whoop-access");
    expect(request.url).toBe(WHOOP_PROVIDER.revocationUrl);
    expect(request.method).toBe("DELETE");
    expect(request.headers.get("authorization")).toBe("Bearer whoop-access");
  });

  it("rejects incomplete scopes and malformed identities", () => {
    expect(() => decodeWhoopTokenResponse({
      access_token: "whoop-access",
      refresh_token: "whoop-refresh",
      expires_in: 3_600,
      scope: "offline read:profile",
      token_type: "bearer",
    })).toThrow("invalid WHOOP token response");
    expect(() => decodeWhoopIdentity({
      user_id: "10129",
      email: "member@example.test",
      first_name: "Nano",
      last_name: "Athlete",
    })).toThrow("invalid WHOOP identity response");
  });
});
