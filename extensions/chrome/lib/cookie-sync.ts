export const COOKIE_JAR_SCHEMA_VERSION = 1 as const;
export const MAX_COOKIE_COUNT = 300;

export type BrowserCookieSameSite = "no_restriction" | "lax" | "strict" | "unspecified";

export interface BrowserCookiePartitionKey {
  topLevelSite: string;
  hasCrossSiteAncestor?: boolean;
}

export interface BrowserCookie {
  name: string;
  value: string;
  domain: string;
  path: string;
  hostOnly: boolean;
  secure: boolean;
  httpOnly: boolean;
  sameSite: BrowserCookieSameSite;
  session: boolean;
  expirationDate?: number;
  storeId: string;
  partitionKey?: BrowserCookiePartitionKey;
}

export interface BrowserCookieJarV1 {
  schema_version: typeof COOKIE_JAR_SCHEMA_VERSION;
  origin: string;
  profile_id: string;
  store_id: string;
  revision: number;
  cookies: BrowserCookie[];
}

export interface BrowserCookieJarFence {
  origin: string;
  profile_id: string;
  store_id: string;
}

export interface CookieCaptureHandle {
  capture_id: string;
  lease_id: string;
  origin: string;
  profile_id: string;
  store_id: string;
  cookie_count: number;
  captured_at_ms: number;
}

export interface SyncedCookieJarReference {
  jar_id: string;
  lease_id: string;
  origin: string;
  profile_id: string;
  store_id: string;
  cookie_count: number;
  revision: number;
}

export interface CookieJarMetadata extends BrowserCookieJarFence {
  id: string;
  revision: number;
  cookie_count: number;
  updated_at?: number;
}

export interface CookieRestoreConfirmation {
  confirmation_id: string;
  origin: string;
  cookie_count: number;
}

/**
 * Authenticated Connect owns the implementation of this interface. Cookie values
 * must be passed directly between that client boundary and the background worker;
 * callers must not put a jar in DOM/React state or any Storage implementation.
 */
export interface CookieSyncTransport {
  list(fence: BrowserCookieJarFence): Promise<readonly CookieJarMetadata[]>;
  replace(jarId: string, jar: BrowserCookieJarV1): Promise<CookieJarMetadata>;
  materialize(jarId: string, fence: BrowserCookieJarFence): Promise<BrowserCookieJarV1>;
  delete(jarId: string, fence: BrowserCookieJarFence, revision: number): Promise<void>;
}

type ChromeCookieLike = Readonly<{
  name: unknown;
  value: unknown;
  domain: unknown;
  path: unknown;
  hostOnly: unknown;
  secure: unknown;
  httpOnly: unknown;
  sameSite: unknown;
  session: unknown;
  expirationDate?: unknown;
  storeId: unknown;
  partitionKey?: unknown;
}>;

export function createCookieJar(
  fence: BrowserCookieJarFence,
  source: readonly ChromeCookieLike[],
  revision = 0,
): BrowserCookieJarV1 {
  const expected = validateFence(fence);
  if (!Number.isSafeInteger(revision) || revision < 0) throw new Error("Cookie jar revision is invalid.");
  if (source.length > MAX_COOKIE_COUNT) throw new Error("This site has too many cookies to sync safely.");
  const cookies = source.map((cookie) => validateCookie(cookie, expected));
  const identities = new Set<string>();
  for (const cookie of cookies) {
    const identity = cookieIdentity(cookie);
    if (identities.has(identity)) throw new Error("The cookie jar contains duplicate cookie identities.");
    identities.add(identity);
  }
  return {
    schema_version: COOKIE_JAR_SCHEMA_VERSION,
    ...expected,
    revision,
    cookies,
  };
}

export function validateCookieJar(value: unknown): BrowserCookieJarV1 {
  const record = exactRecord(value, [
    "schema_version",
    "origin",
    "profile_id",
    "store_id",
    "revision",
    "cookies",
  ], "Cookie jar");
  if (record.schema_version !== COOKIE_JAR_SCHEMA_VERSION) {
    throw new Error("The cookie jar schema version is unsupported.");
  }
  if (!Array.isArray(record.cookies)) throw new Error("Cookie jar cookies are invalid.");
  return createCookieJar({
    origin: requiredString(record.origin, "Cookie jar origin"),
    profile_id: requiredString(record.profile_id, "Cookie jar profile"),
    store_id: requiredString(record.store_id, "Cookie jar store"),
  }, record.cookies as ChromeCookieLike[], requiredRevision(record.revision));
}

export function assertCookieJarFence(jar: BrowserCookieJarV1, expected: BrowserCookieJarFence): void {
  const fence = validateFence(expected);
  if (jar.origin !== fence.origin || jar.profile_id !== fence.profile_id || jar.store_id !== fence.store_id) {
    throw new Error("The cookie jar does not belong to this exact site and browser profile.");
  }
  if (jar.cookies.some((cookie) => cookie.storeId !== fence.store_id)) {
    throw new Error("The cookie jar contains cookies from another browser store.");
  }
}

export function cookieSetDetails(cookie: BrowserCookie, originValue: string): chrome.cookies.SetDetails {
  const origin = canonicalOrigin(originValue);
  assertCookieAppliesToOrigin(cookie, origin);
  return {
    url: cookieUrl(cookie, origin),
    name: cookie.name,
    value: cookie.value,
    ...(cookie.hostOnly ? {} : { domain: cookie.domain }),
    path: cookie.path,
    secure: cookie.secure,
    httpOnly: cookie.httpOnly,
    sameSite: cookie.sameSite,
    ...(cookie.session ? {} : { expirationDate: cookie.expirationDate }),
    storeId: cookie.storeId,
    ...(cookie.partitionKey ? { partitionKey: { ...cookie.partitionKey } } : {}),
  };
}

export function cookieRemovalDetails(cookie: BrowserCookie, originValue: string): chrome.cookies.CookieDetails {
  const origin = canonicalOrigin(originValue);
  assertCookieAppliesToOrigin(cookie, origin);
  return {
    url: cookieUrl(cookie, origin),
    name: cookie.name,
    storeId: cookie.storeId,
    ...(cookie.partitionKey ? { partitionKey: { ...cookie.partitionKey } } : {}),
  };
}

function validateFence(value: BrowserCookieJarFence): BrowserCookieJarFence {
  return {
    origin: canonicalOrigin(value.origin),
    profile_id: requiredString(value.profile_id, "Cookie jar profile"),
    store_id: requiredString(value.store_id, "Cookie jar store"),
  };
}

function validateCookie(value: ChromeCookieLike, fence: BrowserCookieJarFence): BrowserCookie {
  const cookie: BrowserCookie = {
    name: stringValue(value.name, "Cookie name"),
    value: stringValue(value.value, "Cookie value"),
    domain: requiredString(value.domain, "Cookie domain"),
    path: requiredString(value.path, "Cookie path"),
    hostOnly: booleanValue(value.hostOnly, "Cookie hostOnly"),
    secure: booleanValue(value.secure, "Cookie secure"),
    httpOnly: booleanValue(value.httpOnly, "Cookie httpOnly"),
    sameSite: sameSiteValue(value.sameSite),
    session: booleanValue(value.session, "Cookie session"),
    storeId: requiredString(value.storeId, "Cookie store"),
    ...(value.partitionKey === undefined ? {} : { partitionKey: partitionKeyValue(value.partitionKey) }),
  };
  if (!cookie.path.startsWith("/") || /[\u0000-\u001f\u007f]/.test(cookie.path)) {
    throw new Error("Cookie path is unsupported.");
  }
  if (byteLength(cookie.name) > 4096 || byteLength(cookie.value) > 16 * 1024
    || byteLength(cookie.domain) > 253 || byteLength(cookie.path) > 2048) {
    throw new Error("Cookie data exceeds the supported sync bounds.");
  }
  if (/[\u0000-\u001f\u007f]/.test(cookie.name) || /[\u0000\r\n]/.test(cookie.value)) {
    throw new Error("Cookie data contains unsupported control characters.");
  }
  if (cookie.storeId !== fence.store_id) throw new Error("Cookie belongs to another browser store.");
  if (cookie.session) {
    if (value.expirationDate !== undefined) throw new Error("Session cookie has an expiration date.");
  } else {
    if (typeof value.expirationDate !== "number" || !Number.isFinite(value.expirationDate) || value.expirationDate <= 0) {
      throw new Error("Persistent cookie expiration is invalid.");
    }
    cookie.expirationDate = value.expirationDate;
  }
  assertCookieAppliesToOrigin(cookie, fence.origin);
  return cookie;
}

function assertCookieAppliesToOrigin(cookie: BrowserCookie, originValue: string): void {
  const origin = new URL(canonicalOrigin(originValue));
  const rawDomain = cookie.domain.startsWith(".") ? cookie.domain.slice(1) : cookie.domain;
  const domain = rawDomain.toLowerCase();
  if (!domain || /[\s/\\]/.test(domain)) throw new Error("Cookie domain is unsupported.");
  const applies = cookie.hostOnly
    ? origin.hostname === domain
    : origin.hostname === domain || origin.hostname.endsWith(`.${domain}`);
  if (!applies) throw new Error("Cookie does not apply to the exact jar origin.");
  if (cookie.secure && origin.protocol !== "https:") {
    throw new Error("Secure cookie cannot be restored to this origin.");
  }
}

function partitionKeyValue(value: unknown): BrowserCookiePartitionKey {
  const record = exactRecord(value, ["topLevelSite", "hasCrossSiteAncestor"], "Cookie partition key");
  const topLevelSite = canonicalOrigin(requiredString(record.topLevelSite, "Cookie partition top-level site"));
  if (record.hasCrossSiteAncestor !== undefined && typeof record.hasCrossSiteAncestor !== "boolean") {
    throw new Error("Cookie partition ancestor state is unsupported.");
  }
  return {
    topLevelSite,
    ...(record.hasCrossSiteAncestor === undefined ? {} : { hasCrossSiteAncestor: record.hasCrossSiteAncestor }),
  };
}

function cookieUrl(cookie: BrowserCookie, originValue: string): string {
  const url = new URL(originValue);
  url.pathname = cookie.path;
  url.search = "";
  url.hash = "";
  return url.href;
}

function cookieIdentity(cookie: BrowserCookie): string {
  return JSON.stringify([
    cookie.name,
    cookie.domain,
    cookie.path,
    cookie.hostOnly,
    cookie.storeId,
    cookie.partitionKey?.topLevelSite ?? "",
    cookie.partitionKey?.hasCrossSiteAncestor ?? null,
  ]);
}

function canonicalOrigin(value: string): string {
  const url = new URL(value);
  if ((url.protocol !== "http:" && url.protocol !== "https:")
    || url.username || url.password || url.pathname !== "/" || url.search || url.hash) {
    throw new Error("Cookie jar origin must be one exact HTTP(S) origin.");
  }
  return url.origin;
}

function exactRecord(value: unknown, allowed: readonly string[], label: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} is invalid.`);
  const record = value as Record<string, unknown>;
  const expected = new Set(allowed);
  if (Object.keys(record).some((key) => !expected.has(key))) throw new Error(`${label} has unsupported fields.`);
  return record;
}

function stringValue(value: unknown, label: string): string {
  if (typeof value !== "string") throw new Error(`${label} is invalid.`);
  return value;
}

function requiredString(value: unknown, label: string): string {
  const result = stringValue(value, label);
  if (!result) throw new Error(`${label} is invalid.`);
  return result;
}

function booleanValue(value: unknown, label: string): boolean {
  if (typeof value !== "boolean") throw new Error(`${label} is invalid.`);
  return value;
}

function sameSiteValue(value: unknown): BrowserCookieSameSite {
  if (value !== "no_restriction" && value !== "lax" && value !== "strict" && value !== "unspecified") {
    throw new Error("Cookie SameSite state is unsupported.");
  }
  return value;
}

function requiredRevision(value: unknown): number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) throw new Error("Cookie jar revision is invalid.");
  return value as number;
}

function byteLength(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}
