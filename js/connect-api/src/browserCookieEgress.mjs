const BROWSER_COOKIE_JAR_ID = /^[A-Za-z0-9_-]{22,64}$/;
const BROWSER_COOKIE_IDENTITY = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const MAX_BROWSER_COOKIE_JARS = 25;
const MAX_BROWSER_COOKIES = 300;
const MAX_COOKIE_NAME_BYTES = 4_096;
const COOKIE_NAME = /^[^\u0000-\u0020\u007f()<>@,;:\\"/\[\]?={}]+$/;
const BROKER_ERROR_CODES = new Set([
  "body_too_large",
  "browser_cookie_jar_binding_conflict",
  "browser_cookie_jar_limit_reached",
  "browser_cookie_jar_not_found",
  "browser_cookie_jar_revision_conflict",
  "invalid_browser_cookie_jar",
  "invalid_browser_cookie_jar_binding",
  "invalid_browser_cookie_jar_delete",
  "invalid_content_type",
  "rate_limited",
]);

/** Centralizes the private broker route so storage path changes have one owner. */
export function browserCookieBrokerPath(brokerUserId, jarId, projection = false) {
  if (typeof brokerUserId !== "string" || brokerUserId.length === 0) {
    throw new TypeError("invalid broker user");
  }
  const root = `/users/${encodeURIComponent(brokerUserId)}/credentials/browser-cookie-jars`;
  if (jarId === undefined) return root;
  if (!BROWSER_COOKIE_JAR_ID.test(jarId)) throw new TypeError("invalid browser cookie jar id");
  const suffix = projection === true || projection === "materialize"
    ? "/materialize"
    : projection === "names" ? "/names" : projection === false ? "" : undefined;
  if (suffix === undefined) throw new TypeError("invalid browser cookie jar projection");
  return `${root}/${jarId}${suffix}`;
}

export function projectBrowserCookieJarList(value, binding) {
  if (!isRecord(value) || !hasExactKeys(value, ["browser_cookie_jars"])
    || !Array.isArray(value.browser_cookie_jars)
    || value.browser_cookie_jars.length > MAX_BROWSER_COOKIE_JARS) {
    throw new TypeError("invalid browser cookie jar list");
  }
  return {
    browser_cookie_jars: value.browser_cookie_jars
      .map(projectBrowserCookieJarMetadata)
      .filter((metadata) => sameBinding(metadata, binding)),
  };
}

export function projectBrowserCookieJarMetadata(value) {
  if (!isRecord(value) || !hasExactKeys(value, [
    "id", "origin", "profile_id", "store_id", "revision", "cookie_count", "updated_at",
  ]) || !BROWSER_COOKIE_JAR_ID.test(String(value.id))
    || !validOrigin(value.origin) || !validIdentity(value.profile_id) || !validIdentity(value.store_id)
    || !positiveInteger(value.revision) || !nonnegativeInteger(value.cookie_count)
    || value.cookie_count > MAX_BROWSER_COOKIES || !nonnegativeInteger(value.updated_at)) {
    throw new TypeError("invalid browser cookie jar metadata");
  }
  return {
    id: value.id,
    origin: value.origin,
    profile_id: value.profile_id,
    store_id: value.store_id,
    revision: value.revision,
    cookie_count: value.cookie_count,
    updated_at: value.updated_at,
  };
}

export function projectBrowserCookieJarMaterialization(value, jarId, binding) {
  if (!isRecord(value) || !hasExactKeys(value, [
    "schema_version", "id", "origin", "profile_id", "store_id", "revision", "updated_at", "cookies",
  ]) || value.schema_version !== 1 || value.id !== jarId
    || !BROWSER_COOKIE_JAR_ID.test(jarId) || !sameBinding(value, binding)
    || !positiveInteger(value.revision) || !nonnegativeInteger(value.updated_at)
    || !Array.isArray(value.cookies) || value.cookies.length > MAX_BROWSER_COOKIES) {
    throw new TypeError("invalid browser cookie jar materialization");
  }
  return {
    schema_version: 1,
    id: jarId,
    origin: binding.origin,
    profile_id: binding.profile_id,
    store_id: binding.store_id,
    revision: value.revision,
    updated_at: value.updated_at,
    cookies: value.cookies.map((cookie) => projectBrowserCookie(cookie, binding.store_id)),
  };
}

export function projectBrowserCookieJarNames(value, jarId, binding) {
  if (!isRecord(value) || !hasExactKeys(value, [
    "id", "origin", "profile_id", "store_id", "revision", "updated_at", "cookie_count",
    "cookie_names",
  ]) || value.id !== jarId || !BROWSER_COOKIE_JAR_ID.test(jarId)
    || !sameBinding(value, binding) || !positiveInteger(value.revision)
    || !nonnegativeInteger(value.updated_at) || !nonnegativeInteger(value.cookie_count)
    || value.cookie_count > MAX_BROWSER_COOKIES || !Array.isArray(value.cookie_names)
    || value.cookie_names.length > value.cookie_count
    || value.cookie_names.some((name) => !validCookieName(name))
    || value.cookie_names.some((name, index) => index > 0 && value.cookie_names[index - 1] >= name)) {
    throw new TypeError("invalid browser cookie jar names");
  }
  return {
    id: jarId,
    origin: binding.origin,
    profile_id: binding.profile_id,
    store_id: binding.store_id,
    revision: value.revision,
    updated_at: value.updated_at,
    cookie_count: value.cookie_count,
    cookie_names: [...value.cookie_names],
  };
}

export function projectBrowserCookieBrokerError(value) {
  if (!isRecord(value) || typeof value.error !== "string" || !BROKER_ERROR_CODES.has(value.error)) {
    throw new TypeError("invalid browser cookie broker error");
  }
  return { error: value.error };
}

function projectBrowserCookie(value, storeId) {
  if (!isRecord(value)) throw new TypeError("invalid browser cookie");
  const hasExpiration = Object.prototype.hasOwnProperty.call(value, "expirationDate");
  const hasPartition = Object.prototype.hasOwnProperty.call(value, "partitionKey");
  if (!hasExactKeys(value, [
    "name", "value", "domain", "path", "hostOnly", "secure", "httpOnly", "sameSite", "session",
    ...(hasExpiration ? ["expirationDate"] : []), "storeId", ...(hasPartition ? ["partitionKey"] : []),
  ]) || typeof value.name !== "string" || typeof value.value !== "string"
    || typeof value.domain !== "string" || typeof value.path !== "string"
    || typeof value.hostOnly !== "boolean" || typeof value.secure !== "boolean"
    || typeof value.httpOnly !== "boolean" || typeof value.session !== "boolean"
    || !["no_restriction", "lax", "strict", "unspecified"].includes(value.sameSite)
    || value.storeId !== storeId
    || (hasExpiration && (typeof value.expirationDate !== "number" || !Number.isFinite(value.expirationDate)))) {
    throw new TypeError("invalid browser cookie");
  }
  let partitionKey;
  if (hasPartition) {
    const partition = value.partitionKey;
    const hasAncestor = isRecord(partition)
      && Object.prototype.hasOwnProperty.call(partition, "hasCrossSiteAncestor");
    if (!isRecord(partition)
      || !hasExactKeys(partition, ["topLevelSite", ...(hasAncestor ? ["hasCrossSiteAncestor"] : [])])
      || !validOrigin(partition.topLevelSite)
      || (hasAncestor && typeof partition.hasCrossSiteAncestor !== "boolean")) {
      throw new TypeError("invalid browser cookie partition");
    }
    partitionKey = {
      topLevelSite: partition.topLevelSite,
      ...(hasAncestor ? { hasCrossSiteAncestor: partition.hasCrossSiteAncestor } : {}),
    };
  }
  return {
    name: value.name,
    value: value.value,
    domain: value.domain,
    path: value.path,
    hostOnly: value.hostOnly,
    secure: value.secure,
    httpOnly: value.httpOnly,
    sameSite: value.sameSite,
    session: value.session,
    ...(hasExpiration ? { expirationDate: value.expirationDate } : {}),
    storeId,
    ...(partitionKey ? { partitionKey } : {}),
  };
}

function sameBinding(value, binding) {
  return isRecord(binding)
    && value.origin === binding.origin
    && value.profile_id === binding.profile_id
    && value.store_id === binding.store_id;
}

function validOrigin(value) {
  if (typeof value !== "string" || value.length === 0 || value.length > 2_048) return false;
  try {
    const url = new URL(value);
    return url.origin === value && (url.protocol === "https:" || url.protocol === "http:");
  } catch {
    return false;
  }
}

function validIdentity(value) {
  return typeof value === "string" && BROWSER_COOKIE_IDENTITY.test(value);
}

function validCookieName(value) {
  return typeof value === "string" && value.length > 0 && COOKIE_NAME.test(value)
    && new TextEncoder().encode(value).byteLength <= MAX_COOKIE_NAME_BYTES;
}

function positiveInteger(value) {
  return Number.isSafeInteger(value) && value > 0;
}

function nonnegativeInteger(value) {
  return Number.isSafeInteger(value) && value >= 0;
}

function hasExactKeys(value, expected) {
  const keys = new Set(expected);
  return Object.keys(value).length === keys.size && Object.keys(value).every((key) => keys.has(key));
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
