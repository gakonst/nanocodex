const VAULT_ID = /^[A-Za-z0-9_-]{22,64}$/;
const VAULT_ID_HEADER = "x-nanocodex-vault-id";
const METHODS = new Set(["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]);
const PRIVATE_HEADER = /(?:^|[-_])(?:auth(?:orization)?|cookie|credential|password|proxy|secret|token|api[-_]?key)(?:$|[-_]|\d)/i;
const VAULT_PLACEHOLDER = /\{\{NANOCODEX_VAULT_(?:USERNAME|PASSWORD|BASIC|CARD_NUMBER|EXPIRY_MONTH|EXPIRY_YEAR|CVV|BILLING_ZIP)\}\}/;
const FORBIDDEN_HEADERS = new Set([
  "connection", "cookie", "host", "origin", "proxy-authorization", "proxy-connection", "referer", "te", "trailer",
  "transfer-encoding", "upgrade", "x-nanocodex-subject", VAULT_ID_HEADER,
]);
const encoder = new TextEncoder();

/** Returns undefined for an ordinary request, otherwise an exact private Vault envelope. */
export function vaultEgressEnvelope(value) {
  const source = isRecord(value?.headers) ? value.headers : undefined;
  if (!source || !Object.prototype.hasOwnProperty.call(source, VAULT_ID_HEADER)) return undefined;
  const vaultId = source[VAULT_ID_HEADER];
  if (typeof vaultId !== "string" || !VAULT_ID.test(vaultId)) {
    throw new TypeError("invalid Vault item id");
  }
  if (typeof value.url !== "string" || value.url.length === 0 || value.url.length > 8_192) {
    throw new TypeError("invalid Vault request URL");
  }
  const method = value.method === undefined ? "GET"
    : typeof value.method === "string" ? value.method.toUpperCase() : "";
  if (!METHODS.has(method)) throw new TypeError("invalid Vault request method");
  if (value.body !== undefined
    && (typeof value.body !== "string" || encoder.encode(value.body).byteLength > 64 * 1024)) {
    throw new TypeError("invalid Vault request body");
  }
  if (value.body !== undefined && (method === "GET" || method === "HEAD")) {
    throw new TypeError("invalid Vault request body");
  }
  if (Object.keys(source).length > 65) throw new TypeError("invalid Vault request headers");
  const headers = new Headers();
  for (const [name, headerValue] of Object.entries(source)) {
    if (name === VAULT_ID_HEADER) continue;
    const lower = name.toLowerCase();
    const privatePlaceholder = typeof headerValue === "string"
      && PRIVATE_HEADER.test(name)
      && safeVaultHeaderValue(lower, headerValue);
    if (typeof headerValue !== "string" || name.length > 128 || headerValue.length > 4_096
      || (!privatePlaceholder && PRIVATE_HEADER.test(name)) || FORBIDDEN_HEADERS.has(lower)
      || lower.startsWith("cf-") || lower.startsWith("forwarded")
      || lower.startsWith("sec-") || lower.startsWith("x-forwarded-")
      || headers.has(lower)) {
      throw new TypeError("invalid Vault request headers");
    }
    try { headers.set(name, headerValue); } catch {
      throw new TypeError("invalid Vault request headers");
    }
  }
  const hasPlaceholder = [...headers.values()].some((headerValue) => VAULT_PLACEHOLDER.test(headerValue))
    || (typeof value.body === "string" && VAULT_PLACEHOLDER.test(value.body));
  if (!hasPlaceholder) throw new TypeError("Vault request requires a supported placeholder");
  return {
    vault_id: vaultId,
    url: value.url,
    method,
    headers: Object.fromEntries(headers.entries()),
    ...(value.body === undefined ? {} : { body: value.body }),
  };
}

function safeVaultHeaderValue(name, value) {
  if (name === "cookie" || name === "proxy-authorization") return false;
  if (name === "authorization") {
    return value === "Basic {{NANOCODEX_VAULT_BASIC}}"
      || value === "Bearer {{NANOCODEX_VAULT_PASSWORD}}";
  }
  return /^\{\{NANOCODEX_VAULT_(?:PASSWORD|BASIC|CARD_NUMBER|EXPIRY_MONTH|EXPIRY_YEAR|CVV|BILLING_ZIP)\}\}$/.test(value);
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
