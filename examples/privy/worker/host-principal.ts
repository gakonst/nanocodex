import { PrivyClient } from "@privy-io/node";
import { HostPrincipal } from "nanocodex/connect/server";

const PRINCIPAL_PATH = "/api/nanocodex/host-principal";
const CONFIG_PATH = "/api/config";
const HOST_SESSION_COOKIE = "__Host-nanocodex-privy-session";

export type Env = Readonly<{
  PRIVY_APP_ID?: string;
  PRIVY_APP_SECRET?: string;
  NANOCODEX_HOST_APP_ID?: string;
  NANOCODEX_HOST_APP_ORIGIN?: string;
  NANOCODEX_HOST_PROJECT_SECRET?: string;
  NANOCODEX_API_URL?: string;
  NANOCODEX_CONNECT_DIALOG_URL?: string;
}>;

export type HostClaims = Readonly<{
  issuer: "privy";
  tenant: string;
  subject: string;
  sessionId: string;
}>;

type PrivyVerifier = Readonly<{
  utils(): Readonly<{ auth(): Readonly<{
    verifyAuthToken(token: string): Promise<Readonly<{
      user_id?: string;
      session_id?: string;
    }>>;
  }> }>;
}>;

type Issuer = Readonly<{
  handler(options: Readonly<{
    authenticate(request: Request): Promise<HostClaims | undefined>;
  }>): (request: Request) => Promise<Response>;
  revoke(options: HostClaims & Readonly<{ signal?: AbortSignal }>): Promise<void>;
}>;

type Dependencies = Readonly<{
  createPrivy?(env: Env): PrivyVerifier;
  createIssuer?(configuration: HostConfiguration): Issuer;
}>;

type HostConfiguration = Readonly<{
  appId: string;
  appOrigin: string;
  secret: string;
  apiUrl: string;
  dialogUrl: string;
  privyAppId: string;
  privySecret: string;
}>;

export async function routeHostPrincipal(
  request: Request,
  env: Env,
  dependencies: Dependencies = {},
): Promise<Response | undefined> {
  const url = new URL(request.url);
  if (url.pathname === CONFIG_PATH && request.method === "GET") {
    const configuration = configurationFromEnv(env);
    if (!configuration) return unavailable();
    return json({
      appId: configuration.appId,
      appOrigin: configuration.appOrigin,
      privyAppId: configuration.privyAppId,
      nanocodexApiUrl: configuration.apiUrl,
      connectDialogUrl: configuration.dialogUrl,
    });
  }
  if (url.pathname !== PRINCIPAL_PATH) return undefined;
  if (request.method !== "POST" && request.method !== "DELETE") {
    return json({ error: "method_not_allowed" }, { status: 405, headers: { allow: "POST, DELETE" } });
  }

  const configuration = configurationFromEnv(env);
  if (!configuration) return unavailable();
  if (!sameOrigin(request, configuration.appOrigin)) {
    return json({ error: "forbidden" }, { status: 403 });
  }

  const privy = dependencies.createPrivy?.(env) ?? new PrivyClient({
    appId: configuration.privyAppId,
    appSecret: configuration.privySecret,
  });
  const issuer = dependencies.createIssuer?.(configuration) ?? HostPrincipal.create({
    appId: configuration.appId,
    appOrigin: configuration.appOrigin,
    secret: configuration.secret,
    baseUrl: configuration.apiUrl,
  });
  const authenticate = (incoming: Request) => authenticatePrivy(
    incoming,
    privy,
    configuration.privyAppId,
  );

  if (request.method === "POST") {
    const claims = await authenticate(request);
    if (!claims) return json({ error: "unauthorized" }, { status: 401 });
    const previousClaims = await readSealedClaims(request, configuration);
    if (previousClaims && !equalClaims(previousClaims, claims)) {
      await issuer.revoke({ ...previousClaims, signal: request.signal });
    }
    const response = await issuer.handler({ authenticate: async () => claims })(request);
    if (!response.ok) return response;
    const sealed = await sealClaims(claims, configuration);
    const forwarded = new Response(response.body, response);
    forwarded.headers.append("set-cookie", hostSessionCookie(sealed));
    return forwarded;
  }
  const claims = await readSealedClaims(request, configuration) ?? await authenticate(request);
  if (claims) await issuer.revoke({ ...claims, signal: request.signal });
  return new Response(null, {
    status: 204,
    headers: { ...securityHeaders(), "set-cookie": hostSessionCookie("", 0) },
  });
}

function equalClaims(left: HostClaims, right: HostClaims): boolean {
  return left.issuer === right.issuer && left.tenant === right.tenant
    && left.subject === right.subject && left.sessionId === right.sessionId;
}

export async function authenticatePrivy(
  request: Request,
  privy: PrivyVerifier,
  tenant: string,
): Promise<HostClaims | undefined> {
  const safeTenant = boundedClaim(tenant);
  const token = readCookie(request.headers.get("cookie"), "privy-token");
  if (!safeTenant || !token) return undefined;
  try {
    const claims = await privy.utils().auth().verifyAuthToken(token);
    const subject = boundedClaim(claims.user_id);
    const sessionId = boundedClaim(claims.session_id);
    if (!subject || !sessionId) return undefined;
    return { issuer: "privy", tenant: safeTenant, subject, sessionId };
  } catch {
    return undefined;
  }
}

function configurationFromEnv(env: Env): HostConfiguration | undefined {
  const appId = boundedClaim(env.NANOCODEX_HOST_APP_ID?.trim());
  const appOrigin = exactOrigin(env.NANOCODEX_HOST_APP_ORIGIN);
  const secret = env.NANOCODEX_HOST_PROJECT_SECRET?.trim();
  const privyAppId = boundedClaim(env.PRIVY_APP_ID?.trim());
  const privySecret = env.PRIVY_APP_SECRET?.trim();
  const apiUrl = exactOrigin(env.NANOCODEX_API_URL ?? "https://api.nanocodex.xyz");
  const dialogUrl = exactPublicUrl(
    env.NANOCODEX_CONNECT_DIALOG_URL
      ?? "https://nanocodex.gakonst.workers.dev/connect-dialog/",
  );
  if (!appId || !appOrigin || !secret || secret.length < 32 || !privyAppId || !privySecret
    || !apiUrl || !dialogUrl) return undefined;
  return { appId, appOrigin, secret, apiUrl, dialogUrl, privyAppId, privySecret };
}

function boundedClaim(value: string | undefined): string | undefined {
  return value && value.length <= 512 && !/[\u0000-\u001f\u007f]/.test(value) ? value : undefined;
}

function exactOrigin(value: string | undefined): string | undefined {
  if (!value) return undefined;
  try {
    const url = new URL(value);
    const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname.toLowerCase());
    return url.origin === value && (url.protocol === "https:" || (url.protocol === "http:" && loopback))
      ? url.origin : undefined;
  } catch {
    return undefined;
  }
}

function exactPublicUrl(value: string): string | undefined {
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password && !url.hash
      ? url.href : undefined;
  } catch {
    return undefined;
  }
}

function sameOrigin(request: Request, appOrigin: string): boolean {
  if (request.headers.get("origin") !== appOrigin) return false;
  const site = request.headers.get("sec-fetch-site");
  return site === null || site === "same-origin";
}

function readCookie(header: string | null, name: string): string | undefined {
  for (const part of (header ?? "").split(";")) {
    const [key, ...rawValue] = part.trim().split("=");
    if (key !== name) continue;
    try {
      return decodeURIComponent(rawValue.join("=")) || undefined;
    } catch {
      return undefined;
    }
  }
  return undefined;
}

async function sealClaims(claims: HostClaims, configuration: HostConfiguration): Promise<string> {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const key = await claimsKey(configuration.secret);
  const plaintext = new TextEncoder().encode(JSON.stringify(claims));
  const ciphertext = new Uint8Array(await crypto.subtle.encrypt({
    name: "AES-GCM",
    iv,
    additionalData: claimsContext(configuration),
  }, key, plaintext));
  const sealed = new Uint8Array(iv.length + ciphertext.length);
  sealed.set(iv);
  sealed.set(ciphertext, iv.length);
  return base64url(sealed);
}

async function readSealedClaims(
  request: Request,
  configuration: HostConfiguration,
): Promise<HostClaims | undefined> {
  const encoded = readCookie(request.headers.get("cookie"), HOST_SESSION_COOKIE);
  if (!encoded || !/^[A-Za-z0-9_-]{40,4096}$/.test(encoded)) return undefined;
  try {
    const sealed = fromBase64url(encoded);
    if (sealed.length <= 28) return undefined;
    const plaintext = await crypto.subtle.decrypt({
      name: "AES-GCM",
      iv: sealed.slice(0, 12),
      additionalData: claimsContext(configuration),
    }, await claimsKey(configuration.secret), sealed.slice(12));
    const value = JSON.parse(new TextDecoder().decode(plaintext)) as unknown;
    if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
    const record = value as Record<string, unknown>;
    if (Object.keys(record).sort().join(",") !== "issuer,sessionId,subject,tenant"
      || record.issuer !== "privy"
      || boundedClaim(typeof record.tenant === "string" ? record.tenant : undefined) !== configuration.privyAppId) {
      return undefined;
    }
    const subject = boundedClaim(typeof record.subject === "string" ? record.subject : undefined);
    const sessionId = boundedClaim(typeof record.sessionId === "string" ? record.sessionId : undefined);
    return subject && sessionId
      ? { issuer: "privy", tenant: configuration.privyAppId, subject, sessionId }
      : undefined;
  } catch {
    return undefined;
  }
}

async function claimsKey(secret: string): Promise<CryptoKey> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(secret));
  return crypto.subtle.importKey("raw", digest, "AES-GCM", false, ["encrypt", "decrypt"]);
}

function claimsContext(configuration: HostConfiguration): Uint8Array<ArrayBuffer> {
  return new TextEncoder().encode(
    `nanocodex:privy-host-session:v1\0${configuration.appId}\0${configuration.appOrigin}`,
  );
}

function hostSessionCookie(value: string, maxAge = 7 * 24 * 60 * 60): string {
  return `${HOST_SESSION_COOKIE}=${value}; Path=/; Max-Age=${maxAge}; HttpOnly; Secure; SameSite=Strict`;
}

function base64url(value: Uint8Array): string {
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function fromBase64url(value: string): Uint8Array {
  const padded = value.replaceAll("-", "+").replaceAll("_", "/").padEnd(Math.ceil(value.length / 4) * 4, "=");
  return Uint8Array.from(atob(padded), (character) => character.charCodeAt(0));
}

function unavailable(): Response {
  return json({ error: "host_auth_not_configured" }, { status: 503 });
}

function json(body: unknown, init: ResponseInit = {}): Response {
  return Response.json(body, {
    ...init,
    headers: { ...securityHeaders(), ...Object.fromEntries(new Headers(init.headers)) },
  });
}

function securityHeaders(): Record<string, string> {
  return {
    "cache-control": "no-store",
    "x-content-type-options": "nosniff",
  };
}
