/**
 * The R2 contract between the managed Session that publishes sites and the
 * Sites Worker that serves them. Every object is immutable except host
 * records, which the owning Session creates and deletes to grant and revoke a
 * hostname.
 */

/** 130-bit random DNS label; the hostname itself is the capability. */
export const SITE_HOST_LABEL = /^[a-z2-7]{26}$/;
export const SITE_ID = /^[a-z0-9][a-z0-9-]{0,62}$/;
export const THREAD_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const SHA256 = /^[0-9a-f]{64}$/;

export type SiteFile = Readonly<{ sha256: string; size: number; type: string }>;

export type SiteManifest = Readonly<{
  format: 1;
  site_id: string;
  title: string;
  /** Served for `/` and, with `spa`, for unknown extensionless paths. */
  entry: string;
  spa: boolean;
  /** Relative paths without a leading slash, such as `assets/app.js`. */
  files: Readonly<Record<string, SiteFile>>;
}>;

export type SiteHostKind = "share" | "view";

export type SiteHostRecord = Readonly<{
  format: 1;
  kind: SiteHostKind;
  thread_id: string;
  site_id: string;
  version: number;
  manifest: string;
  /** Unix milliseconds, or null for a share without an expiry. */
  expires_at: number | null;
}>;

export const hostKey = (label: string): string => {
  if (!SITE_HOST_LABEL.test(label)) throw new TypeError("invalid site host label");
  return `hosts/${label}.json`;
};

export const threadPrefix = (threadId: string): string => {
  if (!THREAD_ID.test(threadId)) throw new TypeError("invalid site thread id");
  return `threads/${threadId}/`;
};

export const blobKey = (threadId: string, sha256: string): string => {
  if (!SHA256.test(sha256)) throw new TypeError("invalid site blob digest");
  return `${threadPrefix(threadId)}blobs/${sha256}`;
};

export const manifestKey = (threadId: string, sha256: string): string => {
  if (!SHA256.test(sha256)) throw new TypeError("invalid site manifest digest");
  return `${threadPrefix(threadId)}manifests/${sha256}.json`;
};

export function newHostLabel(): string {
  const alphabet = "abcdefghijklmnopqrstuvwxyz234567";
  const bytes = crypto.getRandomValues(new Uint8Array(26));
  // 32 divides 256, so taking the low five bits keeps every symbol uniform.
  return Array.from(bytes, byte => alphabet[byte & 31]).join("");
}

/** Accepts only canonical relative paths; a manifest can never name `..` or an absolute path. */
export function isSitePath(path: string): boolean {
  return path.length > 0 && path.length <= 1024 && !/[\u0000-\u001f\u007f\\]/.test(path)
    && path.split("/").every(segment => segment !== "" && segment !== "." && segment !== "..");
}

const TYPES: Readonly<Record<string, string>> = {
  html: "text/html; charset=utf-8", htm: "text/html; charset=utf-8",
  css: "text/css; charset=utf-8", js: "text/javascript; charset=utf-8", mjs: "text/javascript; charset=utf-8",
  json: "application/json; charset=utf-8", map: "application/json; charset=utf-8",
  webmanifest: "application/manifest+json; charset=utf-8",
  txt: "text/plain; charset=utf-8", md: "text/markdown; charset=utf-8", csv: "text/csv; charset=utf-8",
  xml: "application/xml; charset=utf-8", svg: "image/svg+xml",
  png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp",
  avif: "image/avif", ico: "image/x-icon",
  pdf: "application/pdf", wasm: "application/wasm",
  woff: "font/woff", woff2: "font/woff2", ttf: "font/ttf", otf: "font/otf",
  mp4: "video/mp4", webm: "video/webm", mp3: "audio/mpeg", wav: "audio/wav", ogg: "audio/ogg",
};

/** The manifest fixes each file's type at publish time; browsers never sniff it. */
export function siteContentType(path: string): string {
  const name = path.slice(path.lastIndexOf("/") + 1);
  const dot = name.lastIndexOf(".");
  return (dot > 0 && TYPES[name.slice(dot + 1).toLowerCase()]) || "application/octet-stream";
}

export function parseManifest(encoded: string): SiteManifest {
  const value: unknown = JSON.parse(encoded);
  const record = exact(value, ["format", "site_id", "title", "entry", "spa", "files"]);
  if (record.format !== 1 || typeof record.site_id !== "string" || !SITE_ID.test(record.site_id)
    || typeof record.title !== "string" || typeof record.entry !== "string" || typeof record.spa !== "boolean"
    || !record.files || typeof record.files !== "object" || Array.isArray(record.files)) {
    throw new TypeError("invalid site manifest");
  }
  const files: Record<string, SiteFile> = {};
  for (const [path, file] of Object.entries(record.files as Record<string, unknown>)) {
    const entry = exact(file, ["sha256", "size", "type"]);
    if (!isSitePath(path) || typeof entry.sha256 !== "string" || !SHA256.test(entry.sha256)
      || !Number.isSafeInteger(entry.size) || (entry.size as number) < 0 || typeof entry.type !== "string") {
      throw new TypeError("invalid site manifest file");
    }
    files[path] = { sha256: entry.sha256, size: entry.size as number, type: entry.type };
  }
  if (!Object.hasOwn(files, record.entry)) throw new TypeError("site manifest entry is missing");
  return { format: 1, site_id: record.site_id, title: record.title, entry: record.entry, spa: record.spa, files };
}

export function parseHostRecord(encoded: string): SiteHostRecord {
  const record = exact(JSON.parse(encoded), ["format", "kind", "thread_id", "site_id", "version", "manifest", "expires_at"]);
  if (record.format !== 1 || (record.kind !== "share" && record.kind !== "view")
    || typeof record.thread_id !== "string" || !THREAD_ID.test(record.thread_id)
    || typeof record.site_id !== "string" || !SITE_ID.test(record.site_id)
    || !Number.isSafeInteger(record.version) || (record.version as number) < 1
    || typeof record.manifest !== "string" || !record.manifest.startsWith(`${threadPrefix(record.thread_id)}manifests/`)
    || (record.expires_at !== null && !Number.isSafeInteger(record.expires_at))) {
    throw new TypeError("invalid site host record");
  }
  return {
    format: 1, kind: record.kind, thread_id: record.thread_id, site_id: record.site_id,
    version: record.version as number, manifest: record.manifest, expires_at: record.expires_at as number | null,
  };
}

function exact(value: unknown, keys: readonly string[]): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError("expected an object");
  const record = value as Record<string, unknown>;
  if (Object.keys(record).some(key => !keys.includes(key)) || keys.some(key => !Object.hasOwn(record, key))) {
    throw new TypeError("unexpected site record shape");
  }
  return record;
}
