const GOOGLE_AUTHORIZATION_URL = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL = "https://oauth2.googleapis.com/token";
const GOOGLE_IDENTITY_URL = "https://openidconnect.googleapis.com/v1/userinfo";

const GDRIVE_SCOPES = [
  "openid",
  "email",
  "profile",
  "https://www.googleapis.com/auth/drive",
] as const;

export const GDRIVE_PROVIDER = Object.freeze({
  id: "gdrive" as const,
  authorizationUrl: GOOGLE_AUTHORIZATION_URL,
  tokenUrl: GOOGLE_TOKEN_URL,
  identityUrl: GOOGLE_IDENTITY_URL,
  scopes: Object.freeze([...GDRIVE_SCOPES]),
});

export interface GDriveAuthorizationInput {
  clientId: string;
  redirectUri: string;
  state: string;
  codeChallenge: string;
  loginHint?: string;
}

export interface GDriveTokenExchangeInput {
  clientId: string;
  clientSecret: string;
  redirectUri: string;
  authorizationCode: string;
  codeVerifier: string;
}

export interface GDriveTokenResponse {
  accessToken: string;
  refreshToken?: string;
  expiresInSeconds: number;
  grantedScopes: readonly string[];
}

export interface GDriveIdentity {
  accountId: string;
  displayLabel: string;
}

export function buildGDriveAuthorizationParams(
  input: GDriveAuthorizationInput,
): URLSearchParams {
  const clientId = requiredString(input.clientId, "clientId");
  const redirectUri = requiredString(input.redirectUri, "redirectUri");
  const state = requiredString(input.state, "state");
  const codeChallenge = requiredString(input.codeChallenge, "codeChallenge");
  if (!/^[A-Za-z0-9_-]{43}$/.test(codeChallenge)) {
    throw new TypeError("codeChallenge must be a base64url-encoded SHA-256 digest");
  }

  return new URLSearchParams({
    client_id: clientId,
    redirect_uri: redirectUri,
    response_type: "code",
    scope: GDRIVE_SCOPES.join(" "),
    state,
    code_challenge: codeChallenge,
    code_challenge_method: "S256",
    access_type: "offline",
    prompt: "consent",
    include_granted_scopes: "true",
    ...(input.loginHint === undefined ? {} : {
      login_hint: requiredString(input.loginHint, "loginHint"),
    }),
  });
}

export function buildGDriveAuthorizationUrl(input: GDriveAuthorizationInput): URL {
  const url = new URL(GDRIVE_PROVIDER.authorizationUrl);
  url.search = buildGDriveAuthorizationParams(input).toString();
  return url;
}

export function buildGDriveTokenRequest(input: GDriveTokenExchangeInput): Request {
  const body = new URLSearchParams({
    client_id: requiredString(input.clientId, "clientId"),
    client_secret: requiredString(input.clientSecret, "clientSecret"),
    redirect_uri: requiredString(input.redirectUri, "redirectUri"),
    code: requiredString(input.authorizationCode, "authorizationCode"),
    code_verifier: requiredString(input.codeVerifier, "codeVerifier"),
    grant_type: "authorization_code",
  });

  return new Request(GDRIVE_PROVIDER.tokenUrl, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/x-www-form-urlencoded;charset=UTF-8",
    },
    body,
  });
}

export function decodeGDriveTokenResponse(value: unknown): GDriveTokenResponse {
  const response = record(value, "Google token response");
  const accessToken = responseString(response, "access_token");
  const refreshToken = optionalResponseString(response, "refresh_token");
  const expiresInSeconds = positiveInteger(response.expires_in, "expires_in");
  if (response.token_type !== "Bearer") {
    throw new TypeError("Google token response token_type must be Bearer");
  }
  const scope = optionalResponseString(response, "scope");
  const grantedScopes = scope?.split(" ") ?? [...GDRIVE_SCOPES];
  if (grantedScopes.some((candidate) => candidate.length === 0)) {
    throw new TypeError("Google token response scope must be space-delimited");
  }
  for (const requiredScope of GDRIVE_SCOPES) {
    if (!hasGrantedScope(grantedScopes, requiredScope)) {
      throw new TypeError(`Google token response is missing required scope: ${requiredScope}`);
    }
  }

  return Object.freeze({
    accessToken,
    ...(refreshToken ? { refreshToken } : {}),
    expiresInSeconds,
    grantedScopes: Object.freeze(grantedScopes),
  });
}

export function buildGDriveIdentityRequest(accessToken: string): Request {
  return new Request(GDRIVE_PROVIDER.identityUrl, {
    method: "GET",
    headers: {
      accept: "application/json",
      authorization: `Bearer ${requiredString(accessToken, "accessToken")}`,
    },
  });
}

export function decodeGDriveIdentity(value: unknown): GDriveIdentity {
  const response = record(value, "Google identity response");
  const accountId = responseString(response, "sub");
  if (accountId.length > 255) {
    throw new TypeError("Google identity response sub exceeds 255 characters");
  }
  const email = optionalResponseString(response, "email");
  const name = optionalResponseString(response, "name");
  if (response.email_verified !== undefined && typeof response.email_verified !== "boolean") {
    throw new TypeError("Google identity response email_verified must be a boolean");
  }
  const displayLabel = email ?? name;
  if (!displayLabel) {
    throw new TypeError("Google identity response must contain email or name");
  }

  return Object.freeze({ accountId, displayLabel });
}

function requiredString(value: string, field: string): string {
  if (typeof value !== "string" || value.length === 0 || value.trim() !== value) {
    throw new TypeError(`${field} must be a non-empty string without surrounding whitespace`);
  }
  return value;
}

function record(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError(`${label} must be an object`);
  }
  return value as Record<string, unknown>;
}

function responseString(response: Record<string, unknown>, field: string): string {
  const value = response[field];
  if (typeof value !== "string" || value.length === 0 || value.trim() !== value) {
    throw new TypeError(`Google response ${field} must be a non-empty string`);
  }
  return value;
}

function optionalResponseString(
  response: Record<string, unknown>,
  field: string,
): string | undefined {
  if (response[field] === undefined) return undefined;
  return responseString(response, field);
}

function hasGrantedScope(granted: readonly string[], required: string): boolean {
  if (granted.includes(required)) return true;
  if (required === "email") {
    return granted.includes("https://www.googleapis.com/auth/userinfo.email");
  }
  if (required === "profile") {
    return granted.includes("https://www.googleapis.com/auth/userinfo.profile");
  }
  return false;
}

function positiveInteger(value: unknown, field: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0) {
    throw new TypeError(`Google response ${field} must be a positive integer`);
  }
  return value;
}
