export const WHOOP_PROVIDER = Object.freeze({
  id: "whoop" as const,
  authorizationUrl: "https://api.prod.whoop.com/oauth/oauth2/auth",
  tokenUrl: "https://api.prod.whoop.com/oauth/oauth2/token",
  identityUrl: "https://api.prod.whoop.com/developer/v2/user/profile/basic",
  revocationUrl: "https://api.prod.whoop.com/developer/v2/user/access",
  scopes: Object.freeze([
    "offline",
    "read:profile",
    "read:body_measurement",
    "read:cycles",
    "read:recovery",
    "read:sleep",
    "read:workout",
  ] as const),
});

export type WhoopAuthorizationInput = Readonly<{
  clientId: string;
  redirectUri: string;
  state: string;
}>;

export type WhoopTokenExchangeInput = Readonly<{
  clientId: string;
  clientSecret: string;
  code: string;
  redirectUri: string;
}>;

export type WhoopTokenResponse = Readonly<{
  accessToken: string;
  refreshToken?: string;
  expiresIn: number;
  tokenType: "bearer";
  scopes: readonly string[];
}>;

export type WhoopIdentity = Readonly<{
  accountId: string;
  displayLabel: string;
}>;

const SCOPE = /^[A-Za-z0-9._:-]+$/;

export function buildWhoopAuthorizationParams(
  input: WhoopAuthorizationInput,
): URLSearchParams {
  return new URLSearchParams({
    response_type: "code",
    client_id: required(input.clientId, "client ID"),
    redirect_uri: required(input.redirectUri, "redirect URI"),
    scope: WHOOP_PROVIDER.scopes.join(" "),
    state: required(input.state, "state"),
  });
}

export function buildWhoopAuthorizationUrl(input: WhoopAuthorizationInput): URL {
  const url = new URL(WHOOP_PROVIDER.authorizationUrl);
  url.search = buildWhoopAuthorizationParams(input).toString();
  return url;
}

export function buildWhoopTokenRequest(input: WhoopTokenExchangeInput): Request {
  return tokenRequest(new URLSearchParams({
    grant_type: "authorization_code",
    code: required(input.code, "authorization code"),
    client_id: required(input.clientId, "client ID"),
    client_secret: required(input.clientSecret, "client secret"),
    redirect_uri: required(input.redirectUri, "redirect URI"),
  }));
}

export function buildWhoopRefreshRequest(
  clientId: string,
  clientSecret: string,
  refreshToken: string,
): Request {
  return tokenRequest(new URLSearchParams({
    grant_type: "refresh_token",
    refresh_token: required(refreshToken, "refresh token"),
    client_id: required(clientId, "client ID"),
    client_secret: required(clientSecret, "client secret"),
    scope: "offline",
  }));
}

export function decodeWhoopTokenResponse(value: unknown): WhoopTokenResponse {
  const response = record(value, "token response");
  if (typeof response.access_token !== "string" || !response.access_token
    || typeof response.token_type !== "string" || response.token_type.toLowerCase() !== "bearer"
    || !Number.isSafeInteger(response.expires_in) || (response.expires_in as number) <= 0
    || typeof response.scope !== "string" || !response.scope.trim()) {
    throw new Error("invalid WHOOP token response");
  }
  const scopes = response.scope.trim().split(/\s+/);
  if (scopes.some((scope) => !SCOPE.test(scope))
    || WHOOP_PROVIDER.scopes.some((scope) => !scopes.includes(scope))) {
    throw new Error("invalid WHOOP token response");
  }
  if (response.refresh_token !== undefined
    && (typeof response.refresh_token !== "string" || !response.refresh_token)) {
    throw new Error("invalid WHOOP token response");
  }
  return {
    accessToken: response.access_token,
    ...(typeof response.refresh_token === "string" ? { refreshToken: response.refresh_token } : {}),
    expiresIn: response.expires_in as number,
    tokenType: "bearer",
    scopes,
  };
}

export function buildWhoopIdentityRequest(accessToken: string): Request {
  return new Request(WHOOP_PROVIDER.identityUrl, {
    method: "GET",
    headers: {
      accept: "application/json",
      authorization: `Bearer ${required(accessToken, "access token")}`,
    },
  });
}

export function buildWhoopRevocationRequest(accessToken: string): Request {
  return new Request(WHOOP_PROVIDER.revocationUrl, {
    method: "DELETE",
    headers: {
      accept: "application/json",
      authorization: `Bearer ${required(accessToken, "access token")}`,
    },
  });
}

export function decodeWhoopIdentity(value: unknown): WhoopIdentity {
  const response = record(value, "identity response");
  if (!Number.isSafeInteger(response.user_id) || (response.user_id as number) <= 0
    || typeof response.email !== "string" || !response.email.trim()
    || typeof response.first_name !== "string"
    || typeof response.last_name !== "string") {
    throw new Error("invalid WHOOP identity response");
  }
  const email = response.email.trim();
  const name = `${response.first_name.trim()} ${response.last_name.trim()}`.trim();
  return {
    accountId: String(response.user_id),
    displayLabel: name ? `${name} (${email})` : email,
  };
}

function tokenRequest(body: URLSearchParams): Request {
  return new Request(WHOOP_PROVIDER.tokenUrl, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/x-www-form-urlencoded",
    },
    body,
  });
}

function required(value: string, label: string): string {
  if (typeof value !== "string" || !value.trim()) throw new Error(`WHOOP ${label} is required`);
  return value;
}

function record(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`invalid WHOOP ${label}`);
  }
  return value as Record<string, unknown>;
}
