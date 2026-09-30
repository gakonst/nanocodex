// Credentialless public transport still needs browser CORS checks: a page may
// only read cross-origin responses whose server authorizes its document origin.
const safeMethods = new Set(["GET", "HEAD", "POST"]);
const safeResponseHeaders = new Set([
  "cache-control",
  "content-language",
  "content-length",
  "content-type",
  "expires",
  "last-modified",
  "pragma",
]);
const unsafeBytes = /[\x00-\x08\x0a-\x1f\x7f"():<>?@\[\]\\{}]/;
function safeHeader(name, value) {
  if (new TextEncoder().encode(value).length > 128) return false;
  if (name === "accept") return !unsafeBytes.test(value);
  if (name === "accept-language" || name === "content-language")
    return /^[0-9A-Za-z *,\-.;=]*$/.test(value);
  if (name === "content-type")
    return (
      !unsafeBytes.test(value) &&
      [
        "application/x-www-form-urlencoded",
        "multipart/form-data",
        "text/plain",
      ].includes(value.split(";", 1)[0].trim().toLowerCase())
    );
  if (name === "range") {
    const match = /^bytes=(\d+)-(\d*)$/.exec(value);
    return !!match && (!match[2] || Number(match[1]) <= Number(match[2]));
  }
  return false;
}
const tokens = (headers, name) =>
  (headers.get(name) || "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
export function unsafeHeaderNames(headers) {
  const names = [],
    safe = [];
  let size = 0;
  for (const [name, value] of headers) {
    if (safeHeader(name, value)) {
      safe.push(name);
      size += new TextEncoder().encode(value).length;
    } else names.push(name);
  }
  // Fetch also caps the aggregate size of otherwise-safelisted headers.
  if (size > 1024) names.push(...safe);
  return names.sort();
}
export function checkCors(response, documentOrigin, credentials) {
  const allowed = response.headers.get("access-control-allow-origin");
  if (
    allowed !== documentOrigin &&
    !(allowed === "*" && credentials !== "include")
  )
    throw new TypeError("CORS origin denied");
  if (
    credentials === "include" &&
    response.headers.get("access-control-allow-credentials") !== "true"
  )
    throw new TypeError("CORS credentials denied");
}
export async function preflight(
  transport,
  url,
  method,
  headers,
  documentOrigin,
  credentials,
  signal,
) {
  const unsafe = unsafeHeaderNames(headers);
  if (safeMethods.has(method) && !unsafe.length) return;
  const requestHeaders = new Headers({
    origin: documentOrigin,
    "access-control-request-method": method,
  });
  if (unsafe.length)
    requestHeaders.set("access-control-request-headers", unsafe.join(","));
  const response = await transport.fetch(url, {
    method: "OPTIONS",
    headers: requestHeaders,
    redirect: "manual",
    credentials: "omit",
    signal,
  });
  try {
    if (
      !response.ok ||
      response.redirected ||
      (response.url && response.url !== url)
    )
      throw new TypeError("CORS preflight failed");
    checkCors(response, documentOrigin, credentials);
    const methods = tokens(response.headers, "access-control-allow-methods");
    if (
      !safeMethods.has(method) &&
      !methods.includes(method) &&
      !(credentials !== "include" && methods.includes("*"))
    )
      throw new TypeError("CORS method denied");
    const allowed = tokens(
      response.headers,
      "access-control-allow-headers",
    ).map((h) => h.toLowerCase());
    if (
      unsafe.some(
        (name) =>
          !allowed.includes(name) &&
          !(credentials !== "include" && allowed.includes("*")),
      )
    )
      throw new TypeError("CORS headers denied");
  } finally {
    await response.body?.cancel();
  }
}
export function exposedCorsHeaders(headers, credentials) {
  const allowed = tokens(headers, "access-control-expose-headers").map((h) =>
    h.toLowerCase(),
  );
  const result = new Headers();
  for (const [name, value] of headers) {
    if (name === "set-cookie" || name === "set-cookie2") continue;
    if (
      safeResponseHeaders.has(name) ||
      allowed.includes(name) ||
      (credentials !== "include" && allowed.includes("*"))
    )
      result.set(name, value);
  }
  return result;
}
