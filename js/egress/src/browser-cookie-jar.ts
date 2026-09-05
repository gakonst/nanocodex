export const MAX_BROWSER_COOKIE_JARS = 25;
export const MAX_BROWSER_COOKIES_PER_JAR = 300;
export const MAX_BROWSER_COOKIE_JAR_BODY_BYTES = 256 * 1024;
export const BROWSER_COOKIE_JAR_ID = /^[A-Za-z0-9_-]{22,64}$/;

const MAX_ORIGIN_BYTES = 2_048;
const MAX_PROFILE_ID_BYTES = 128;
const MAX_STORE_ID_BYTES = 128;
const MAX_COOKIE_NAME_BYTES = 4_096;
const MAX_COOKIE_VALUE_BYTES = 16 * 1024;
const MAX_COOKIE_DOMAIN_BYTES = 253;
const MAX_COOKIE_PATH_BYTES = 2_048;
const COOKIE_NAME = /^[^\u0000-\u0020\u007f()<>@,;:\\"/\[\]?={}]+$/;
const COOKIE_DOMAIN = /^(?:\.?)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)(?:\.(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?))*$/;
const SAME_SITE = new Set(["no_restriction", "lax", "strict", "unspecified"]);

export type BrowserCookiePartitionKey = Readonly<{
  topLevelSite: string;
  hasCrossSiteAncestor?: boolean;
}>;

export type BrowserCookieV1 = Readonly<{
  name: string;
  value: string;
  domain: string;
  path: string;
  hostOnly: boolean;
  secure: boolean;
  httpOnly: boolean;
  sameSite: "no_restriction" | "lax" | "strict" | "unspecified";
  session: boolean;
  expirationDate?: number;
  storeId: string;
  partitionKey?: BrowserCookiePartitionKey;
}>;

export type BrowserCookieJarUpsert = Readonly<{
  schemaVersion: 1;
  origin: string;
  profileId: string;
  storeId: string;
  revision: number;
  cookies: readonly BrowserCookieV1[];
}>;

export type BrowserCookieJarBinding = Readonly<{
  origin: string;
  profileId: string;
  storeId: string;
}>;

export type BrowserCookieJarDelete = BrowserCookieJarBinding & Readonly<{
  revision: number;
}>;

export type BrowserCookieJarV1 = BrowserCookieJarBinding & Readonly<{
  schemaVersion: 1;
  id: string;
  revision: number;
  updatedAt: number;
  cookies: readonly BrowserCookieV1[];
}>;

export type BrowserCookieJarMetadata = BrowserCookieJarBinding & Readonly<{
  id: string;
  revision: number;
  cookieCount: number;
  updatedAt: number;
}>;

export type BrowserCookieJarNames = BrowserCookieJarMetadata & Readonly<{
  cookieNames: readonly string[];
}>;

export function validateBrowserCookieJarUpsert(
  value: unknown,
): BrowserCookieJarUpsert | undefined {
  if (!isRecord(value) || !hasExactKeys(value, [
    "schema_version", "origin", "profile_id", "store_id", "revision", "cookies",
  ]) || value.schema_version !== 1 || !validRevision(value.revision)
    || !Array.isArray(value.cookies) || value.cookies.length > MAX_BROWSER_COOKIES_PER_JAR) {
    return undefined;
  }
  const binding = validateBindingFields(value);
  if (!binding) return undefined;
  const cookies: BrowserCookieV1[] = [];
  const identities = new Set<string>();
  for (const candidate of value.cookies) {
    const cookie = validateCookie(candidate, binding.origin, binding.storeId);
    if (!cookie) return undefined;
    const identity = cookieIdentity(cookie);
    if (identities.has(identity)) return undefined;
    identities.add(identity);
    cookies.push(cookie);
  }
  return {
    schemaVersion: 1,
    ...binding,
    revision: value.revision as number,
    cookies,
  };
}

export function validateBrowserCookieJarBinding(
  value: unknown,
): BrowserCookieJarBinding | undefined {
  return isRecord(value) && hasExactKeys(value, ["origin", "profile_id", "store_id"])
    ? validateBindingFields(value)
    : undefined;
}

export function validateBrowserCookieJarDelete(
  value: unknown,
): BrowserCookieJarDelete | undefined {
  if (!isRecord(value) || !hasExactKeys(value, [
    "origin", "profile_id", "store_id", "revision",
  ]) || !validRevision(value.revision)) return undefined;
  const binding = validateBindingFields(value);
  return binding ? { ...binding, revision: value.revision as number } : undefined;
}

export function validateStoredBrowserCookieJar(
  id: string,
  value: unknown,
): BrowserCookieJarV1 | undefined {
  if (!BROWSER_COOKIE_JAR_ID.test(id) || !isRecord(value) || !hasExactKeys(value, [
    "schemaVersion", "id", "origin", "profileId", "storeId", "revision", "updatedAt", "cookies",
  ]) || value.schemaVersion !== 1 || value.id !== id || !validPositiveRevision(value.revision)
    || !validTimestamp(value.updatedAt) || !Array.isArray(value.cookies)
    || value.cookies.length > MAX_BROWSER_COOKIES_PER_JAR) return undefined;
  const binding = validateStoredBindingFields(value);
  if (!binding) return undefined;
  const cookies: BrowserCookieV1[] = [];
  const identities = new Set<string>();
  for (const candidate of value.cookies) {
    const cookie = validateCookie(candidate, binding.origin, binding.storeId);
    if (!cookie) return undefined;
    const identity = cookieIdentity(cookie);
    if (identities.has(identity)) return undefined;
    identities.add(identity);
    cookies.push(cookie);
  }
  return {
    schemaVersion: 1,
    id,
    ...binding,
    revision: value.revision as number,
    updatedAt: value.updatedAt as number,
    cookies,
  };
}

export function validateStoredBrowserCookieJarMetadata(
  id: string,
  value: unknown,
): BrowserCookieJarMetadata | undefined {
  if (!BROWSER_COOKIE_JAR_ID.test(id) || !isRecord(value) || !hasExactKeys(value, [
    "id", "origin", "profileId", "storeId", "revision", "cookieCount", "updatedAt",
  ]) || value.id !== id || !validPositiveRevision(value.revision)
    || !Number.isSafeInteger(value.cookieCount) || (value.cookieCount as number) < 0
    || (value.cookieCount as number) > MAX_BROWSER_COOKIES_PER_JAR
    || !validTimestamp(value.updatedAt)) return undefined;
  const binding = validateStoredBindingFields(value);
  return binding ? {
    id,
    ...binding,
    revision: value.revision as number,
    cookieCount: value.cookieCount as number,
    updatedAt: value.updatedAt as number,
  } : undefined;
}

export function browserCookieJarMetadata(
  jar: BrowserCookieJarV1,
): BrowserCookieJarMetadata {
  return {
    id: jar.id,
    origin: jar.origin,
    profileId: jar.profileId,
    storeId: jar.storeId,
    revision: jar.revision,
    cookieCount: jar.cookies.length,
    updatedAt: jar.updatedAt,
  };
}

export function sameBrowserCookieJarBinding(
  left: BrowserCookieJarBinding,
  right: BrowserCookieJarBinding,
): boolean {
  return left.origin === right.origin && left.profileId === right.profileId
    && left.storeId === right.storeId;
}

export function sameBrowserCookieJarMetadata(
  left: BrowserCookieJarMetadata,
  right: BrowserCookieJarMetadata,
): boolean {
  return left.id === right.id && sameBrowserCookieJarBinding(left, right)
    && left.revision === right.revision && left.cookieCount === right.cookieCount
    && left.updatedAt === right.updatedAt;
}

export function publicBrowserCookieJarMetadata(metadata: BrowserCookieJarMetadata): Record<string, unknown> {
  return {
    id: metadata.id,
    origin: metadata.origin,
    profile_id: metadata.profileId,
    store_id: metadata.storeId,
    revision: metadata.revision,
    cookie_count: metadata.cookieCount,
    updated_at: metadata.updatedAt,
  };
}

export function publicBrowserCookieJar(jar: BrowserCookieJarV1): Record<string, unknown> {
  return {
    schema_version: 1,
    id: jar.id,
    origin: jar.origin,
    profile_id: jar.profileId,
    store_id: jar.storeId,
    revision: jar.revision,
    updated_at: jar.updatedAt,
    cookies: jar.cookies,
  };
}

export function publicBrowserCookieJarNames(jar: BrowserCookieJarV1): Record<string, unknown> {
  const metadata = browserCookieJarMetadata(jar);
  return {
    id: metadata.id,
    origin: metadata.origin,
    profile_id: metadata.profileId,
    store_id: metadata.storeId,
    revision: metadata.revision,
    updated_at: metadata.updatedAt,
    cookie_count: metadata.cookieCount,
    cookie_names: [...new Set(jar.cookies.map((cookie) => cookie.name))]
      .sort((left, right) => left < right ? -1 : left > right ? 1 : 0),
  };
}

function validateBindingFields(value: Record<string, unknown>): BrowserCookieJarBinding | undefined {
  const origin = canonicalHttpOrigin(value.origin);
  const profileId = boundedIdentifier(value.profile_id, MAX_PROFILE_ID_BYTES);
  const storeId = boundedIdentifier(value.store_id, MAX_STORE_ID_BYTES);
  return origin && profileId && storeId ? { origin, profileId, storeId } : undefined;
}

function validateStoredBindingFields(value: Record<string, unknown>): BrowserCookieJarBinding | undefined {
  const origin = canonicalHttpOrigin(value.origin);
  const profileId = boundedIdentifier(value.profileId, MAX_PROFILE_ID_BYTES);
  const storeId = boundedIdentifier(value.storeId, MAX_STORE_ID_BYTES);
  return origin && profileId && storeId ? { origin, profileId, storeId } : undefined;
}

function validateCookie(
  value: unknown,
  origin: string,
  storeId: string,
): BrowserCookieV1 | undefined {
  if (!isRecord(value)) return undefined;
  const hasExpiration = Object.prototype.hasOwnProperty.call(value, "expirationDate");
  const hasPartition = Object.prototype.hasOwnProperty.call(value, "partitionKey");
  const expected = [
    "name", "value", "domain", "path", "hostOnly", "secure", "httpOnly", "sameSite",
    "session", ...(hasExpiration ? ["expirationDate"] : []), "storeId",
    ...(hasPartition ? ["partitionKey"] : []),
  ];
  if (!hasExactKeys(value, expected)) return undefined;
  const name = cookieName(value.name);
  const cookieValue = cookieText(value.value, MAX_COOKIE_VALUE_BYTES, true);
  const domain = cookieDomain(value.domain, value.hostOnly, origin);
  const path = cookiePath(value.path);
  if (!name || cookieValue === undefined || !domain || !path
    || typeof value.hostOnly !== "boolean" || typeof value.secure !== "boolean"
    || typeof value.httpOnly !== "boolean" || typeof value.session !== "boolean"
    || typeof value.sameSite !== "string" || !SAME_SITE.has(value.sameSite)
    || value.storeId !== storeId) return undefined;
  if (value.session === hasExpiration) return undefined;
  if (hasExpiration && (typeof value.expirationDate !== "number"
    || !Number.isFinite(value.expirationDate) || value.expirationDate <= 0)) return undefined;
  const partitionKey = hasPartition ? validatePartitionKey(value.partitionKey) : undefined;
  if (hasPartition && !partitionKey) return undefined;
  return {
    name,
    value: cookieValue,
    domain,
    path,
    hostOnly: value.hostOnly,
    secure: value.secure,
    httpOnly: value.httpOnly,
    sameSite: value.sameSite as BrowserCookieV1["sameSite"],
    session: value.session,
    ...(hasExpiration ? { expirationDate: value.expirationDate as number } : {}),
    storeId,
    ...(partitionKey ? { partitionKey } : {}),
  };
}

function validatePartitionKey(value: unknown): BrowserCookiePartitionKey | undefined {
  if (!isRecord(value)) return undefined;
  const hasCrossSiteAncestor = Object.prototype.hasOwnProperty.call(value, "hasCrossSiteAncestor");
  if (!hasExactKeys(value, ["topLevelSite", ...(hasCrossSiteAncestor ? ["hasCrossSiteAncestor"] : [])])
    || (hasCrossSiteAncestor && typeof value.hasCrossSiteAncestor !== "boolean")) return undefined;
  const topLevelSite = canonicalHttpOrigin(value.topLevelSite);
  return topLevelSite ? {
    topLevelSite,
    ...(hasCrossSiteAncestor
      ? { hasCrossSiteAncestor: value.hasCrossSiteAncestor as boolean }
      : {}),
  } : undefined;
}

function canonicalHttpOrigin(value: unknown): string | undefined {
  if (typeof value !== "string" || !value || byteLength(value) > MAX_ORIGIN_BYTES) return undefined;
  let url: URL;
  try { url = new URL(value); } catch { return undefined; }
  if (value !== url.origin || url.username || url.password || url.pathname !== "/"
    || url.search || url.hash || (url.protocol !== "https:" && url.protocol !== "http:")) {
    return undefined;
  }
  if (url.protocol === "http:" && !isLoopbackHostname(url.hostname)) return undefined;
  return value;
}

function cookieDomain(value: unknown, hostOnly: unknown, origin: string): string | undefined {
  if (typeof hostOnly !== "boolean" || typeof value !== "string" || !value
    || value !== value.toLowerCase() || byteLength(value) > MAX_COOKIE_DOMAIN_BYTES
    || !COOKIE_DOMAIN.test(value)) return undefined;
  const hostname = new URL(origin).hostname.toLowerCase().replace(/^\[|\]$/g, "");
  const bare = value.startsWith(".") ? value.slice(1) : value;
  if (hostOnly) return value === hostname && !value.startsWith(".") ? value : undefined;
  if (isIpLiteral(hostname) || isLoopbackHostname(hostname) || bare.split(".").length < 2) {
    return undefined;
  }
  return hostname === bare || hostname.endsWith(`.${bare}`) ? value : undefined;
}

function cookieName(value: unknown): string | undefined {
  const name = cookieText(value, MAX_COOKIE_NAME_BYTES, false);
  return name && COOKIE_NAME.test(name) ? name : undefined;
}

function cookiePath(value: unknown): string | undefined {
  const path = cookieText(value, MAX_COOKIE_PATH_BYTES, false);
  return path?.startsWith("/") ? path : undefined;
}

function cookieText(value: unknown, maxBytes: number, allowEmpty: boolean): string | undefined {
  return typeof value === "string" && (allowEmpty || value.length > 0)
    && !/[\u0000-\u001f\u007f]/.test(value) && byteLength(value) <= maxBytes
    ? value : undefined;
}

function boundedIdentifier(value: unknown, maxBytes: number): string | undefined {
  return typeof value === "string" && /^[A-Za-z0-9][A-Za-z0-9._:-]*$/.test(value)
    && byteLength(value) <= maxBytes ? value : undefined;
}

function cookieIdentity(cookie: BrowserCookieV1): string {
  return JSON.stringify([
    cookie.name, cookie.domain, cookie.hostOnly, cookie.path, cookie.storeId,
    cookie.partitionKey?.topLevelSite ?? null,
    cookie.partitionKey?.hasCrossSiteAncestor ?? null,
  ]);
}

function validRevision(value: unknown): boolean {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function validPositiveRevision(value: unknown): boolean {
  return validRevision(value) && (value as number) > 0;
}

function validTimestamp(value: unknown): boolean {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isLoopbackHostname(value: string): boolean {
  const hostname = value.toLowerCase().replace(/^\[|\]$/g, "");
  return hostname === "localhost" || hostname === "127.0.0.1" || hostname === "::1";
}

function isIpLiteral(value: string): boolean {
  return value.includes(":") || /^\d{1,3}(?:\.\d{1,3}){3}$/.test(value);
}

function hasExactKeys(value: Record<string, unknown>, expected: readonly string[]): boolean {
  const keys = new Set(expected);
  return Object.keys(value).length === keys.size && Object.keys(value).every((key) => keys.has(key));
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function byteLength(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}
