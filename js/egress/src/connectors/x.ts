export const X_PROVIDER = Object.freeze({
  id: "x" as const,
  authorizationUrl: "https://x.com/i/oauth2/authorize",
  tokenUrl: "https://api.x.com/2/oauth2/token",
  revocationUrl: "https://api.x.com/2/oauth2/revoke",
  identityUrl: "https://api.x.com/2/users/me",
  scopes: Object.freeze([
    "tweet.read",
    "tweet.write",
    "users.read",
    "follows.read",
    "follows.write",
    "like.read",
    "like.write",
    "bookmark.read",
    "bookmark.write",
    "list.read",
    "list.write",
    "dm.read",
    "dm.write",
    "media.write",
    "offline.access",
  ] as const),
});

export type XAuthorizationInput = Readonly<{
  clientId: string;
  redirectUri: string;
  state: string;
  codeChallenge: string;
}>;

export type XTokenExchangeInput = Readonly<{
  clientId: string;
  clientSecret: string;
  code: string;
  redirectUri: string;
  codeVerifier: string;
}>;

export type XTokenResponse = Readonly<{
  accessToken: string;
  refreshToken?: string;
  expiresIn: number;
  tokenType: "bearer";
  scopes: readonly string[];
}>;

export type XIdentity = Readonly<{
  accountId: string;
  displayLabel: string;
}>;

const PKCE_CHALLENGE = /^[A-Za-z0-9_-]{43}$/;
const PKCE_VERIFIER = /^[A-Za-z0-9._~-]{43,128}$/;
const SCOPE = /^[A-Za-z0-9._:-]+$/;

export function buildXAuthorizationParams(input: XAuthorizationInput): URLSearchParams {
  if (!PKCE_CHALLENGE.test(input.codeChallenge)) {
    throw new Error("X codeChallenge must be an S256 PKCE challenge");
  }
  return new URLSearchParams({
    response_type: "code",
    client_id: required(input.clientId, "client ID"),
    redirect_uri: required(input.redirectUri, "redirect URI"),
    scope: X_PROVIDER.scopes.join(" "),
    state: required(input.state, "state"),
    code_challenge: input.codeChallenge,
    code_challenge_method: "S256",
  });
}

export function buildXAuthorizationUrl(input: XAuthorizationInput): URL {
  const url = new URL(X_PROVIDER.authorizationUrl);
  url.search = buildXAuthorizationParams(input).toString();
  return url;
}

export function buildXTokenRequest(input: XTokenExchangeInput): Request {
  if (!PKCE_VERIFIER.test(input.codeVerifier)) {
    throw new Error("X codeVerifier must be a valid PKCE verifier");
  }
  const clientId = required(input.clientId, "client ID");
  const clientSecret = required(input.clientSecret, "client secret");
  return new Request(X_PROVIDER.tokenUrl, {
    method: "POST",
    headers: {
      accept: "application/json",
      authorization: `Basic ${btoa(`${clientId}:${clientSecret}`)}`,
      "content-type": "application/x-www-form-urlencoded",
    },
    body: new URLSearchParams({
      code: required(input.code, "authorization code"),
      grant_type: "authorization_code",
      redirect_uri: required(input.redirectUri, "redirect URI"),
      code_verifier: input.codeVerifier,
    }),
  });
}

export function buildXRefreshRequest(
  clientId: string,
  clientSecret: string,
  refreshToken: string,
): Request {
  const id = required(clientId, "client ID");
  const secret = required(clientSecret, "client secret");
  return new Request(X_PROVIDER.tokenUrl, {
    method: "POST",
    headers: {
      accept: "application/json",
      authorization: `Basic ${btoa(`${id}:${secret}`)}`,
      "content-type": "application/x-www-form-urlencoded",
    },
    body: new URLSearchParams({
      refresh_token: required(refreshToken, "refresh token"),
      grant_type: "refresh_token",
    }),
  });
}

export function buildXRevocationRequest(
  clientId: string,
  token: string,
): Request {
  const id = required(clientId, "client ID");
  return new Request(X_PROVIDER.revocationUrl, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/x-www-form-urlencoded",
    },
    body: new URLSearchParams({
      token: required(token, "token"),
      client_id: id,
    }),
  });
}

export function decodeXTokenResponse(value: unknown): XTokenResponse {
  const response = record(value, "token response");
  if (typeof response.access_token !== "string" || !response.access_token
    || typeof response.token_type !== "string" || response.token_type.toLowerCase() !== "bearer"
    || !Number.isSafeInteger(response.expires_in) || (response.expires_in as number) <= 0
    || typeof response.scope !== "string" || !response.scope.trim()) {
    throw new Error("invalid X token response");
  }
  const scopes = response.scope.trim().split(/\s+/);
  if (scopes.some((scope) => !SCOPE.test(scope))
    || X_PROVIDER.scopes.some((scope) => !scopes.includes(scope))) {
    throw new Error("invalid X token response");
  }
  if (response.refresh_token !== undefined
    && (typeof response.refresh_token !== "string" || !response.refresh_token)) {
    throw new Error("invalid X token response");
  }
  return {
    accessToken: response.access_token,
    ...(typeof response.refresh_token === "string" ? { refreshToken: response.refresh_token } : {}),
    expiresIn: response.expires_in as number,
    tokenType: "bearer",
    scopes,
  };
}

export function buildXIdentityRequest(accessToken: string): Request {
  return new Request(X_PROVIDER.identityUrl, {
    headers: {
      accept: "application/json",
      authorization: `Bearer ${required(accessToken, "access token")}`,
    },
  });
}

export function decodeXIdentity(value: unknown): XIdentity {
  const response = record(value, "identity response");
  const data = record(response.data, "identity response data");
  if (typeof data.id !== "string" || !/^[0-9]+$/.test(data.id)
    || typeof data.username !== "string" || !/^[A-Za-z0-9_]{1,15}$/.test(data.username)
    || typeof data.name !== "string" || !data.name.trim()) {
    throw new Error("invalid X identity response");
  }
  return {
    accountId: data.id,
    displayLabel: `${data.name.trim()} (@${data.username})`,
  };
}

function required(value: string, label: string): string {
  if (typeof value !== "string" || !value.trim()) throw new Error(`X ${label} is required`);
  return value;
}

function record(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`invalid X ${label}`);
  }
  return value as Record<string, unknown>;
}
