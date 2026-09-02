const SLACK_ID = /^[A-Z0-9]{1,32}$/;

export const SLACK_PROVIDER = Object.freeze({
  id: "slack" as const,
  authorizationUrl: "https://slack.com/oauth/v2/authorize",
  tokenUrl: "https://slack.com/api/oauth.v2.access",
  revokeUrl: "https://slack.com/api/auth.revoke",
  userScopes: Object.freeze([
    "channels:history", "channels:read", "chat:write", "groups:history", "groups:read",
    "im:history", "im:read", "im:write", "mpim:history", "mpim:read", "mpim:write",
    "reactions:read", "reactions:write", "search:read", "users:read",
  ] as const),
});

export type SlackUserToken = Readonly<{
  accessToken: string;
  refreshToken?: string;
  expiresIn?: number;
  scopes: readonly string[];
  teamId: string;
  teamName: string;
  userId: string;
}>;

export function buildSlackAuthorizationUrl(input: {
  clientId: string; redirectUri: string; state: string;
}): URL {
  const url = new URL(SLACK_PROVIDER.authorizationUrl);
  url.search = new URLSearchParams({
    client_id: required(input.clientId),
    redirect_uri: required(input.redirectUri),
    state: required(input.state),
    user_scope: SLACK_PROVIDER.userScopes.join(","),
  }).toString();
  return url;
}

export function buildSlackTokenRequest(input: {
  clientId: string; clientSecret: string; code: string; redirectUri: string;
}): Request {
  return slackTokenRequest(new URLSearchParams({
    client_id: required(input.clientId), client_secret: required(input.clientSecret),
    code: required(input.code), redirect_uri: required(input.redirectUri),
  }));
}

export function buildSlackTokenRefreshRequest(input: {
  clientId: string; clientSecret: string; refreshToken: string;
}): Request {
  return slackTokenRequest(new URLSearchParams({
    client_id: required(input.clientId), client_secret: required(input.clientSecret),
    grant_type: "refresh_token", refresh_token: required(input.refreshToken),
  }));
}

function slackTokenRequest(body: URLSearchParams): Request {
  return new Request(SLACK_PROVIDER.tokenUrl, {
    method: "POST",
    headers: { accept: "application/json", "content-type": "application/x-www-form-urlencoded" },
    body,
  });
}

export function buildSlackRevocationRequest(accessToken: string): Request {
  return new Request(SLACK_PROVIDER.revokeUrl, {
    method: "POST",
    headers: {
      accept: "application/json",
      authorization: `Bearer ${required(accessToken)}`,
      "content-type": "application/x-www-form-urlencoded",
    },
  });
}

export function decodeSlackTokenResponse(value: unknown): SlackUserToken {
  const response = record(value);
  if (response.ok !== true) throw new TypeError("invalid Slack token response");
  const user = record(response.authed_user);
  const team = record(response.team);
  const accessToken = user.access_token;
  const userId = user.id;
  const teamId = team.id;
  const teamName = team.name;
  if (typeof accessToken !== "string" || accessToken === ""
    || typeof userId !== "string" || !SLACK_ID.test(userId)
    || typeof teamId !== "string" || !SLACK_ID.test(teamId)
    || typeof teamName !== "string" || teamName.trim() === "" || teamName.length > 192
    || user.token_type !== "user" || typeof user.scope !== "string") {
    throw new TypeError("invalid Slack token response");
  }
  const scopes = decodeScopes(user.scope);
  const refreshToken = optionalString(user.refresh_token);
  const expiresIn = optionalPositiveInteger(user.expires_in);
  if ((refreshToken === undefined) !== (expiresIn === undefined)) {
    throw new TypeError("invalid Slack token response");
  }
  return { accessToken, ...(refreshToken ? { refreshToken } : {}),
    ...(expiresIn ? { expiresIn } : {}), scopes, teamId, teamName: teamName.trim(), userId };
}

export function decodeSlackRefreshResponse(
  value: unknown,
  identity: Pick<SlackUserToken, "teamId" | "teamName" | "userId">,
): SlackUserToken {
  const response = record(value);
  const token = isRecord(response.authed_user) && response.authed_user.token_type === "user"
    ? response.authed_user : response;
  if (response.ok !== true || typeof token.access_token !== "string" || token.access_token === ""
    || token.token_type !== "user" || typeof token.scope !== "string"
    || typeof token.refresh_token !== "string" || token.refresh_token === ""
    || !Number.isSafeInteger(token.expires_in) || (token.expires_in as number) <= 0) {
    throw new TypeError("invalid Slack token response");
  }
  return { accessToken: token.access_token, refreshToken: token.refresh_token,
    expiresIn: token.expires_in as number, scopes: decodeScopes(token.scope), ...identity };
}

export function slackConnectionLabel(teamName: string, userId: string): string {
  return `${teamName} (${userId})`;
}

function decodeScopes(value: string): string[] {
  const scopes = value === "" ? [] : value.split(",").map((item) => item.trim());
  if (scopes.some((item) => !/^[a-z][a-z0-9._:-]{0,127}$/.test(item))
    || SLACK_PROVIDER.userScopes.some((scope) => !scopes.includes(scope))) {
    throw new TypeError("invalid Slack token response");
  }
  return scopes;
}

function required(value: string): string {
  if (typeof value !== "string" || value.trim() === "" || value.length > 2_048) {
    throw new TypeError("Slack value must be a bounded non-empty string");
  }
  return value;
}
function record(value: unknown): Record<string, unknown> {
  if (!isRecord(value)) throw new TypeError("invalid Slack token response");
  return value;
}
function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}
function optionalString(value: unknown): string | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "string" || value === "") throw new TypeError("invalid Slack token response");
  return value;
}
function optionalPositiveInteger(value: unknown): number | undefined {
  if (value === undefined) return undefined;
  if (!Number.isSafeInteger(value) || (value as number) <= 0) {
    throw new TypeError("invalid Slack token response");
  }
  return value as number;
}
