import {
  SITE_HOST_LABEL,
  blobKey,
  hostKey,
  parseHostRecord,
  parseManifest,
  type SiteFile,
  type SiteManifest,
} from "./format";

export interface Env {
  SITES: R2Bucket;
  /** Registrable zone whose first-level subdomains are site hosts, such as `sites.example`. */
  SITES_DOMAIN: string;
}

// Generated pages may load common CDNs, but cannot send data anywhere except
// their own origin, post forms elsewhere, or embed plugins.
const SITE_CSP = [
  "default-src 'self' data: blob:",
  "script-src 'self' 'unsafe-inline' 'unsafe-eval' blob: https://cdn.jsdelivr.net https://unpkg.com https://cdnjs.cloudflare.com https://esm.sh",
  "style-src 'self' 'unsafe-inline' https://fonts.googleapis.com https://cdn.jsdelivr.net https://unpkg.com https://cdnjs.cloudflare.com",
  "font-src 'self' data: https://fonts.gstatic.com",
  "img-src 'self' data: blob: https:",
  "media-src 'self' data: blob:",
  "connect-src 'self'",
  "worker-src 'self' blob:",
  "form-action 'self'",
  "base-uri 'self'",
  "object-src 'none'",
].join("; ");

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
    const label = hostLabel(url.hostname, env.SITES_DOMAIN);
    if (!label) return unavailable(request);
    if (request.method !== "GET" && request.method !== "HEAD") {
      return new Response(null, { status: 405, headers: { ...POLICY_HEADERS, allow: "GET, HEAD" } });
    }
    // The host record is read on every request so revocation and expiry take
    // effect on the next request. Every response is revalidated for the same reason.
    const record = await readHostRecord(env.SITES, label);
    if (!record || (record.expires_at !== null && record.expires_at <= Date.now())) return unavailable(request);
    const manifest = await readManifest(env.SITES, record.manifest);
    if (!manifest) return unavailable(request);
    const resolved = resolve(manifest, url.pathname);
    if (resolved === undefined) return unavailable(request);
    if (typeof resolved === "string") {
      return new Response(null, { status: 308, headers: { ...POLICY_HEADERS, location: resolved + url.search } });
    }
    const etag = `"${resolved.sha256}"`;
    const headers = new Headers({
      ...POLICY_HEADERS,
      "content-type": resolved.type,
      "content-security-policy": SITE_CSP,
      "cache-control": "no-cache",
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

function hostLabel(hostname: string, domain: string): string | undefined {
  const suffix = `.${domain.toLowerCase()}`;
  const host = hostname.toLowerCase();
  if (!domain || !host.endsWith(suffix)) return undefined;
  const label = host.slice(0, -suffix.length);
  return SITE_HOST_LABEL.test(label) ? label : undefined;
}

async function readHostRecord(bucket: R2Bucket, label: string) {
  const object = await bucket.get(hostKey(label));
  if (!object) return undefined;
  try { return parseHostRecord(await object.text()); } catch { return undefined; }
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
