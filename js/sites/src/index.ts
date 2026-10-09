import {
  SITE_GRANT_PARAM,
  SITE_HOST_LABEL,
  SITE_SESSION_COOKIE,
  blobKey,
  newHostLabel,
  sha256Hex,
  hostKey,
  parseHostRecord,
  parseManifest,
  type SiteFile,
  type SiteHostRecord,
  type SiteManifest,
} from "./format";

export interface Env {
  SITES: R2Bucket;
  /**
   * Registrable zone whose first-level subdomains are site hosts, such as
   * `sites.example`. When empty, sites are served by path instead, as
   * `https://<worker host>/<label>/`.
   */
  SITES_DOMAIN: string;
}

/** Where a request's site lives: its link label and the URL prefix it is served under. */
type SiteTarget = Readonly<{
  label: string;
  /** `""` for host links, `/<label>` for path links. */
  base: string;
  /** Path inside the site; undefined for a path link without its trailing slash. */
  path: string | undefined;
}>;

// Generated pages may load common CDNs, but cannot send data anywhere except
// their own origin, post forms elsewhere, or embed plugins.
// `self` is 'self' for host links, or the site's own path prefix for path
// links, which share one origin.
const siteCsp = (self: string) => [
  `default-src ${self} data: blob:`,
  `script-src ${self} 'unsafe-inline' 'unsafe-eval' blob: https://cdn.jsdelivr.net https://unpkg.com https://cdnjs.cloudflare.com https://esm.sh`,
  `style-src ${self} 'unsafe-inline' https://fonts.googleapis.com https://cdn.jsdelivr.net https://unpkg.com https://cdnjs.cloudflare.com`,
  `font-src ${self} data: https://fonts.gstatic.com`,
  `img-src ${self} data: blob: https:`,
  `media-src ${self} data: blob:`,
  `connect-src ${self}`,
  `worker-src ${self} blob:`,
  `form-action ${self}`,
  `base-uri ${self}`,
  "object-src 'none'",
].join("; ");

// Public path links share an origin with every other site, including the
// visitor's own private views, so their pages run in an opaque origin: no
// cookies, no storage, and no same-origin reads. Private views are not
// sandboxed, because their session cookie must reach every subresource and
// only the owner's own content runs there.
const SANDBOX = "sandbox allow-scripts allow-forms allow-popups allow-popups-to-escape-sandbox allow-modals allow-downloads";

function sitePolicy(url: URL, target: SiteTarget, kind: SiteHostRecord["kind"]): Record<string, string> {
  if (!target.base) return { "content-security-policy": siteCsp("'self'") };
  const csp = siteCsp(`${url.origin}${target.base}/`);
  if (kind === "view") return { "content-security-policy": csp };
  // Sandboxed pages are cross-origin to their own files, so module scripts,
  // fonts, and fetches need CORS. Anyone holding a share link can read it already.
  return { "content-security-policy": `${SANDBOX}; ${csp}`, "access-control-allow-origin": "*" };
}

const POLICY_HEADERS = {
  "x-content-type-options": "nosniff",
  "referrer-policy": "no-referrer",
  "x-robots-tag": "noindex, nofollow",
  "permissions-policy": "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
};

// Manifests are content-addressed, so a cached copy can never go stale.
const manifests = new Map<string, SiteManifest>();
const MAX_CACHED_MANIFESTS = 256;

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const target = siteTarget(url, env.SITES_DOMAIN);
    if (!target) return unavailable(request);
    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response(null, { status: 405, headers: { ...POLICY_HEADERS, allow: "GET, HEAD" } });
    }
    // Redirect before reading the record so the response reveals nothing, and
    // so a view's path-scoped cookie applies to the request that follows.
    if (target.path === undefined) {
      return new Response(null, { status: 308, headers: { ...POLICY_HEADERS, location: `${target.base}/${url.search}` } });
    }
    // The host record is read on every request so revocation and expiry take
    // effect on the next request. Every response is revalidated for the same reason.
    const stored = await readHostRecord(env.SITES, target.label);
    const record = stored?.record;
    if (!stored || !record || (record.expires_at !== null && record.expires_at <= Date.now())) return unavailable(request);
    if (record.kind === "view") {
      const grant = url.searchParams.get(SITE_GRANT_PARAM);
      if (grant !== null) return exchangeGrant(env, url, target, stored, grant, request);
      if (!await hasSession(request, record)) return unavailable(request);
    }
    const manifest = await readManifest(env.SITES, record.manifest);
    if (!manifest) return unavailable(request);
    const resolved = resolve(manifest, target.path);
    if (resolved === undefined) return unavailable(request);
    if (typeof resolved === "string") {
      return new Response(null, { status: 308, headers: { ...POLICY_HEADERS, location: target.base + resolved + url.search } });
    }
    const etag = `"${resolved.sha256}"`;
    const headers = new Headers({
      ...POLICY_HEADERS,
      "content-type": resolved.type,
      ...sitePolicy(url, target, record.kind),
      "cache-control": record.kind === "view" ? "private, no-cache" : "no-cache",
      etag,
    });
    if (request.headers.get("if-none-match")?.split(",").some(tag => tag.trim().replace(/^W\//, "") === etag)) {
      return new Response(null, { status: 304, headers });
    }
    const key = blobKey(record.thread_id, resolved.sha256);
    if (request.method === "HEAD") {
      const object = await env.SITES.head(key);
      if (!object) return unavailable(request);
      headers.set("content-length", String(object.size));
      return new Response(null, { headers });
    }
    const object = await env.SITES.get(key);
    if (!object) return unavailable(request);
    headers.set("content-length", String(object.size));
    return new Response(object.body, { headers });
  },
} satisfies ExportedHandler<Env>;

function siteTarget(url: URL, domain: string): SiteTarget | undefined {
  if (!domain) {
    const match = /^\/([^/]+)(\/.*)?$/.exec(url.pathname);
    if (!match || !SITE_HOST_LABEL.test(match[1]!)) return undefined;
    return { label: match[1]!, base: `/${match[1]}`, path: match[2] };
  }
  const suffix = `.${domain.toLowerCase()}`;
  const host = url.hostname.toLowerCase();
  if (!host.endsWith(suffix)) return undefined;
  const label = host.slice(0, -suffix.length);
  return SITE_HOST_LABEL.test(label) ? { label, base: "", path: url.pathname } : undefined;
}

type StoredHost = Readonly<{ label: string; etag: string; record: SiteHostRecord | undefined }>;

async function readHostRecord(bucket: R2Bucket, label: string): Promise<StoredHost | undefined> {
  const object = await bucket.get(hostKey(label));
  if (!object) return undefined;
  let record: SiteHostRecord | undefined;
  try { record = parseHostRecord(await object.text()); } catch { record = undefined; }
  return { label, etag: object.etag, record };
}

/**
 * Exchanges a view's single-use grant for a session cookie bound to this
 * browser. The grant only ever reaches the owner through an authenticated
 * open request; the conditional write lets exactly one browser redeem it.
 */
async function exchangeGrant(env: Env, url: URL, target: SiteTarget, stored: StoredHost, grant: string, request: Request): Promise<Response> {
  const record = stored.record!;
  if (record.session !== undefined || !SITE_HOST_LABEL.test(grant) || await sha256Hex(grant) !== record.grant) return unavailable(request);
  const session = `${newHostLabel()}${newHostLabel()}`;
  const claimed = await env.SITES.put(hostKey(stored.label), JSON.stringify({ ...record, session: await sha256Hex(session) }), {
    onlyIf: { etagMatches: stored.etag },
    httpMetadata: { contentType: "application/json" },
  });
  if (!claimed) return unavailable(request);
  const clean = new URL(url);
  clean.searchParams.delete(SITE_GRANT_PARAM);
  const maxAge = record.expires_at === null ? undefined : Math.max(0, Math.floor((record.expires_at - Date.now()) / 1000));
  const cookie = [
    `${SITE_SESSION_COOKIE}=${session}`,
    `Path=${target.base}/`,
    "HttpOnly",
    "SameSite=Lax",
    ...(url.protocol === "https:" ? ["Secure"] : []),
    ...(maxAge === undefined ? [] : [`Max-Age=${maxAge}`]),
  ].join("; ");
  return new Response(null, {
    status: 303,
    headers: { ...POLICY_HEADERS, location: clean.pathname + clean.search, "set-cookie": cookie, "cache-control": "no-store" },
  });
}

async function hasSession(request: Request, record: SiteHostRecord): Promise<boolean> {
  if (record.session === undefined) return false;
  for (const part of request.headers.get("cookie")?.split(";") ?? []) {
    const [name, ...value] = part.trim().split("=");
    if (name === SITE_SESSION_COOKIE && await sha256Hex(value.join("=")) === record.session) return true;
  }
  return false;
}

async function readManifest(bucket: R2Bucket, key: string): Promise<SiteManifest | undefined> {
  const cached = manifests.get(key);
  if (cached) return cached;
  const object = await bucket.get(key);
  if (!object) return undefined;
  let manifest: SiteManifest;
  try { manifest = parseManifest(await object.text()); } catch { return undefined; }
  if (manifests.size >= MAX_CACHED_MANIFESTS) manifests.delete(manifests.keys().next().value!);
  manifests.set(key, manifest);
  return manifest;
}

/** Returns the file to serve, a redirect target for a directory, or undefined. */
function resolve(manifest: SiteManifest, pathname: string): SiteFile | string | undefined {
  let path: string;
  try { path = decodeURIComponent(pathname).replace(/^\/+/, ""); } catch { return undefined; }
  if (path === "") return manifest.files[manifest.entry];
  if (path.endsWith("/")) return manifest.files[`${path}index.html`];
  const file = manifest.files[path];
  if (file) return file;
  if (manifest.files[`${path}/index.html`]) return `/${path}/`;
  const name = path.slice(path.lastIndexOf("/") + 1);
  return manifest.spa && !name.includes(".") ? manifest.files[manifest.entry] : undefined;
}

/** Missing, revoked, and expired links are indistinguishable. */
function unavailable(request: Request): Response {
  const body = "<!doctype html><meta charset=utf-8><meta name=viewport content=\"width=device-width\"><title>Link unavailable</title>"
    + "<style>body{font:16px/1.5 system-ui,sans-serif;max-width:32rem;margin:20vh auto;padding:0 1rem;color:#262626}</style>"
    + "<h1>This link isn't available</h1><p>It may have been turned off by the person who shared it, or it may have expired.</p>";
  return new Response(request.method === "HEAD" ? null : body, {
    status: 404,
    headers: {
      ...POLICY_HEADERS,
      "content-type": "text/html; charset=utf-8",
      "content-security-policy": "default-src 'none'; style-src 'unsafe-inline'",
      "cache-control": "no-store",
    },
  });
}
