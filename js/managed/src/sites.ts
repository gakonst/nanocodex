import {
  SITE_GRANT_PARAM,
  SITE_ID,
  blobKey,
  hostKey,
  isSitePath,
  manifestKey,
  newHostLabel,
  sha256Hex,
  siteContentType,
  threadPrefix,
  type SiteFile,
  type SiteHostKind,
  type SiteHostRecord,
} from "@nanocodex/sites/format";
import type { Workspace } from "nanocodex-tools";

/**
 * Static sites published from a thread's files. Each version is an immutable,
 * content-addressed snapshot in the sites bucket; the thread's Durable Object
 * is the only authority that creates or deletes the host records that make a
 * version reachable.
 */

const MAX_FILES = 2_000;
const MAX_FILE_BYTES = 25 * 1024 * 1024;
const MAX_TOTAL_BYTES = 50 * 1024 * 1024;
const MAX_SITES = 100;
const MAX_ACTIVE_SHARES = 50;
const MAX_SHARE_LIFETIME_MS = 366 * 24 * 60 * 60 * 1_000;
export const SITE_VIEW_TTL_MS = 60 * 60 * 1_000;
const READ_CONCURRENCY = 4;
const SHARE_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

// Directories and files that commonly hold credentials or dependencies. They
// are skipped, never uploaded, and reported only as a count.
const EXCLUDED_DIRECTORIES = new Set([".git", "node_modules", ".ssh", ".aws", ".gnupg", ".nanocodex"]);
const EXCLUDED_FILE = /^(?:\.env(?:\..*)?|\.npmrc|\.netrc|\.pypirc|\.git-credentials|id_(?:rsa|dsa|ecdsa|ed25519)(?:\.pub)?|.+\.(?:pem|key|p12|pfx|jks|keystore))$/i;

export class SiteError extends Error {
  constructor(readonly status: number, readonly code: string, message: string) { super(message); }
}

export type SiteSourceListing = Readonly<{
  files: readonly Readonly<{ name: string; size: number }>[];
  directories: readonly string[];
}>;

/** A directory or single file the thread can read without running model code. */
export type SiteSource =
  | Readonly<{ kind: "file"; name: string; size: number; read(): Promise<Uint8Array> }>
  | Readonly<{ kind: "directory"; list(relative: string): Promise<SiteSourceListing>; read(relative: string): Promise<Uint8Array> }>;

export type PublishInput = Readonly<{ path: string; id?: string; title?: string; entry?: string; spa?: boolean }>;

export type PublishedSite = Readonly<{
  type: "nanocodex.site";
  site_id: string;
  title: string;
  version: number;
  entry: string;
  files: number;
  bytes: number;
  excluded: number;
  created: boolean;
}>;

type SiteRow = { id: string; title: string; latest_version: number; created_at: number; updated_at: number };
type VersionRow = { site_id: string; version: number; manifest_key: string; entry: string; files: number; bytes: number; source: string; created_at: number };
type ShareRow = { id: string; site_id: string; host: string; version: number; created_at: number; expires_at: number | null; revoked_at: number | null };

export class ThreadSites {
  constructor(
    private readonly storage: DurableObjectStorage,
    private readonly threadId: string,
    private readonly bucket: R2Bucket | undefined,
    private readonly originPattern: string | undefined,
  ) {
    storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS managed_sites (
        id TEXT PRIMARY KEY, title TEXT NOT NULL, latest_version INTEGER NOT NULL,
        created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS managed_site_versions (
        site_id TEXT NOT NULL, version INTEGER NOT NULL, manifest_key TEXT NOT NULL, entry TEXT NOT NULL,
        files INTEGER NOT NULL, bytes INTEGER NOT NULL, source TEXT NOT NULL, created_at INTEGER NOT NULL,
        PRIMARY KEY (site_id, version)
      );
      CREATE TABLE IF NOT EXISTS managed_site_hosts (
        host TEXT PRIMARY KEY, kind TEXT NOT NULL CHECK(kind IN ('share','view')), id TEXT NOT NULL UNIQUE,
        site_id TEXT NOT NULL, version INTEGER NOT NULL, created_at INTEGER NOT NULL,
        expires_at INTEGER, revoked_at INTEGER
      );
    `);
  }

  list() {
    const sites = this.storage.sql.exec<SiteRow>(
      "SELECT id,title,latest_version,created_at,updated_at FROM managed_sites ORDER BY updated_at DESC,id").toArray();
    return sites.map(site => ({
      ...site,
      versions: this.storage.sql.exec<Omit<VersionRow, "site_id" | "manifest_key">>(
        "SELECT version,entry,files,bytes,source,created_at FROM managed_site_versions WHERE site_id=? ORDER BY version DESC",
        site.id).toArray(),
      shares: this.listShares(site.id),
    }));
  }

  async publish(source: SiteSource, input: PublishInput, signal?: AbortSignal): Promise<PublishedSite> {
    const bucket = this.#bucket();
    const id = input.id ?? slug(input.title ?? baseName(input.path));
    if (!SITE_ID.test(id)) throw new SiteError(400, "invalid_site_id", "Site IDs use 1-63 lowercase letters, digits, and hyphens");
    const existing = this.#site(id);
    if (!existing && this.storage.sql.exec<{ count: number }>("SELECT COUNT(*) AS count FROM managed_sites").one().count >= MAX_SITES) {
      throw new SiteError(429, "site_limit", `A thread can publish at most ${MAX_SITES} sites`);
    }
    const title = (input.title ?? existing?.title ?? id).trim().slice(0, 200) || id;
    const { files, excluded } = await collect(source, signal);
    const entry = input.entry ?? (files.has("index.html") ? "index.html" : files.size === 1 ? [...files.keys()][0]! : undefined);
    if (entry === undefined) throw new SiteError(422, "site_entry_required", "The site has no index.html; pass entry to choose the page served at /");
    if (!files.has(entry)) throw new SiteError(422, "site_entry_missing", `The entry ${entry} is not among the published files`);

    const manifestFiles: Record<string, SiteFile> = {};
    const written = new Set<string>();
    let bytes = 0;
    await forEachConcurrent([...files.keys()].sort(), READ_CONCURRENCY, async path => {
      signal?.throwIfAborted();
      const contents = source.kind === "file" ? await source.read() : await source.read(path);
      bytes += contents.byteLength;
      if (contents.byteLength > MAX_FILE_BYTES || bytes > MAX_TOTAL_BYTES) throw new SiteError(413, "site_too_large", sizeMessage());
      const sha256 = await digest(contents);
      const key = blobKey(this.threadId, sha256);
      if (!written.has(sha256)) {
        written.add(sha256);
        if (!await bucket.head(key)) await bucket.put(key, contents, { sha256 });
      }
      manifestFiles[path] = { sha256, size: contents.byteLength, type: siteContentType(path) };
    });
    const manifest = JSON.stringify({ format: 1, site_id: id, title, entry, spa: input.spa ?? false,
      files: Object.fromEntries(Object.keys(manifestFiles).sort().map(path => [path, manifestFiles[path]])) });
    const encoded = new TextEncoder().encode(manifest);
    const key = manifestKey(this.threadId, await digest(encoded));
    if (!await bucket.head(key)) await bucket.put(key, encoded, { httpMetadata: { contentType: "application/json" } });

    // Version assignment is one synchronous transaction; replaying an identical
    // publish returns the version it already created instead of a duplicate.
    return this.storage.transactionSync(() => {
      const now = Date.now();
      const current = this.#site(id);
      const latest = current && this.storage.sql.exec<VersionRow>(
        "SELECT * FROM managed_site_versions WHERE site_id=? AND version=?", id, current.latest_version).toArray()[0];
      const result = { type: "nanocodex.site" as const, site_id: id, title, entry, files: files.size, bytes, excluded };
      if (latest?.manifest_key === key) return { ...result, version: latest.version, created: false };
      const version = (current?.latest_version ?? 0) + 1;
      this.storage.sql.exec("INSERT INTO managed_site_versions(site_id,version,manifest_key,entry,files,bytes,source,created_at) VALUES(?,?,?,?,?,?,?,?)",
        id, version, key, entry, files.size, bytes, input.path, now);
      if (current) this.storage.sql.exec("UPDATE managed_sites SET title=?,latest_version=?,updated_at=? WHERE id=?", title, version, now, id);
      else this.storage.sql.exec("INSERT INTO managed_sites(id,title,latest_version,created_at,updated_at) VALUES(?,?,?,?,?)", id, title, version, now, now);
      return { ...result, version, created: true };
    });
  }

  /**
   * A private, short-lived host for the owner to look at one version. The URL
   * carries a single-use grant that the first browser to load it exchanges for
   * a session cookie; nobody else can open the view, even with its address.
   */
  async open(siteId: string, version?: number) {
    this.#origin();
    const target = this.#version(siteId, version);
    await this.#sweepExpiredViews();
    const expiresAt = Date.now() + SITE_VIEW_TTL_MS;
    const grant = newHostLabel();
    const host = await this.#grant("view", target, expiresAt, await sha256Hex(grant));
    return { site_id: siteId, version: target.version, url: `${this.#url(host.host)}?${SITE_GRANT_PARAM}=${grant}`, expires_at: expiresAt };
  }

  listShares(siteId: string) {
    return this.storage.sql.exec<ShareRow>(
      "SELECT id,site_id,host,version,created_at,expires_at FROM managed_site_hosts WHERE kind='share' AND site_id=? AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at>?) ORDER BY created_at,id",
      siteId, Date.now()).toArray().map(({ host, ...share }) => ({ ...share, url: this.#url(host) }));
  }

  async createShare(siteId: string, options: { version?: number; expires_at?: number | null }) {
    this.#origin();
    const target = this.#version(siteId, options.version);
    const expiresAt = options.expires_at ?? null;
    if (expiresAt !== null && (!Number.isSafeInteger(expiresAt) || expiresAt <= Date.now() || expiresAt > Date.now() + MAX_SHARE_LIFETIME_MS)) {
      throw new SiteError(400, "invalid_expiry", "expires_at must be a future Unix millisecond time within a year");
    }
    if (this.storage.sql.exec<{ count: number }>(
      "SELECT COUNT(*) AS count FROM managed_site_hosts WHERE kind='share' AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at>?)", Date.now()).one().count >= MAX_ACTIVE_SHARES) {
      throw new SiteError(429, "site_share_limit", "Too many active site links; revoke one first");
    }
    const share = await this.#grant("share", target, expiresAt);
    return { id: share.id, site_id: siteId, version: target.version, url: this.#url(share.host), created_at: share.created_at, expires_at: expiresAt };
  }

  async revokeShare(siteId: string, shareId: string): Promise<boolean> {
    if (!SHARE_ID.test(shareId)) return false;
    const share = this.storage.sql.exec<ShareRow>(
      "SELECT * FROM managed_site_hosts WHERE kind='share' AND id=? AND site_id=? AND revoked_at IS NULL", shareId, siteId).toArray()[0];
    if (!share) return false;
    // Delete the public record first: a failure leaves the link listed and active, never hidden but live.
    await this.#bucket().delete(hostKey(share.host));
    this.storage.sql.exec("UPDATE managed_site_hosts SET revoked_at=? WHERE host=?", Date.now(), share.host);
    return true;
  }

  /** Removes every host record and object for this thread, then its rows. */
  async deleteAll(): Promise<void> {
    if (!this.bucket) return;
    const hosts = this.storage.sql.exec<{ host: string }>("SELECT host FROM managed_site_hosts").toArray().map(row => hostKey(row.host));
    for (let index = 0; index < hosts.length; index += 1_000) await this.bucket.delete(hosts.slice(index, index + 1_000));
    let cursor: string | undefined;
    do {
      const page = await this.bucket.list({ prefix: threadPrefix(this.threadId), cursor });
      if (page.objects.length) await this.bucket.delete(page.objects.map(object => object.key));
      cursor = page.truncated ? page.cursor : undefined;
    } while (cursor);
    this.clear();
  }

  clear(): void {
    this.storage.sql.exec("DELETE FROM managed_site_hosts");
    this.storage.sql.exec("DELETE FROM managed_site_versions");
    this.storage.sql.exec("DELETE FROM managed_sites");
  }

  async #grant(kind: SiteHostKind, target: VersionRow, expiresAt: number | null, grant?: string) {
    const row = { host: newHostLabel(), id: crypto.randomUUID(), created_at: Date.now() };
    // Record the grant before it becomes reachable, so a lost write can only
    // leave an unreachable row that revocation and deletion still clean up.
    this.storage.sql.exec("INSERT INTO managed_site_hosts(host,kind,id,site_id,version,created_at,expires_at) VALUES(?,?,?,?,?,?,?)",
      row.host, kind, row.id, target.site_id, target.version, row.created_at, expiresAt);
    const record: SiteHostRecord = { format: 1, kind, thread_id: this.threadId, site_id: target.site_id,
      version: target.version, manifest: target.manifest_key, expires_at: expiresAt, ...(grant ? { grant } : {}) };
    try {
      await this.#bucket().put(hostKey(row.host), JSON.stringify(record), { httpMetadata: { contentType: "application/json" } });
    } catch (error) {
      this.storage.sql.exec("DELETE FROM managed_site_hosts WHERE host=?", row.host);
      throw error;
    }
    return row;
  }

  async #sweepExpiredViews(): Promise<void> {
    const expired = this.storage.sql.exec<{ host: string }>(
      "SELECT host FROM managed_site_hosts WHERE kind='view' AND expires_at<=? LIMIT 100", Date.now()).toArray();
    if (!expired.length) return;
    await this.#bucket().delete(expired.map(row => hostKey(row.host)));
    for (const row of expired) this.storage.sql.exec("DELETE FROM managed_site_hosts WHERE host=?", row.host);
  }

  #site(id: string): SiteRow | undefined {
    return this.storage.sql.exec<SiteRow>("SELECT * FROM managed_sites WHERE id=?", id).toArray()[0];
  }

  #version(siteId: string, version?: number): VersionRow {
    if (version !== undefined && (!Number.isSafeInteger(version) || version < 1)) throw new SiteError(400, "invalid_version", "version must be a positive integer");
    const site = SITE_ID.test(siteId) ? this.#site(siteId) : undefined;
    const row = site && this.storage.sql.exec<VersionRow>(
      "SELECT * FROM managed_site_versions WHERE site_id=? AND version=?", siteId, version ?? site.latest_version).toArray()[0];
    if (!row) throw new SiteError(404, "not_found", "Site version not found");
    return row;
  }

  #bucket(): R2Bucket {
    if (!this.bucket) throw new SiteError(503, "sites_unavailable", "Site publishing is not configured");
    return this.bucket;
  }

  #origin(): string {
    // Host links (`https://*.zone`) or path links on one host (`https://host/*`).
    if (!this.originPattern || !/^https?:\/\/(?:\*\.[a-z0-9.-]+(?::\d{1,5})?|[a-z0-9.-]+(?::\d{1,5})?\/\*)$/.test(this.originPattern)) {
      throw new SiteError(503, "sites_unavailable", "Site links are not configured");
    }
    return this.originPattern;
  }

  #url(host: string): string {
    return `${this.#origin().replace("*", host)}/`;
  }
}

/** Owner routes: /sites, /sites/:id/open, /sites/:id/shares, /sites/:id/shares/:share. */
export async function handleThreadSitesRequest(
  request: Request,
  url: URL,
  sites: ThreadSites,
  resolveSource: (path: string) => Promise<SiteSource>,
): Promise<Response> {
  const headers = { "cache-control": "no-store" };
  const parts = url.pathname.split("/").slice(2);
  try {
    if (url.search) throw new SiteError(400, "invalid_request", "Unexpected query parameters");
    if (parts.length === 0) {
      if (request.method === "GET") return Response.json({ data: sites.list() }, { headers });
      if (request.method !== "POST") throw new SiteError(405, "method_not_allowed", "Use GET or POST");
      const body = await jsonBody(request, ["path", "id", "title", "entry", "spa"]);
      if (typeof body.path !== "string" || !body.path.startsWith("/")
        || (body.id !== undefined && typeof body.id !== "string") || (body.title !== undefined && typeof body.title !== "string")
        || (body.entry !== undefined && (typeof body.entry !== "string" || !isSitePath(body.entry)))
        || (body.spa !== undefined && typeof body.spa !== "boolean")) {
        throw new SiteError(400, "invalid_request", "Expected { path, id?, title?, entry?, spa? }");
      }
      const input = body as PublishInput;
      const published = await sites.publish(await resolveSource(input.path), input, request.signal);
      return Response.json(published, { status: published.created ? 201 : 200, headers });
    }
    const [siteId, action, shareId, ...extra] = parts;
    if (!siteId || !SITE_ID.test(siteId) || extra.length) throw new SiteError(404, "not_found", "Not found");
    if (action === "open" && shareId === undefined) {
      if (request.method !== "POST") throw new SiteError(405, "method_not_allowed", "Use POST");
      const body = await jsonBody(request, ["version"], true);
      return Response.json(await sites.open(siteId, body.version as number | undefined), { headers });
    }
    if (action !== "shares") throw new SiteError(404, "not_found", "Not found");
    if (shareId !== undefined) {
      if (request.method !== "DELETE") throw new SiteError(405, "method_not_allowed", "Use DELETE");
      if (!await sites.revokeShare(siteId, shareId)) throw new SiteError(404, "not_found", "Site link not found");
      return new Response(null, { status: 204, headers });
    }
    if (request.method === "GET") return Response.json({ data: sites.listShares(siteId) }, { headers });
    if (request.method !== "POST") throw new SiteError(405, "method_not_allowed", "Use GET or POST");
    const body = await jsonBody(request, ["version", "expires_at"], true);
    if ((body.expires_at !== undefined && body.expires_at !== null && typeof body.expires_at !== "number")) {
      throw new SiteError(400, "invalid_request", "expires_at must be a number or null");
    }
    return Response.json(await sites.createShare(siteId, body as { version?: number; expires_at?: number | null }), { status: 201, headers });
  } catch (error) {
    if (error instanceof SiteError) return Response.json({ error: error.code, message: error.message }, { status: error.status, headers });
    throw error;
  }
}

/** Reads `/brain` through the thread's brain workspace, including small files kept in SQLite. */
export function workspaceSiteSource(workspace: Pick<Workspace, "root" | "list" | "readFile">, path: string): Promise<SiteSource> {
  return describe(path, {
    async stat() {
      if (path === workspace.root) return { kind: "directory" };
      const parent = path.slice(0, path.lastIndexOf("/")) || "/";
      const entries = await workspace.list(parent).catch(missing);
      const entry = entries?.find(candidate => candidate.path === path);
      return entry?.kind === "directory" ? { kind: "directory" } : entry ? { kind: "file", size: entry.size ?? 0 } : undefined;
    },
    async list(relative) {
      const directory = relative ? `${path}/${relative}` : path;
      const entries = await workspace.list(directory);
      return {
        files: entries.filter(entry => entry.kind === "file").map(entry => ({ name: entry.path.slice(directory.length + 1), size: entry.size ?? 0 })),
        directories: entries.filter(entry => entry.kind === "directory").map(entry => entry.path.slice(directory.length + 1)),
      };
    },
    read: relative => workspace.readFile(relative ? `${path}/${relative}` : path),
  });
}

/** Reads a Cloudflare sandbox workspace straight from its R2 prefix, without waking the container. */
export function bucketSiteSource(bucket: R2Bucket, prefix: string, relative: string, path: string): Promise<SiteSource> {
  const base = relative ? `${prefix}${relative}` : prefix.replace(/\/$/, "");
  return describe(path, {
    async stat() {
      if (relative) {
        const object = await bucket.head(base);
        if (object) return { kind: "file", size: object.size };
      }
      const page = await bucket.list({ prefix: `${base}/`, limit: 1 });
      return page.objects.length || page.delimitedPrefixes.length ? { kind: "directory" } : undefined;
    },
    async list(child) {
      const directory = `${child ? `${base}/${child}` : base}/`;
      const files: { name: string; size: number }[] = [];
      const directories: string[] = [];
      let cursor: string | undefined;
      do {
        const page = await bucket.list({ prefix: directory, delimiter: "/", cursor });
        for (const object of page.objects) {
          // s3fs keeps empty directories as zero-byte `name/` markers.
          if (!object.key.endsWith("/")) files.push({ name: object.key.slice(directory.length), size: object.size });
        }
        for (const nested of page.delimitedPrefixes) directories.push(nested.slice(directory.length, -1));
        cursor = page.truncated ? page.cursor : undefined;
      } while (cursor);
      return { files, directories };
    },
    async read(child) {
      const object = await bucket.get(child ? `${base}/${child}` : base);
      if (!object) throw new SiteError(409, "site_source_changed", "A file changed while publishing; try again");
      return new Uint8Array(await object.arrayBuffer());
    },
  });
}

async function describe(path: string, source: {
  stat(): Promise<{ kind: "directory" } | { kind: "file"; size: number } | undefined>;
  list(relative: string): Promise<SiteSourceListing>;
  read(relative: string): Promise<Uint8Array>;
}): Promise<SiteSource> {
  const stat = await source.stat();
  if (!stat) throw new SiteError(404, "site_source_not_found", `${path} does not exist`);
  if (stat.kind === "directory") return { kind: "directory", list: source.list, read: source.read };
  const name = baseName(path);
  if (excludedPath(name)) throw new SiteError(422, "site_source_excluded", `${name} looks like a secret and can't be published`);
  return { kind: "file", name, size: stat.size, read: () => source.read("") };
}

async function collect(source: SiteSource, signal?: AbortSignal) {
  const files = new Map<string, number>();
  let excluded = 0;
  let bytes = 0;
  const add = (path: string, size: number) => {
    if (!isSitePath(path)) { excluded += 1; return; }
    files.set(path, size);
    bytes += size;
    if (files.size > MAX_FILES) throw new SiteError(413, "site_too_large", `A site can contain at most ${MAX_FILES} files`);
    if (size > MAX_FILE_BYTES || bytes > MAX_TOTAL_BYTES) throw new SiteError(413, "site_too_large", sizeMessage());
  };
  if (source.kind === "file") {
    add(source.name, source.size);
    return { files, excluded };
  }
  const pending = [""];
  while (pending.length) {
    signal?.throwIfAborted();
    const directory = pending.pop()!;
    const listing = await source.list(directory);
    for (const file of listing.files) {
      const path = directory ? `${directory}/${file.name}` : file.name;
      if (excludedPath(file.name)) excluded += 1;
      else add(path, file.size);
    }
    for (const name of listing.directories) {
      if (EXCLUDED_DIRECTORIES.has(name)) excluded += 1;
      else pending.push(directory ? `${directory}/${name}` : name);
    }
  }
  if (files.size === 0) throw new SiteError(422, "site_empty", "There are no publishable files at that path");
  return { files, excluded };
}

function excludedPath(name: string): boolean {
  return EXCLUDED_DIRECTORIES.has(name) || EXCLUDED_FILE.test(name);
}

async function jsonBody(request: Request, keys: readonly string[], optional = false): Promise<Record<string, unknown>> {
  const text = await request.text();
  if (optional && text === "") return {};
  if (text.length > 4_096) throw new SiteError(400, "invalid_request", "Request body is too large");
  let value: unknown;
  try { value = JSON.parse(text); } catch { throw new SiteError(400, "invalid_request", "Expected a JSON object"); }
  if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).some(key => !keys.includes(key))) {
    throw new SiteError(400, "invalid_request", `Expected only ${keys.join(", ")}`);
  }
  return value as Record<string, unknown>;
}

async function digest(bytes: Uint8Array): Promise<string> {
  const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return Array.from(hash, byte => byte.toString(16).padStart(2, "0")).join("");
}

async function forEachConcurrent<T>(items: readonly T[], limit: number, run: (item: T) => Promise<void>): Promise<void> {
  let next = 0;
  await Promise.all(Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) await run(items[next++]!);
  }));
}

function slug(value: string): string {
  return value.toLowerCase().replace(/\.[a-z0-9]+$/, "").replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 63).replace(/-+$/, "") || "site";
}

function baseName(path: string): string {
  return path.replace(/\/+$/, "").slice(path.replace(/\/+$/, "").lastIndexOf("/") + 1) || "site";
}

function missing(error: unknown): undefined {
  if ((error as { code?: unknown })?.code === "ENOENT" || (error as { code?: unknown })?.code === "ENOTDIR") return undefined;
  throw error;
}

function sizeMessage(): string {
  return `A site can be at most ${MAX_TOTAL_BYTES / 1024 / 1024} MB, with no file over ${MAX_FILE_BYTES / 1024 / 1024} MB`;
}
