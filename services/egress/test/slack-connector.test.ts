import { describe, expect, it } from "vitest";

import {
  buildSlackAuthorizationUrl,
  buildSlackRevocationRequest,
  buildSlackTokenRefreshRequest,
  buildSlackTokenRequest,
  decodeSlackRefreshResponse,
  decodeSlackTokenResponse,
  SLACK_PROVIDER,
} from "../src/connectors/slack";

describe("Slack user connector", () => {
  it("requests bounded user scopes that let the connector act as the user", () => {
    expect(SLACK_PROVIDER.userScopes).toContain("chat:write");
    expect(SLACK_PROVIDER.userScopes).toContain("channels:history");
    expect(SLACK_PROVIDER.userScopes).toContain("im:write");
    expect(SLACK_PROVIDER.userScopes).toContain("reactions:write");
    expect(SLACK_PROVIDER.userScopes.some((scope) => scope.startsWith("admin."))).toBe(false);

    const url = buildSlackAuthorizationUrl({
      clientId: "client-id",
      redirectUri: "https://connect.example/v1/connectors/slack/callback",
      state: "unguessable-state",
    });
    expect(url.origin + url.pathname).toBe(SLACK_PROVIDER.authorizationUrl);
    expect(url.searchParams.get("scope")).toBeNull();
    expect(url.searchParams.get("user_scope")).toBe(SLACK_PROVIDER.userScopes.join(","));
    expect(url.searchParams.get("state")).toBe("unguessable-state");
  });

  it("exchanges and refreshes OAuth user tokens", async () => {
    const exchange = buildSlackTokenRequest({
      clientId: "client-id",
      clientSecret: "client-secret",
      code: "code",
      redirectUri: "https://connect.example/callback",
    });
    expect(exchange.url).toBe(SLACK_PROVIDER.tokenUrl);
    expect(Object.fromEntries(await exchange.formData())).toEqual({
      client_id: "client-id",
      client_secret: "client-secret",
      code: "code",
      redirect_uri: "https://connect.example/callback",
    });

    const refresh = buildSlackTokenRefreshRequest({
      clientId: "client-id",
      clientSecret: "client-secret",
      refreshToken: "refresh-secret",
    });
    expect(Object.fromEntries(await refresh.formData())).toEqual({
      client_id: "client-id",
      client_secret: "client-secret",
      grant_type: "refresh_token",
      refresh_token: "refresh-secret",
    });
  });

  it("decodes the authed_user token and workspace identity", () => {
    expect(decodeSlackTokenResponse({
      ok: true,
      team: { id: "T123", name: "Acme" },
      authed_user: {
        id: "U456",
        scope: SLACK_PROVIDER.userScopes.join(","),
        access_token: "xoxp-access-secret",
        token_type: "user",
        refresh_token: "xoxe-refresh-secret",
        expires_in: 43_200,
      },
    })).toEqual({
      accessToken: "xoxp-access-secret",
      refreshToken: "xoxe-refresh-secret",
      expiresIn: 43_200,
      scopes: SLACK_PROVIDER.userScopes,
      teamId: "T123",
      teamName: "Acme",
      userId: "U456",
    });
  });

  it("retains the workspace identity across rotating refreshes", () => {
    expect(decodeSlackRefreshResponse({
      ok: true,
      access_token: "xoxp-next",
      token_type: "user",
      scope: SLACK_PROVIDER.userScopes.join(","),
      refresh_token: "xoxe-next",
      expires_in: 43_200,
    }, { teamId: "T123", teamName: "Acme", userId: "U456" })).toEqual({
      accessToken: "xoxp-next",
      refreshToken: "xoxe-next",
      expiresIn: 43_200,
      scopes: SLACK_PROVIDER.userScopes,
      teamId: "T123",
      teamName: "Acme",
      userId: "U456",
    });
  });

  it("accepts the nested rotating user-token shape", () => {
    expect(decodeSlackRefreshResponse({
      ok: true,
      authed_user: {
        access_token: "xoxe.xoxp-next",
        token_type: "user",
        scope: SLACK_PROVIDER.userScopes.join(","),
        refresh_token: "xoxe-next",
        expires_in: 43_200,
      },
    }, { teamId: "T123", teamName: "Acme", userId: "U456" })).toMatchObject({
      accessToken: "xoxe.xoxp-next",
      refreshToken: "xoxe-next",
      teamId: "T123",
      userId: "U456",
    });
  });

  it("revokes the connected user token without putting it in the URL", () => {
    const request = buildSlackRevocationRequest("xoxp-secret");
    expect(request.url).toBe(SLACK_PROVIDER.revokeUrl);
    expect(request.headers.get("authorization")).toBe("Bearer xoxp-secret");
    expect(request.url).not.toContain("xoxp-secret");
  });

  it("rejects bot tokens and malformed workspace identities", () => {
    expect(() => decodeSlackTokenResponse({
      ok: true,
      team: { id: "T123", name: "Acme" },
      authed_user: {
        id: "U456",
        scope: SLACK_PROVIDER.userScopes.join(","),
        access_token: "xoxb-bot",
        token_type: "bot",
      },
    })).toThrow("invalid Slack token response");
    expect(() => decodeSlackTokenResponse({
      ok: true,
      team: { id: "not/a/team", name: "Acme" },
      authed_user: {
        id: "U456",
        scope: SLACK_PROVIDER.userScopes.join(","),
        access_token: "secret",
        token_type: "user",
      },
    })).toThrow("invalid Slack token response");
  });
});
