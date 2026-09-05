export const GOOGLE_CAPABILITIES = Object.freeze({
  gmail: "https://mail.google.com/",
  gdrive: "https://www.googleapis.com/auth/drive",
  gcalendar: "https://www.googleapis.com/auth/calendar",
  gtasks: "https://www.googleapis.com/auth/tasks",
  gdocs: "https://www.googleapis.com/auth/documents",
  gsheets: "https://www.googleapis.com/auth/spreadsheets",
  gslides: "https://www.googleapis.com/auth/presentations",
  gcontacts: "https://www.googleapis.com/auth/contacts.readonly",
} as const);

export type GoogleCapabilityId = keyof typeof GOOGLE_CAPABILITIES;

export const GOOGLE_PROVIDER = Object.freeze({
  id: "google" as const,
  authorizationUrl: "https://accounts.google.com/o/oauth2/v2/auth",
  tokenUrl: "https://oauth2.googleapis.com/token",
  identityUrl: "https://openidconnect.googleapis.com/v1/userinfo",
  scopes: Object.freeze([
    "openid",
    "email",
    "profile",
    ...Object.values(GOOGLE_CAPABILITIES),
  ]),
});

type AuthorizationInput = Readonly<{
  clientId: string;
  redirectUri: string;
  state: string;
  codeChallenge: string;
  loginHint?: string;
}>;

type TokenInput = Readonly<{
  clientId: string;
  clientSecret: string;
  code: string;
  codeVerifier: string;
  redirectUri: string;
}>;

export type GoogleToken = Readonly<{
  accessToken: string;
  refreshToken?: string;
  expiresIn: number;
  scopes: readonly string[];
}>;

export function buildGoogleAuthorizationUrl(input: AuthorizationInput): URL {
  if (!/^[A-Za-z0-9_-]{43}$/.test(input.codeChallenge)) {
    throw new TypeError("Google codeChallenge must be an S256 PKCE challenge");
  }
  const url = new URL(GOOGLE_PROVIDER.authorizationUrl);
  url.search = new URLSearchParams({
    client_id: required(input.clientId, "clientId"),
    redirect_uri: required(input.redirectUri, "redirectUri"),
    response_type: "code",
    scope: GOOGLE_PROVIDER.scopes.join(" "),
    state: required(input.state, "state"),
    code_challenge: input.codeChallenge,
    code_challenge_method: "S256",
    access_type: "offline",
    prompt: "consent",
    include_granted_scopes: "true",
    enable_granular_consent: "true",
    ...(input.loginHint === undefined ? {} : {
      login_hint: required(input.loginHint, "loginHint"),
    }),
  }).toString();
  return url;
}

export function buildGoogleTokenRequest(input: TokenInput): Request {
  if (!/^[A-Za-z0-9._~-]{43,128}$/.test(input.codeVerifier)) {
    throw new TypeError("Google codeVerifier must be a valid PKCE verifier");
  }
  return new Request(GOOGLE_PROVIDER.tokenUrl, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/x-www-form-urlencoded",
    },
    body: new URLSearchParams({
      client_id: required(input.clientId, "clientId"),
      client_secret: required(input.clientSecret, "clientSecret"),
      code: required(input.code, "code"),
      code_verifier: input.codeVerifier,
      grant_type: "authorization_code",
      redirect_uri: required(input.redirectUri, "redirectUri"),
    }),
  });
}

export function decodeGoogleTokenResponse(value: unknown): GoogleToken {
  const response = record(value, "Google token response");
  if ("error" in response || response.token_type !== "Bearer") {
    throw new TypeError("invalid Google token response");
  }
  const accessToken = stringValue(response.access_token, "access_token");
  const refreshToken = optionalString(response.refresh_token, "refresh_token");
  const expiresIn = positiveInteger(response.expires_in, "expires_in");
  const scope = stringValue(response.scope, "scope");
  const scopes = scope.split(/\s+/);
  if (scopes.some((item) => item.length === 0 || item.length > 512)) {
    throw new TypeError("invalid Google token response scope");
  }
  return { accessToken, ...(refreshToken ? { refreshToken } : {}), expiresIn, scopes };
}

export function buildGoogleIdentityRequest(accessToken: string): Request {
  return new Request(GOOGLE_PROVIDER.identityUrl, {
    headers: {
      accept: "application/json",
      authorization: `Bearer ${required(accessToken, "accessToken")}`,
    },
  });
}

export function decodeGoogleIdentity(value: unknown): { accountId: string; displayLabel: string } {
  const response = record(value, "Google identity response");
  const accountId = stringValue(response.sub, "sub");
  const email = stringValue(response.email, "email");
  if (accountId.length > 255 || email.length > 256 || response.email_verified !== true) {
    throw new TypeError("invalid Google identity response");
  }
  return { accountId, displayLabel: email };
}

export function googleCapabilities(scopes: readonly string[]): GoogleCapabilityId[] {
  const granted = new Set(scopes);
  return (Object.entries(GOOGLE_CAPABILITIES) as [GoogleCapabilityId, string][])
    .filter(([, scope]) => granted.has(scope))
    .map(([id]) => id);
}

function required(value: string, field: string): string {
  if (typeof value !== "string" || value.trim() === "" || value.length > 2_048) {
    throw new TypeError(`Google ${field} must be a bounded non-empty string`);
  }
  return value;
}

function record(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError(`${label} must be an object`);
  }
  return value as Record<string, unknown>;
}

function stringValue(value: unknown, field: string): string {
  if (typeof value !== "string" || value.trim() === "" || value.trim() !== value) {
    throw new TypeError(`Google ${field} must be a non-empty string`);
  }
  return value;
}

function optionalString(value: unknown, field: string): string | undefined {
  return value === undefined ? undefined : stringValue(value, field);
}

function positiveInteger(value: unknown, field: string): number {
  if (!Number.isSafeInteger(value) || (value as number) <= 0) {
    throw new TypeError(`Google ${field} must be a positive integer`);
  }
  return value as number;
}
