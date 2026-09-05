const GITHUB_API_VERSION = "2026-03-10";

export const GITHUB_PROVIDER = Object.freeze({
  id: "github" as const,
  authorizationUrl: "https://github.com/login/oauth/authorize",
  tokenUrl: "https://github.com/login/oauth/access_token",
  identityUrl: "https://api.github.com/user",
  // Repository work includes private repositories and workflow-file updates,
  // without granting organization, account-admin, package, or deletion scopes.
  scopes: Object.freeze([
    "repo",
    "workflow",
  ] as const),
});

export interface GitHubAuthorizationInput {
  clientId: string;
  redirectUri: string;
  state: string;
  codeChallenge: string;
}

export interface GitHubTokenExchangeInput {
  clientId: string;
  clientSecret: string;
  code: string;
  redirectUri: string;
  codeVerifier: string;
}

export interface GitHubTokenRefreshInput {
  clientId: string;
  clientSecret: string;
  refreshToken: string;
}

export interface GitHubTokenResponse {
  accessToken: string;
  tokenType: "bearer";
  scopes: readonly string[];
  expiresIn?: number;
  refreshToken?: string;
  refreshTokenExpiresIn?: number;
}

export interface GitHubIdentity {
  accountId: string;
  displayLabel: string;
}

export function buildGitHubAuthorizationParams(
  input: GitHubAuthorizationInput,
): URLSearchParams {
  const codeChallenge = required(input.codeChallenge, "code challenge");
  if (!/^[A-Za-z0-9_-]{43}$/.test(codeChallenge)) {
    throw new Error("GitHub PKCE code challenge must be a 43-character base64url value");
  }

  return new URLSearchParams({
    client_id: required(input.clientId, "client ID"),
    redirect_uri: required(input.redirectUri, "redirect URI"),
    scope: GITHUB_PROVIDER.scopes.join(" "),
    state: required(input.state, "state"),
    code_challenge: codeChallenge,
    code_challenge_method: "S256",
  });
}

export function buildGitHubAuthorizationUrl(input: GitHubAuthorizationInput): URL {
  const url = new URL(GITHUB_PROVIDER.authorizationUrl);
  url.search = buildGitHubAuthorizationParams(input).toString();
  return url;
}

export function buildGitHubTokenRequest(input: GitHubTokenExchangeInput): Request {
  const codeVerifier = required(input.codeVerifier, "code verifier");
  if (!/^[A-Za-z0-9._~-]{43,128}$/.test(codeVerifier)) {
    throw new Error("GitHub code verifier must be a valid PKCE verifier");
  }
  const body = new URLSearchParams({
    client_id: required(input.clientId, "client ID"),
    client_secret: required(input.clientSecret, "client secret"),
    code: required(input.code, "authorization code"),
    redirect_uri: required(input.redirectUri, "redirect URI"),
    code_verifier: codeVerifier,
  });

  return new Request(GITHUB_PROVIDER.tokenUrl, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/x-www-form-urlencoded",
    },
    body,
  });
}

export function buildGitHubTokenRefreshRequest(input: GitHubTokenRefreshInput): Request {
  const body = new URLSearchParams({
    client_id: required(input.clientId, "client ID"),
    client_secret: required(input.clientSecret, "client secret"),
    grant_type: "refresh_token",
    refresh_token: required(input.refreshToken, "refresh token"),
  });

  return new Request(GITHUB_PROVIDER.tokenUrl, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/x-www-form-urlencoded",
    },
    body,
  });
}

export function decodeGitHubTokenResponse(value: unknown): GitHubTokenResponse {
  const response = record(value, "GitHub token response");
  const allowed = new Set([
    "access_token",
    "token_type",
    "scope",
    "expires_in",
    "refresh_token",
    "refresh_token_expires_in",
  ]);
  if (Object.keys(response).some((key) => !allowed.has(key))) {
    throw new Error("invalid GitHub token response");
  }

  const accessToken = response.access_token;
  const tokenType = response.token_type;
  const scope = response.scope;
  if (typeof accessToken !== "string" || accessToken.length === 0
    || typeof tokenType !== "string" || tokenType.toLowerCase() !== "bearer"
    || typeof scope !== "string") {
    throw new Error("invalid GitHub token response");
  }

  const scopes = scope === "" ? [] : scope.split(",").map((item) => item.trim());
  if (scopes.some((item) => !/^[A-Za-z0-9:_-]+$/.test(item))) {
    throw new Error("invalid GitHub token response");
  }

  const expiresIn = optionalPositiveInteger(response.expires_in);
  const refreshTokenExpiresIn = optionalPositiveInteger(response.refresh_token_expires_in);
  const refreshToken = response.refresh_token;
  if (refreshToken !== undefined && (typeof refreshToken !== "string" || refreshToken.length === 0)) {
    throw new Error("invalid GitHub token response");
  }

  return {
    accessToken,
    tokenType: "bearer",
    scopes,
    ...(expiresIn === undefined ? {} : { expiresIn }),
    ...(refreshToken === undefined ? {} : { refreshToken }),
    ...(refreshTokenExpiresIn === undefined ? {} : { refreshTokenExpiresIn }),
  };
}

export function buildGitHubIdentityRequest(
  accessToken: string,
  userAgent = "nanocodex-egress",
): Request {
  return new Request(GITHUB_PROVIDER.identityUrl, {
    method: "GET",
    headers: {
      accept: "application/vnd.github+json",
      authorization: `Bearer ${required(accessToken, "access token")}`,
      "user-agent": required(userAgent, "user agent"),
      "x-github-api-version": GITHUB_API_VERSION,
    },
  });
}

export function decodeGitHubIdentity(value: unknown): GitHubIdentity {
  const response = record(value, "GitHub identity response");
  if (!Number.isSafeInteger(response.id) || (response.id as number) <= 0
    || typeof response.login !== "string" || response.login.trim().length === 0
    || !(response.name === undefined || response.name === null || typeof response.name === "string")) {
    throw new Error("invalid GitHub identity response");
  }

  const login = response.login.trim();
  const name = typeof response.name === "string" ? response.name.trim() : "";
  return {
    accountId: String(response.id),
    displayLabel: name === "" ? login : `${name} (${login})`,
  };
}

function required(value: string, label: string): string {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new Error(`GitHub ${label} is required`);
  }
  return value;
}

function record(value: unknown, label: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`invalid ${label}`);
  }
  return value as Record<string, unknown>;
}

function optionalPositiveInteger(value: unknown): number | undefined {
  if (value === undefined) return undefined;
  if (!Number.isSafeInteger(value) || (value as number) <= 0) {
    throw new Error("invalid GitHub token response");
  }
  return value as number;
}
