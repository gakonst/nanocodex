const PROVIDER_PLACEHOLDER = "Bearer NANOCODEX_PROVIDER_CREDENTIAL";
const CONNECTOR_CONNECTION_HEADER = "x-nanocodex-connector-connection";
const CONNECTOR_CONNECTION = /^[A-Za-z0-9_-]{43}$/;
const SUBJECT = /^[A-Za-z0-9_-]{43,128}$/;
const VAULT_ID = /^[A-Za-z0-9_-]{22,64}$/;
export const VAULT_ID_HEADER = "x-nanocodex-vault-id";
const VAULT_EGRESS_URL = "https://vault-egress.internal/v1/request";
const MAX_VAULT_ENVELOPE_BYTES = 96 * 1024;
const MAX_VAULT_URL_BYTES = 8 * 1024;
const MAX_VAULT_BODY_BYTES = 64 * 1024;
const MAX_VAULT_HEADERS = 64;
const MAX_VAULT_HEADER_BYTES = 32 * 1024;
const MAX_VAULT_HEADER_NAME_BYTES = 128;
const MAX_VAULT_HEADER_VALUE_BYTES = 4 * 1024;
const ORDINARY_METHODS = new Set(["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]);
const BLOCKED_RESPONSE_HEADERS = new Set([
  "clear-site-data",
  "connection",
  "keep-alive",
  "nel",
  "proxy-authenticate",
  "proxy-authorization",
  "refresh",
  "report-to",
  "set-cookie",
  "set-cookie2",
  "trailer",
  "transfer-encoding",
  "upgrade",
  "x-nanocodex-connector-connection",
  "x-nanocodex-subject",
  "x-nanocodex-target-url",
]);

export type ManagedEgressConnectorId =
  | "github"
  | "gmail"
  | "gdrive"
  | "gcalendar"
  | "gtasks"
  | "gdocs"
  | "gsheets"
  | "gslides"
  | "gcontacts"
  | "slack"
  | "x";

/** True preserves the caller's selector; a connection id injects an authorized default. */
export type ManagedEgressConnectorAccess = boolean | string;

/** Resolves an exact-grant selector without allowing ambient provider defaults. */
export function exactConnectorAccess(
  approved: readonly string[],
  selected?: string,
): ManagedEgressConnectorAccess {
  if (selected !== undefined) return approved.includes(selected) ? selected : false;
  return approved.length === 1 ? approved[0]! : false;
}

type ProviderPolicy = Readonly<{
  connector: ManagedEgressConnectorId;
  path: (pathname: string) => boolean;
  publicWithoutSubject?: boolean;
}>;

const PROVIDERS = new Map<string, readonly ProviderPolicy[]>([
  ["github.com", [{
    connector: "github",
    path: (path) => /^\/[A-Za-z0-9_-]+\/[A-Za-z0-9_.-]+\/(?:info\/refs|git-upload-pack|git-receive-pack)$/.test(path),
    publicWithoutSubject: true,
  }]],
  ["api.github.com", [{
    connector: "github",
    path: (path) => path.startsWith("/"),
  }]],
  ["gmail.googleapis.com", [{
    connector: "gmail",
    path: (path) => path.startsWith("/gmail/v1/users/me/"),
  }]],
  ["www.googleapis.com", [
    {
      connector: "gdrive",
      path: (path) => path.startsWith("/drive/v3/") || path.startsWith("/upload/drive/v3/"),
    },
    { connector: "gcalendar", path: (path) => path.startsWith("/calendar/v3/") },
  ]],
  ["calendar.googleapis.com", [{
    connector: "gcalendar",
    path: (path) => path.startsWith("/calendar/v3/"),
  }]],
  ["tasks.googleapis.com", [{
    connector: "gtasks",
    path: (path) => path.startsWith("/tasks/v1/"),
  }]],
  ["docs.googleapis.com", [{
    connector: "gdocs",
    path: (path) => path.startsWith("/v1/documents/"),
  }]],
  ["sheets.googleapis.com", [{
    connector: "gsheets",
    path: (path) => path.startsWith("/v4/spreadsheets/"),
  }]],
  ["slides.googleapis.com", [{
    connector: "gslides",
    path: (path) => path.startsWith("/v1/presentations/"),
  }]],
  ["people.googleapis.com", [{
    connector: "gcontacts",
    path: (path) => /^\/v1\/(?:people|contactGroups|otherContacts)(?:\/|:|$)/.test(path),
  }]],
  ["slack.com", [{
    connector: "slack",
    path: (path) => /^\/api\/[A-Za-z0-9._-]+$/.test(path),
  }]],
  ["api.x.com", [{
    connector: "x",
    path: (path) => /^\/2\/(?:tweets|users|lists|dm_(?:conversations|events)|media)(?:\/|$)/.test(path),
  }]],
]);
const VAULT_PROVIDER_HOSTS = new Set([
  ...PROVIDERS.keys(),
  "api.openai.com",
  "chatgpt.com",
]);

const PRIVATE_HEADER = /(?:^|[-_])(?:auth(?:orization)?|cookie|credential|password|proxy|secret|token|api[-_]?key)(?:$|[-_]|\d)/i;
const FORBIDDEN_HEADERS = new Set([
  "connection", "host", "origin", "proxy-connection", "referer", "te", "trailer",
  "transfer-encoding", "upgrade", CONNECTOR_CONNECTION_HEADER, "x-nanocodex-subject",
  VAULT_ID_HEADER, "x-nanocodex-target-url",
]);
const VAULT_FORBIDDEN_HEADERS = new Set([
  "content-length", "cookie", "expect", "proxy-authorization", "via",
]);
const VAULT_BASIC_AUTHORIZATION = "Basic {{NANOCODEX_VAULT_BASIC}}";
const VAULT_BEARER_AUTHORIZATION = "Bearer {{NANOCODEX_VAULT_PASSWORD}}";
const VAULT_PLACEHOLDERS = new Set([
  "{{NANOCODEX_VAULT_API_KEY}}",
  "{{NANOCODEX_VAULT_USERNAME}}",
  "{{NANOCODEX_VAULT_PASSWORD}}",
  "{{NANOCODEX_VAULT_BASIC}}",
  "{{NANOCODEX_VAULT_CARD_NUMBER}}",
  "{{NANOCODEX_VAULT_EXPIRY_MONTH}}",
  "{{NANOCODEX_VAULT_EXPIRY_YEAR}}",
  "{{NANOCODEX_VAULT_CVV}}",
  "{{NANOCODEX_VAULT_BILLING_ZIP}}",
]);
const PRIVATE_HOST_SUFFIXES = [
  ".internal",
  ".invalid",
  ".local",
  ".localhost",
  ".test",
  ".home.arpa",
];

export async function handleManagedEgress(
  request: Request,
  binding: Fetcher,
  subject?: string,
  connectorAllowed: (
    connector: ManagedEgressConnectorId,
    connectionId?: string,
  ) => ManagedEgressConnectorAccess = () => true,
): Promise<Response> {
  if (request.headers.has(VAULT_ID_HEADER)) {
    return handleVaultEgress(request, binding, subject);
  }
  const method = request.method.toUpperCase();
  if (!ORDINARY_METHODS.has(method)) return failure(403, "method_denied");

  let url: URL;
  try { url = validateUrl(new URL(request.url)); } catch { return failure(403, "destination_denied"); }
  const candidate = providerFor(url);
  const provider = candidate?.publicWithoutSubject && subject === undefined ? undefined : candidate;
  const headerFailure = forbiddenHeader(request.headers, {
    allowConnectorConnection: provider !== undefined,
  });
  if (headerFailure) return failure(403, "credential_header_denied");
  if (!provider && PROVIDERS.has(url.hostname) && url.hostname !== "github.com") {
    return failure(403, "destination_denied");
  }
  if (provider) {
    const selected = request.headers.get(CONNECTOR_CONNECTION_HEADER) ?? undefined;
    if (selected !== undefined && !CONNECTOR_CONNECTION.test(selected)) {
      return failure(403, "connector_connection_invalid");
    }
    const access = connectorAllowed(provider.connector, selected);
    if (!access) return failure(403, "connector_forbidden");
    const resolved = typeof access === "string" ? access : selected;
    if (resolved !== undefined && !CONNECTOR_CONNECTION.test(resolved)) {
      return failure(403, "connector_connection_invalid");
    }
    if (!subject || !SUBJECT.test(subject)) return failure(403, "requires_login");
    if (!canonicalProviderPath(provider, url.pathname) || !provider.path(url.pathname)) {
      return failure(403, "connector_path_denied");
    }
    const headers = new Headers(request.headers);
    headers.set("authorization", PROVIDER_PLACEHOLDER);
    headers.set("x-nanocodex-subject", subject);
    if (resolved !== undefined) headers.set(CONNECTOR_CONNECTION_HEADER, resolved);
    return projectResponse(await binding.fetch(new Request(request.url, {
      method,
      headers,
      ...(method === "GET" || method === "HEAD" || !request.body ? {} : { body: request.body }),
      redirect: "manual",
      signal: request.signal,
    })));
  }

  const headers = new Headers(request.headers);
  headers.set("x-nanocodex-target-url", url.href);
  if (subject !== undefined) headers.set("x-nanocodex-subject", subject);
  return projectResponse(await binding.fetch(new Request("https://public-egress.internal/v1/request", {
    method,
    headers,
    ...(method === "GET" || method === "HEAD" || !request.body ? {} : { body: request.body }),
    redirect: "manual",
    signal: request.signal,
  })));
}

async function handleVaultEgress(
  request: Request,
  binding: Fetcher,
  subject: string | undefined,
): Promise<Response> {
  const vaultId = request.headers.get(VAULT_ID_HEADER);
  if (!vaultId || !VAULT_ID.test(vaultId)) return failure(403, "vault_reference_denied");
  if (!subject || !SUBJECT.test(subject)) return failure(403, "requires_login");

  const method = request.method.toUpperCase();
  if (!ORDINARY_METHODS.has(method)) return failure(403, "method_denied");
  let url: URL;
  try { url = validateUrl(new URL(request.url)); } catch { return failure(403, "destination_denied"); }
  if (encodedBytes(url.href) > MAX_VAULT_URL_BYTES || isProviderDestination(url)) {
    return failure(403, "destination_denied");
  }

  const headers = new Headers(request.headers);
  headers.delete(VAULT_ID_HEADER);
  if (forbiddenHeader(headers, { vaultMode: true })) {
    return failure(403, "credential_header_denied");
  }
  const projectedHeaders = boundedVaultHeaders(headers);
  if (!projectedHeaders) return failure(413, "request_too_large");

  let body: string | undefined;
  try {
    body = method === "GET" || method === "HEAD" || !request.body
      ? undefined
      : await readBoundedText(request, MAX_VAULT_BODY_BYTES);
  } catch {
    return failure(413, "request_too_large");
  }
  const envelope = JSON.stringify({
    vault_id: vaultId,
    url: url.href,
    method,
    headers: projectedHeaders,
    ...(body === undefined ? {} : { body }),
  });
  if (encodedBytes(envelope) > MAX_VAULT_ENVELOPE_BYTES) {
    return failure(413, "request_too_large");
  }
  return projectResponse(await binding.fetch(new Request(VAULT_EGRESS_URL, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      "x-nanocodex-subject": subject,
    },
    body: envelope,
    redirect: "manual",
    signal: request.signal,
  })));
}

function providerFor(url: URL): ProviderPolicy | undefined {
  if (url.protocol !== "https:" || url.port) return undefined;
  return PROVIDERS.get(url.hostname)?.find((provider) => provider.path(url.pathname));
}

function isProviderDestination(url: URL): boolean {
  return VAULT_PROVIDER_HOSTS.has(url.hostname.toLowerCase().replace(/\.$/, ""));
}

function canonicalProviderPath(provider: ProviderPolicy, pathname: string): boolean {
  return provider.connector === "github"
    || (!pathname.includes("\\") && !/%(?:2e|2f|5c|25)/i.test(pathname));
}


function validateUrl(url: URL): URL {
  if ((url.protocol !== "http:" && url.protocol !== "https:") || url.username || url.password
    || url.hash) {
    throw new Error("invalid public URL");
  }
  const hostname = url.hostname.toLowerCase().replace(/\.$/, "");
  if (!hostname || hostname === "localhost" || PRIVATE_HOST_SUFFIXES.some((suffix) => (
    hostname === suffix.slice(1) || hostname.endsWith(suffix)
  ))
    || isDeniedIpLiteral(hostname)) {
    throw new Error("private destination");
  }
  return url;
}

function isDeniedIpLiteral(hostname: string): boolean {
  const ipv4 = hostname.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (ipv4) {
    const octets = ipv4.slice(1).map(Number);
    if (octets.some((value) => value > 255)) return true;
    const [a, b] = octets;
    return a === 0 || a === 10 || a === 127 || a >= 224
      || (a === 100 && b >= 64 && b <= 127)
      || (a === 169 && b === 254) || (a === 172 && b >= 16 && b <= 31)
      || (a === 192 && (b === 0 || b === 168)) || (a === 198 && (b === 18 || b === 19));
  }
  if (!hostname.includes(":")) return false;
  const normalized = hostname.replace(/^\[/, "").replace(/\]$/, "").toLowerCase();
  return normalized === "::" || normalized === "::1" || normalized.startsWith("fc")
    || normalized.startsWith("fd") || normalized.startsWith("fe8")
    || normalized.startsWith("fe9") || normalized.startsWith("fea")
    || normalized.startsWith("feb") || normalized.startsWith("ff")
    || normalized.startsWith("::ffff:");
}

function forbiddenHeader(
  headers: Headers,
  options: Readonly<{ allowConnectorConnection?: boolean; vaultMode?: boolean }> = {},
): string | undefined {
  for (const [name, value] of headers) {
    const lower = name.toLowerCase();
    if (options.allowConnectorConnection && lower === CONNECTOR_CONNECTION_HEADER) continue;
    if (FORBIDDEN_HEADERS.has(lower)
      || (options.vaultMode && (VAULT_FORBIDDEN_HEADERS.has(lower)
        || lower.startsWith("x-nanocodex-")))
      || lower.startsWith("cf-") || lower.startsWith("forwarded")
      || lower.startsWith("sec-") || lower.startsWith("x-forwarded-")) return name;
    if (PRIVATE_HEADER.test(name)
      && (!options.vaultMode || !isVaultPlaceholderHeader(lower, value))) {
      return name;
    }
  }
  return undefined;
}

export function isValidVaultId(value: string | null | undefined): value is string {
  return typeof value === "string" && VAULT_ID.test(value);
}

export function isPrivateEgressHeader(name: string): boolean {
  return PRIVATE_HEADER.test(name);
}

export function isVaultPlaceholderHeader(name: string, value: string): boolean {
  const lower = name.toLowerCase();
  if (lower === "cookie" || lower === "proxy-authorization") return false;
  if (lower === "authorization") {
    return value === VAULT_BASIC_AUTHORIZATION || value === VAULT_BEARER_AUTHORIZATION
      || value === "Bearer {{NANOCODEX_VAULT_API_KEY}}";
  }
  return VAULT_PLACEHOLDERS.has(value);
}

function boundedVaultHeaders(headers: Headers): Record<string, string> | undefined {
  const entries = [...headers];
  if (entries.length > MAX_VAULT_HEADERS) return undefined;
  let aggregateBytes = 0;
  for (const [name, value] of entries) {
    const nameBytes = encodedBytes(name);
    const valueBytes = encodedBytes(value);
    if (nameBytes > MAX_VAULT_HEADER_NAME_BYTES || valueBytes > MAX_VAULT_HEADER_VALUE_BYTES) {
      return undefined;
    }
    aggregateBytes += nameBytes + valueBytes;
    if (aggregateBytes > MAX_VAULT_HEADER_BYTES) return undefined;
  }
  return Object.fromEntries(entries);
}

async function readBoundedText(request: Request, maxBytes: number): Promise<string> {
  if (!request.body) return "";
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > maxBytes) {
        await reader.cancel();
        throw new Error("request too large");
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const encoded = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    encoded.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(encoded);
}

function encodedBytes(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function projectResponse(response: Response): Response {
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers: projectedHeaders(response.headers),
  });
}

function projectedHeaders(source: Headers): Headers {
  const headers = new Headers();
  for (const [name, value] of source) {
    const lower = name.toLowerCase();
    if (!PRIVATE_HEADER.test(name) && !BLOCKED_RESPONSE_HEADERS.has(lower)) {
      headers.append(name, value);
    }
  }
  return headers;
}

function failure(status: number, error: string): Response {
  return Response.json({ error }, {
    status,
    headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" },
  });
}
