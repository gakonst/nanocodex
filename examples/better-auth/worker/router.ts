const CONFIG_PATH = "/api/config";
const SESSION_PATH = "/api/session";
const PRINCIPAL_PATH = "/api/nanocodex/host-principal";
const AUTH_BASE_PATH = "/api/auth";
const AUTH_ROUTES: ReadonlyMap<string, string> = new Map([
  [`${AUTH_BASE_PATH}/sign-in/social`, "POST"],
  [`${AUTH_BASE_PATH}/callback/github`, "GET"],
  [`${AUTH_BASE_PATH}/sign-out`, "POST"],
]);
const BETTER_AUTH_SECRET_V1 = /^v1\.([A-Za-z0-9_-]{43})$/;
const APP_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;

export type WorkerEnv = Readonly<{
  ASSETS: Fetcher;
  AUTH_DB: D1Database;
  BETTER_AUTH_SECRET?: string;
  BETTER_AUTH_GITHUB_CLIENT_ID?: string;
  BETTER_AUTH_GITHUB_CLIENT_SECRET?: string;
  NANOCODEX_HOST_APP_ID?: string;
  NANOCODEX_HOST_APP_ORIGIN?: string;
  NANOCODEX_HOST_PROJECT_SECRET?: string;
}>;

export type HostClaims = Readonly<{
  issuer: "better-auth:github";
  tenant: string;
  subject: string;
  sessionId: string;
}>;

export type AuthServer = Readonly<{
  api: Readonly<{
    getSession(options: Readonly<{ headers: Headers }>): Promise<Readonly<{
      user?: Readonly<{ id?: string }>;
      session?: Readonly<{ token?: string }>;
    }> | null>;
  }>;
  handler(request: Request): Promise<Response>;
}>;

export type HostPrincipalIssuer = Readonly<{
  handler(options: Readonly<{
    authenticate(request: Request): Promise<HostClaims | undefined>;
  }>): (request: Request) => Promise<Response>;
  revoke(options: HostClaims & Readonly<{ signal?: AbortSignal }>): Promise<void>;
}>;

export type RouterDependencies = Readonly<{
  auth(env: WorkerEnv): AuthServer;
  principal(env: WorkerEnv, configuration: PrivateConfiguration): HostPrincipalIssuer;
}>;

export type PrivateConfiguration = Readonly<{
  appId: string;
  appOrigin: string;
  githubClientId: string;
  githubClientSecret: string;
  betterAuthSecret: string;
  hostProjectSecret: string;
}>;

export async function routeRequest(
  request: Request,
  env: WorkerEnv,
  dependencies: RouterDependencies,
): Promise<Response> {
  const url = new URL(request.url);
  const configuration = privateConfiguration(env);

  if (url.pathname === CONFIG_PATH) {
    if (request.method !== "GET") return methodNotAllowed("GET");
    return json(configuration
      ? { configured: true, appId: configuration.appId, appOrigin: configuration.appOrigin }
      : { configured: false });
  }

  if (url.pathname === SESSION_PATH) {
    if (request.method !== "GET") return methodNotAllowed("GET");
    if (!configuration) return json({ authenticated: false });
    const claims = await authenticateBetterAuth(
      request,
      dependencies.auth(env),
      configuration.githubClientId,
    );
    return json({ authenticated: Boolean(claims) });
  }

  if (url.pathname === PRINCIPAL_PATH) {
    if (request.method !== "POST" && request.method !== "DELETE") {
      return methodNotAllowed("POST, DELETE");
    }
    if (!configuration) return unavailable();
    if (!sameOriginRequest(request, configuration.appOrigin)) return forbidden();
    const auth = dependencies.auth(env);
    const principal = dependencies.principal(env, configuration);
    const authenticate = (incoming: Request) => authenticateBetterAuth(
      incoming,
      auth,
      configuration.githubClientId,
    );
    if (request.method === "POST") {
      return withSecurityHeaders(await principal.handler({ authenticate })(request));
    }
    const claims = await authenticate(request);
    if (!claims) return json({ error: "unauthorized" }, { status: 401 });
    await principal.revoke({ ...claims, signal: request.signal });
    return new Response(null, { status: 204, headers: securityHeaders() });
  }

  if (url.pathname === AUTH_BASE_PATH || url.pathname.startsWith(`${AUTH_BASE_PATH}/`)) {
    const method = AUTH_ROUTES.get(url.pathname);
    if (!method) return notFound();
    if (request.method !== method) return methodNotAllowed(method);
    if (!configuration) return unavailable();
    if (url.pathname !== `${AUTH_BASE_PATH}/callback/github`
      && !sameOriginRequest(request, configuration.appOrigin)) return forbidden();
    if (url.pathname === `${AUTH_BASE_PATH}/sign-in/social`
      && !await exactGithubSignIn(request, configuration.appOrigin)) return forbidden();
    return withSecurityHeaders(await dependencies.auth(env).handler(request));
  }

  if (url.pathname.startsWith("/api/")) return notFound();
  return env.ASSETS.fetch(request);
}

export async function authenticateBetterAuth(
  request: Request,
  auth: AuthServer,
  tenant: string,
): Promise<HostClaims | undefined> {
  const boundedTenant = boundedClaim(tenant);
  if (!boundedTenant) return undefined;
  try {
    const session = await auth.api.getSession({ headers: request.headers });
    const subject = boundedClaim(session?.user?.id);
    const sessionId = boundedClaim(session?.session?.token);
    if (!subject || !sessionId) return undefined;
    return {
      issuer: "better-auth:github",
      tenant: boundedTenant,
      subject,
      sessionId,
    };
  } catch {
    return undefined;
  }
}

export function canonicalBetterAuthSecret(value: string | undefined): boolean {
  const payload = value?.match(BETTER_AUTH_SECRET_V1)?.[1];
  if (!payload) return false;
  try {
    const bytes = Uint8Array.from(
      atob(`${payload.replace(/-/g, "+").replace(/_/g, "/")}=`),
      (character) => character.charCodeAt(0),
    );
    if (bytes.length !== 32 || base64Url(bytes) !== payload) return false;
    return !isDegenerateSecret(bytes);
  } catch {
    return false;
  }
}

function privateConfiguration(env: WorkerEnv): PrivateConfiguration | undefined {
  const appId = APP_ID.test(env.NANOCODEX_HOST_APP_ID ?? "")
    ? env.NANOCODEX_HOST_APP_ID
    : undefined;
  const appOrigin = exactPublicOrigin(env.NANOCODEX_HOST_APP_ORIGIN);
  const githubClientId = boundedClaim(env.BETTER_AUTH_GITHUB_CLIENT_ID);
  const githubClientSecret = boundedSecret(env.BETTER_AUTH_GITHUB_CLIENT_SECRET, 1);
  const hostProjectSecret = boundedSecret(env.NANOCODEX_HOST_PROJECT_SECRET, 32);
  const betterAuthSecret = canonicalBetterAuthSecret(env.BETTER_AUTH_SECRET)
    ? env.BETTER_AUTH_SECRET
    : undefined;
  if (!appId || !appOrigin || !githubClientId || !githubClientSecret
    || !hostProjectSecret || !betterAuthSecret || !env.AUTH_DB) return undefined;
  return {
    appId,
    appOrigin,
    githubClientId,
    githubClientSecret,
    betterAuthSecret,
    hostProjectSecret,
  };
}

async function exactGithubSignIn(request: Request, appOrigin: string): Promise<boolean> {
  try {
    const body = await request.clone().json() as unknown;
    if (!body || typeof body !== "object" || Array.isArray(body)) return false;
    const value = body as Record<string, unknown>;
    if (Object.keys(value).some((key) => key !== "provider" && key !== "callbackURL")) {
      return false;
    }
    return value.provider === "github" && value.callbackURL === `${appOrigin}/`;
  } catch {
    return false;
  }
}

function sameOriginRequest(request: Request, appOrigin: string): boolean {
  if (request.headers.get("origin") !== appOrigin) return false;
  const fetchSite = request.headers.get("sec-fetch-site");
  return fetchSite === null || fetchSite === "same-origin";
}

function exactPublicOrigin(value: string | undefined): string | undefined {
  if (!value) return undefined;
  try {
    const url = new URL(value);
    const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname.toLowerCase());
    if (url.origin !== value || url.username || url.password
      || (url.protocol !== "https:" && !(url.protocol === "http:" && loopback))) {
      return undefined;
    }
    return url.origin;
  } catch {
    return undefined;
  }
}

function boundedClaim(value: string | undefined): string | undefined {
  return value && value.length <= 512 && !/[\u0000-\u001f\u007f]/.test(value)
    ? value
    : undefined;
}

function boundedSecret(value: string | undefined, minimum: number): string | undefined {
  return value && value.length >= minimum && value.length <= 512 && /^\S+$/.test(value)
    ? value
    : undefined;
}

function base64Url(bytes: Uint8Array): string {
  return btoa(String.fromCharCode(...bytes))
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

function isDegenerateSecret(bytes: Uint8Array): boolean {
  const constant = bytes.every((byte) => byte === bytes[0]);
  const ascending = bytes.every((byte, index) => byte === ((bytes[0]! + index) & 0xff));
  const descending = bytes.every((byte, index) => byte === ((bytes[0]! - index) & 0xff));
  const printable = bytes.every((byte) => byte >= 0x20 && byte <= 0x7e);
  return constant || ascending || descending || printable;
}

function withSecurityHeaders(response: Response): Response {
  const headers = new Headers(response.headers);
  for (const [name, value] of Object.entries(securityHeaders())) headers.set(name, value);
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers,
  });
}

function json(body: unknown, init?: ResponseInit): Response {
  return Response.json(body, {
    ...init,
    headers: { ...securityHeaders(), ...init?.headers },
  });
}

function securityHeaders(): Record<string, string> {
  return {
    "cache-control": "no-store",
    "x-content-type-options": "nosniff",
  };
}

function forbidden(): Response {
  return json({ error: "forbidden" }, { status: 403 });
}

function unavailable(): Response {
  return json({ error: "not_configured" }, { status: 503 });
}

function notFound(): Response {
  return json({ error: "not_found" }, { status: 404 });
}

function methodNotAllowed(allow: string): Response {
  return json({ error: "method_not_allowed" }, { status: 405, headers: { allow } });
}

type Fetcher = Readonly<{ fetch(request: Request): Promise<Response> }>;
