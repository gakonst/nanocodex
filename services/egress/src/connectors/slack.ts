const SLACK_ID = /^[A-Z0-9]{1,32}$/;

export const SLACK_PROVIDER = Object.freeze({
  id: "slack" as const,
  authorizationUrl: "https://slack.com/oauth/v2/authorize",
  tokenUrl: "https://slack.com/api/oauth.v2.access",
  revokeUrl: "https://slack.com/api/auth.revoke",
  userScopes: Object.freeze([
    "channels:history",
    "channels:read",
    "chat:write",
    "groups:history",
    "groups:read",
    "im:history",
    "im:read",
    "im:write",
    "mpim:history",
    "mpim:read",
    "mpim:write",
    "reactions:read",
    "reactions:write",
    "search:read",
    "users:read",
  ] as const),
});

export type SlackAuthorizationInput = Readonly<{
  clientId: string;
  redirectUri: string;
  state: string;
}>;

export type SlackTokenExchangeInput = Readonly<{
  clientId: string;
  clientSecret: string;
  code: string;
  redirectUri: string;
}>;

export type SlackTokenRefreshInput = Readonly<{
  clientId: string;
  clientSecret: string;
  refreshToken: string;
}>;

export type SlackUserToken = Readonly<{
  accessToken: string;
  refreshToken?: string;
  expiresIn?: number;
  scopes: readonly string[];
  teamId: string;
  teamName: string;
  userId: string;
}>;

export function buildSlackAuthorizationUrl(input: SlackAuthorizationInput): URL {
  const url = new URL(SLACK_PROVIDER.authorizationUrl);
  url.search = new URLSearchParams({
    client_id: required(input.clientId, "client ID"),
    redirect_uri: required(input.redirectUri, "redirect URI"),
    state: required(input.state, "state"),
    user_scope: SLACK_PROVIDER.userScopes.join(","),
  }).toString();
  return url;
}

export function buildSlackTokenRequest(input: SlackTokenExchangeInput): Request {
  return slackTokenRequest(new URLSearchParams({
    client_id: required(input.clientId, "client ID"),
    client_secret: required(input.clientSecret, "client secret"),
    code: required(input.code, "authorization code"),
    redirect_uri: required(input.redirectUri, "redirect URI"),
  }));
}

export function buildSlackTokenRefreshRequest(input: SlackTokenRefreshInput): Request {
  return slackTokenRequest(new URLSearchParams({
    client_id: required(input.clientId, "client ID"),
    client_secret: required(input.clientSecret, "client secret"),
    grant_type: "refresh_token",
    refresh_token: required(input.refreshToken, "refresh token"),
  }));
}

function slackTokenRequest(body: URLSearchParams): Request {
  return new Request(SLACK_PROVIDER.tokenUrl, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/x-www-form-urlencoded",
    },
    body,
  });
}

export function buildSlackRevocationRequest(accessToken: string): Request {
  return new Request(SLACK_PROVIDER.revokeUrl, {
    method: "POST",
    headers: {
      accept: "application/json",
      authorization: `Bearer ${required(accessToken, "access token")}`,
      "content-type": "application/x-www-form-urlencoded",
    },
  });
}

export function decodeSlackTokenResponse(value: unknown): SlackUserToken {
  const response = record(value);
  if (response.ok !== true) throw new Error("invalid Slack token response");
  const authedUser = record(response.authed_user);
  const team = record(response.team);
  const accessToken = authedUser.access_token;
  const userId = authedUser.id;
  const teamId = team.id;
  const teamName = team.name;
  const tokenType = authedUser.token_type;
  const scope = authedUser.scope;
  if (typeof accessToken !== "string" || accessToken.length === 0
    || typeof userId !== "string" || !SLACK_ID.test(userId)
    || typeof teamId !== "string" || !SLACK_ID.test(teamId)
    || typeof teamName !== "string" || teamName.trim().length === 0 || teamName.length > 256
    || tokenType !== "user" || typeof scope !== "string") {
    throw new Error("invalid Slack token response");
  }
  const scopes = scope === "" ? [] : scope.split(",").map((item) => item.trim());
  if (scopes.some((item) => !/^[a-z][a-z0-9._:-]{0,127}$/.test(item))) {
    throw new Error("invalid Slack token response");
  }
  if (SLACK_PROVIDER.userScopes.some((requiredScope) => !scopes.includes(requiredScope))) {
    throw new Error("invalid Slack token response");
  }
  const refreshToken = optionalString(authedUser.refresh_token);
  const expiresIn = optionalPositiveInteger(authedUser.expires_in);
  if ((refreshToken === undefined) !== (expiresIn === undefined)) {
    throw new Error("invalid Slack token response");
  }
  return {
    accessToken,
    ...(refreshToken ? { refreshToken } : {}),
    ...(expiresIn ? { expiresIn } : {}),
    scopes,
    teamId,
    teamName: teamName.trim(),
    userId,
  };
}

export function decodeSlackRefreshResponse(
  value: unknown,
  identity: Pick<SlackUserToken, "teamId" | "teamName" | "userId">,
): SlackUserToken {
  const response = record(value);
  const token = isRecord(response.authed_user) && response.authed_user.token_type === "user"
    ? response.authed_user
    : response;
  if (response.ok !== true
    || typeof token.access_token !== "string" || token.access_token.length === 0
    || token.token_type !== "user"
    || typeof token.scope !== "string"
    || typeof token.refresh_token !== "string" || token.refresh_token.length === 0
    || !Number.isSafeInteger(token.expires_in) || (token.expires_in as number) <= 0) {
    throw new Error("invalid Slack token response");
  }
  const scopes = token.scope === "" ? [] : token.scope.split(",").map((item) => item.trim());
  if (scopes.some((item) => !/^[a-z][a-z0-9._:-]{0,127}$/.test(item))
    || SLACK_PROVIDER.userScopes.some((requiredScope) => !scopes.includes(requiredScope))) {
    throw new Error("invalid Slack token response");
  }
  return {
    accessToken: token.access_token,
    refreshToken: token.refresh_token,
    expiresIn: token.expires_in as number,
    scopes,
    ...identity,
  };
}

export function slackConnectionLabel(teamName: string, userId: string): string {
  return `${teamName} (${userId})`;
}

function required(value: string, label: string): string {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new Error(`Slack ${label} is required`);
  }
  return value;
}

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("invalid Slack token response");
  }
  return value as Record<string, unknown>;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function optionalString(value: unknown): string | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "string" || value.length === 0) throw new Error("invalid Slack token response");
  return value;
}

function optionalPositiveInteger(value: unknown): number | undefined {
  if (value === undefined) return undefined;
  if (!Number.isSafeInteger(value) || (value as number) <= 0) {
    throw new Error("invalid Slack token response");
  }
  return value as number;
}
