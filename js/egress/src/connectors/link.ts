// Protocol: https://github.com/stripe/link-cli (device auth and Link SDK).
export const LINK_CLIENT_ID = "lwlpk_U7Qy7ThG69STZk";
export const LINK_SCOPES = ["userinfo:read", "payment_methods.agentic"] as const;
export const LINK_PATH = /^\/(?:userinfo|spend_requests(?:\/lsrq_[A-Za-z0-9]+(?:\/(?:request_approval|cancel))?)?)$/;

export function linkAuthRequest(path: "code" | "token" | "revoke", fields: Record<string, string>): Request {
  return new Request(`https://login.link.com/device/${path}`, {
    method: "POST",
    headers: { "content-type": "application/x-www-form-urlencoded", accept: "application/json" },
    body: new URLSearchParams({ client_id: LINK_CLIENT_ID, ...fields }),
  });
}

export function linkVerificationUrl(value: unknown): string {
  if (typeof value !== "string" || value.length > 4096) throw new TypeError("Invalid Link verification URL");
  const url = new URL(value);
  if (!["https://link.com", "https://app.link.com", "https://login.link.com"].includes(url.origin)
    || url.username || url.password || url.hash) throw new TypeError("Invalid Link verification URL");
  return url.href;
}

export function decodeLinkDevice(value: Record<string, unknown>) {
  return {
    deviceCode: bounded(value.device_code),
    userCode: bounded(value.user_code),
    authorizationUrl: linkVerificationUrl(value.verification_uri_complete),
    expiresIn: positive(value.expires_in, 3600),
    interval: value.interval === undefined ? 5 : positive(value.interval, 300),
  };
}

export function decodeLinkToken(value: Record<string, unknown>, previousScopes?: readonly string[]) {
  if (typeof value.token_type !== "string" || value.token_type.toLowerCase() !== "bearer") {
    throw new TypeError("Invalid Link token type");
  }
  // Link can omit the scope echo; the device request uses exactly LINK_SCOPES.
  const scopes = value.scope === undefined ? [...(previousScopes ?? LINK_SCOPES)] : bounded(value.scope).split(/\s+/);
  if (LINK_SCOPES.some(scope => !scopes.includes(scope)) || scopes.some(scope => !LINK_SCOPES.includes(scope as typeof LINK_SCOPES[number]))) {
    throw new TypeError("Invalid Link scopes");
  }
  return {
    accessToken: bounded(value.access_token), refreshToken: bounded(value.refresh_token),
    expiresIn: positive(value.expires_in, 31_536_000), scopes,
  };
}

export function decodeLinkIdentity(value: Record<string, unknown>) {
  const email = bounded(value.email).trim().toLowerCase();
  if (email.length > 256 || !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email)) throw new TypeError("Invalid Link identity");
  return { accountId: email, displayLabel: email };
}

/** Only approval requests and their status are exposed. Delegated approval and
 * credential expansion are deliberately outside this connector's authority. */
export function linkRequestAllowed(method: string, url: URL): boolean {
  if (!LINK_PATH.test(url.pathname)) return false;
  if (url.search && !(method === "GET" && url.pathname === "/spend_requests"
    && url.search === "?include_history=true")) return false;
  if (method === "GET") return !/\/(?:request_approval|cancel)$/.test(url.pathname);
  return method === "POST" && url.pathname.startsWith("/spend_requests");
}

export function linkBodyAllowed(value: unknown): boolean {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  // Fail closed on unrecognized fields that might enable delegated spending.
  const allowed = new Set(["merchant_name", "merchant_url", "context", "amount", "currency",
    "payment_method_id", "line_items", "totals", "metadata", "test", "credential_type", "network_id",
    "execution_method", "merchant_account_id"]);
  return Object.keys(value).every(key => allowed.has(key));
}

export function redactLinkCredentials(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(redactLinkCredentials);
  if (!value || typeof value !== "object") return value;
  return Object.fromEntries(Object.entries(value).filter(([key]) =>
    !["card", "shared_payment_token", "link_pay_token", "payment_details"].includes(key))
    .map(([key, item]) => [key, redactLinkCredentials(item)]));
}

function bounded(value: unknown): string {
  if (typeof value !== "string" || !value.trim() || value.length > 8192 || /[\x00-\x1f\x7f]/.test(value)) {
    throw new TypeError("Invalid Link response");
  }
  return value;
}
function positive(value: unknown, max: number): number {
  if (!Number.isSafeInteger(value) || (value as number) <= 0 || (value as number) > max) throw new TypeError("Invalid Link expiry");
  return value as number;
}
