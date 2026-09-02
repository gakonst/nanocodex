const PROVIDER_PLACEHOLDER = "Bearer NANOCODEX_PROVIDER_CREDENTIAL";
const CONNECTOR_CONNECTION_HEADER = "x-nanocodex-connector-connection";
const CONNECTOR_CONNECTION = /^[A-Za-z0-9_-]{43}$/;
const SUBJECT = /^[A-Za-z0-9_-]{43,128}$/;
const MAX_REDIRECTS = 5;
const REDIRECTS = new Set([301, 302, 303, 307, 308]);
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
}>;

const PROVIDERS = new Map<string, readonly ProviderPolicy[]>([
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

const PRIVATE_HEADER = /(?:^|[-_])(?:auth(?:orization)?|cookie|credential|password|proxy|secret|token|api[-_]?key)(?:$|[-_]|\d)/i;
const FORBIDDEN_HEADERS = new Set([
  "connection", "host", "origin", "proxy-connection", "referer", "te", "trailer",
  "transfer-encoding", "upgrade", CONNECTOR_CONNECTION_HEADER, "x-nanocodex-subject",
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
  const method = request.method.toUpperCase();
  if (!ORDINARY_METHODS.has(method)) return failure(403, "method_denied");

  let url: URL;
  try { url = validateUrl(new URL(request.url)); } catch { return failure(403, "destination_denied"); }
  const provider = providerFor(url);
  const headerFailure = forbiddenHeader(request.headers, provider !== undefined);
  if (headerFailure) return failure(403, "credential_header_denied");
  if (!provider && PROVIDERS.has(url.hostname)) return failure(403, "destination_denied");
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

  const body = method === "GET" || method === "HEAD" || !request.body
    ? undefined
    : await request.arrayBuffer();
  return fetchPublic(url, request, body);
}

function providerFor(url: URL): ProviderPolicy | undefined {
  if (url.protocol !== "https:" || url.port) return undefined;
  return PROVIDERS.get(url.hostname)?.find((provider) => provider.path(url.pathname));
}

function canonicalProviderPath(provider: ProviderPolicy, pathname: string): boolean {
  return provider.connector === "github"
    || (!pathname.includes("\\") && !/%(?:2e|2f|5c|25)/i.test(pathname));
}

async function fetchPublic(
  initialUrl: URL,
  request: Request,
  originalBody: ArrayBuffer | undefined,
): Promise<Response> {
  let url = initialUrl;
  let method = request.method.toUpperCase();
  let body = originalBody;
  for (let redirects = 0; ; redirects += 1) {
    if (redirects > MAX_REDIRECTS) return failure(502, "too_many_redirects");
    const headers = new Headers(request.headers);
    if (method === "GET" || method === "HEAD") {
      headers.delete("content-length");
      headers.delete("content-type");
      body = undefined;
    }
    try {
      const response = await fetch(url, {
        method,
        headers,
        body,
        redirect: "manual",
        signal: request.signal,
      });
      if (!REDIRECTS.has(response.status)) return projectResponse(response);
      const location = response.headers.get("location");
      await response.body?.cancel();
      if (!location) return failure(502, "invalid_redirect");
      try { url = validateUrl(new URL(location, url)); } catch { return failure(502, "redirect_denied"); }
      if (providerFor(url) || PROVIDERS.has(url.hostname)) {
        return failure(502, "redirect_to_connector_denied");
      }
      if (response.status === 303
        || ((response.status === 301 || response.status === 302) && method === "POST")) {
        method = "GET";
        body = undefined;
      }
    } catch {
      return failure(request.signal.aborted ? 499 : 502, "upstream_unavailable");
    }
  }
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

function forbiddenHeader(headers: Headers, allowConnectorConnection = false): string | undefined {
  for (const [name] of headers) {
    const lower = name.toLowerCase();
    if (allowConnectorConnection && lower === CONNECTOR_CONNECTION_HEADER) continue;
    if (PRIVATE_HEADER.test(name) || FORBIDDEN_HEADERS.has(lower)
      || lower.startsWith("cf-") || lower.startsWith("forwarded")
      || lower.startsWith("sec-") || lower.startsWith("x-forwarded-")) return name;
  }
  return undefined;
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
