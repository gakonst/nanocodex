export const GMAIL_PROVIDER = {
  id: "gmail",
  authorizationUrl: "https://accounts.google.com/o/oauth2/v2/auth",
  tokenUrl: "https://oauth2.googleapis.com/token",
  identityUrl: "https://openidconnect.googleapis.com/v1/userinfo",
  scopes: [
    "openid",
    "email",
    "https://mail.google.com/",
  ],
} as const;

const PKCE_CHALLENGE = /^[A-Za-z0-9_-]{43}$/;
const PKCE_VERIFIER = /^[A-Za-z0-9._~-]{43,128}$/;

export type GmailAuthorizationInput = Readonly<{
  clientId: string;
  redirectUri: string;
  state: string;
  codeChallenge: string;
  loginHint?: string;
}>;

export type GmailTokenExchangeInput = Readonly<{
  clientId: string;
  clientSecret: string;
  code: string;
  codeVerifier: string;
  redirectUri: string;
}>;

export type GmailTokenResponse = Readonly<{
  accessToken: string;
  refreshToken?: string;
  expiresIn: number;
  refreshTokenExpiresIn?: number;
  tokenType: "Bearer";
  scopes: readonly string[];
}>;

export type GmailIdentity = Readonly<{
  accountId: string;
  displayLabel: string;
}>;

export function buildGmailAuthorizationParams(
  input: GmailAuthorizationInput,
): URLSearchParams {
  const clientId = nonEmptyInput(input.clientId, "clientId");
  const redirectUri = nonEmptyInput(input.redirectUri, "redirectUri");
  const state = nonEmptyInput(input.state, "state");
  if (!PKCE_CHALLENGE.test(input.codeChallenge)) {
    throw new Error("Gmail codeChallenge must be an S256 PKCE challenge");
  }

  return new URLSearchParams({
    client_id: clientId,
    redirect_uri: redirectUri,
    response_type: "code",
    scope: GMAIL_PROVIDER.scopes.join(" "),
    state,
    code_challenge: input.codeChallenge,
    code_challenge_method: "S256",
    access_type: "offline",
    prompt: "consent",
    include_granted_scopes: "true",
    ...(input.loginHint === undefined ? {} : {
      login_hint: nonEmptyInput(input.loginHint, "loginHint"),
    }),
  });
}

export function buildGmailAuthorizationUrl(input: GmailAuthorizationInput): URL {
  const url = new URL(GMAIL_PROVIDER.authorizationUrl);
  url.search = buildGmailAuthorizationParams(input).toString();
  return url;
}

export function buildGmailTokenRequest(input: GmailTokenExchangeInput): Request {
  const clientId = nonEmptyInput(input.clientId, "clientId");
  const clientSecret = nonEmptyInput(input.clientSecret, "clientSecret");
  const code = nonEmptyInput(input.code, "code");
  const redirectUri = nonEmptyInput(input.redirectUri, "redirectUri");
  if (!PKCE_VERIFIER.test(input.codeVerifier)) {
    throw new Error("Gmail codeVerifier must be a valid PKCE verifier");
  }

  return new Request(GMAIL_PROVIDER.tokenUrl, {
    method: "POST",
    headers: {
      accept: "application/json",
      "content-type": "application/x-www-form-urlencoded",
    },
    body: new URLSearchParams({
      client_id: clientId,
      client_secret: clientSecret,
      code,
      code_verifier: input.codeVerifier,
      grant_type: "authorization_code",
      redirect_uri: redirectUri,
    }),
  });
}

export function decodeGmailTokenResponse(value: unknown): GmailTokenResponse {
  const response = record(value, "Gmail token response");
  if ("error" in response) throw new Error("Gmail token endpoint returned an OAuth error");

  const accessToken = nonEmptyString(response.access_token, "Gmail access_token");
  const expiresIn = positiveInteger(response.expires_in, "Gmail expires_in");
  if (response.token_type !== "Bearer") {
    throw new Error("Gmail token_type must be Bearer");
  }
  const scope = nonEmptyString(response.scope, "Gmail scope");
  const scopes = scope.trim().split(/\s+/);
  const refreshToken = optionalNonEmptyString(response.refresh_token, "Gmail refresh_token");
  const refreshTokenExpiresIn = optionalPositiveInteger(
    response.refresh_token_expires_in,
    "Gmail refresh_token_expires_in",
  );
  optionalNonEmptyString(response.id_token, "Gmail id_token");

  return {
    accessToken,
    expiresIn,
    tokenType: "Bearer",
    scopes,
    ...(refreshToken === undefined ? {} : { refreshToken }),
    ...(refreshTokenExpiresIn === undefined ? {} : { refreshTokenExpiresIn }),
  };
}

export function buildGmailIdentityRequest(accessToken: string): Request {
  const token = nonEmptyInput(accessToken, "accessToken");
  return new Request(GMAIL_PROVIDER.identityUrl, {
    method: "GET",
    headers: {
      accept: "application/json",
      authorization: `Bearer ${token}`,
    },
  });
}

export function decodeGmailIdentity(value: unknown): GmailIdentity {
  const response = record(value, "Gmail identity response");
  const accountId = nonEmptyString(response.sub, "Gmail identity sub");
  if (accountId.length > 255) throw new Error("Gmail identity sub is too long");
  const email = nonEmptyString(response.email, "Gmail identity email");
  if (typeof response.email_verified !== "boolean") {
    throw new Error("Gmail identity email_verified must be a boolean");
  }

  return { accountId, displayLabel: email };
}

function nonEmptyInput(value: string, name: string): string {
  if (typeof value !== "string" || value.trim() === "") {
    throw new Error(`Gmail ${name} must be a non-empty string`);
  }
  return value;
}

function record(value: unknown, name: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${name} must be an object`);
  }
  return value as Record<string, unknown>;
}

function nonEmptyString(value: unknown, name: string): string {
  if (typeof value !== "string" || value.trim() === "") {
    throw new Error(`${name} must be a non-empty string`);
  }
  return value;
}

function optionalNonEmptyString(value: unknown, name: string): string | undefined {
  return value === undefined ? undefined : nonEmptyString(value, name);
}

function positiveInteger(value: unknown, name: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`${name} must be a positive integer`);
  }
  return value;
}

function optionalPositiveInteger(value: unknown, name: string): number | undefined {
  return value === undefined ? undefined : positiveInteger(value, name);
}
