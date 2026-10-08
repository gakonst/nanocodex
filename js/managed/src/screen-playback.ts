// Screen HLS playback: owner-authorized, token-scoped, in-memory live HLS relay.
// Contract: output/screen-connect-20261008/playback-contract.md (agent3).
import { DurableObject } from "cloudflare:workers";
import type { Principal } from "./account-auth";

type Stub = { fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> };
type Namespace = { getByName(name: string): Stub };
export type ScreenPlaybackEnv = { NANOCODEX_SCREEN_PLAYBACK: Namespace };
export type ScreenPlaybackHostCommand = Readonly<{
  ownerId: string; machine_id: string; surface_id: string; generation?: string | number;
  command: Record<string, unknown>;
}>;
export type ScreenPlaybackHost = (input: ScreenPlaybackHostCommand) => Promise<Response>;
export type ScreenPlaybackOptions = Readonly<{
  authenticate: () => Promise<Principal | undefined>;
  host: ScreenPlaybackHost;
}>;
export type ScreenPlaybackState = "starting" | "live" | "failed" | "ended" | "revoked" | "expired";

const LINKS = "/v1/account/hands/playback-links";
const PUBLIC = "/v1/screen-playback/";
const MAX_ACTIVE = 4;
const MAX_SEGMENTS = 6;
const MAX_SEGMENT_BYTES = 4 * 1024 * 1024;
const MAX_PLAYLIST_BYTES = 16 * 1024;
const RETENTION_MS = 24 * 60 * 60 * 1000;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const STREAM_ID = /^sp_[0-9a-f]{32}$/;
const VIEW_TOKEN = /^nsv_[A-Za-z0-9_-]{43}$/;
const UPLOAD_TOKEN = /^nsu_[A-Za-z0-9_-]{43}$/;
const SEGMENT = /^s(0|[1-9][0-9]{0,9})\.ts$/;
const HOST_ERRORS = new Set(["invalid_request", "unsupported", "busy", "capture_failed", "encoder_failed",
  "upload_rejected", "expired", "broadcast_failed"]);
const ACTIVE = new Set<ScreenPlaybackState>(["starting", "live"]);

const noStore = { "cache-control": "no-store", "x-content-type-options": "nosniff" };
function json(value: unknown, status = 200, extra: HeadersInit = {}): Response {
  return new Response(JSON.stringify(value), { status, headers: { ...noStore, "content-type": "application/json", ...extra } });
}
const error = (code: string, status: number, extra: HeadersInit = {}) => json({ error: code }, status, extra);
const empty = (status: number, extra: HeadersInit = {}) => new Response(null, { status, headers: { ...noStore, ...extra } });
const notFound = () => empty(404);

function randomToken(prefix: string): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  return prefix + btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
function randomId(): string {
  return "sp_" + [...crypto.getRandomValues(new Uint8Array(16))].map((b) => b.toString(16).padStart(2, "0")).join("");
}
async function sha256(text: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** Replace the view token path segment so playback URLs can be logged safely. */
export function redactScreenPlaybackPath(path: string): string {
  return path.replace(/(\/v1\/screen-playback\/[^/]+\/)(?!upload(?:\/|$))[^/?#]+/, "$1[redacted]");
}

/** Host command transport through the owner's account-tools broker (implemented by agent1). */
export function accountToolsPlaybackHost(env: { NANOCODEX_ACCOUNT_TOOLS: Namespace }): ScreenPlaybackHost {
  return (input) => env.NANOCODEX_ACCOUNT_TOOLS.getByName(input.ownerId).fetch(
    "https://account-tools.internal/screens/host-command",
    {
      method: "POST",
      headers: { "content-type": "application/json", "x-nanocodex-owner-id": input.ownerId },
      body: JSON.stringify({ machine_id: input.machine_id, surface_id: input.surface_id,
        ...(input.generation === undefined ? {} : { generation: input.generation }), command: input.command }),
    },
  );
}

/**
 * Forward a host `broadcast_result` (target "hls") to its stream. Only state/error are applied.
 * Pass the broker's ownerId so a host can only affect its own owner's streams.
 */
export async function recordScreenPlaybackHostResult(env: ScreenPlaybackEnv, value: unknown, ownerId?: string): Promise<boolean> {
  if (!value || typeof value !== "object") return false;
  const result = value as Record<string, unknown>;
  if (result.type !== "broadcast_result" || result.target !== "hls" || typeof result.stream_id !== "string"
    || !STREAM_ID.test(result.stream_id) || typeof result.status !== "string") return false;
  const errorCode = typeof result.error === "string" && HOST_ERRORS.has(result.error) ? result.error : undefined;
  const response = await env.NANOCODEX_SCREEN_PLAYBACK.getByName(`stream:${result.stream_id}`).fetch(
    "https://screen-playback.internal/stream/host-result",
    { method: "POST", body: JSON.stringify({ owner: ownerId, status: result.status, error: errorCode }) },
  );
  return response.ok;
}

export async function routeScreenPlayback(
  request: Request, env: ScreenPlaybackEnv, url: URL, options: ScreenPlaybackOptions,
): Promise<Response | undefined> {
  if (url.pathname.startsWith(PUBLIC)) return publicRoute(request, env, url);
  if (url.pathname === LINKS || url.pathname.startsWith(`${LINKS}/`)) return ownerRoute(request, env, url, options);
  return undefined;
}

type CreateBody = Readonly<{
  operation_id: string; machine_id: string; surface_id: string; generation?: string | number;
  expires_in_seconds: number; preset: "720p" | "1080p";
}>;

function parseCreate(value: unknown): CreateBody | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const body = value as Record<string, unknown>;
  const allowed = new Set(["operation_id", "machine_id", "surface_id", "generation", "expires_in_seconds", "preset"]);
  if (Object.keys(body).some((key) => !allowed.has(key))) return undefined;
  const text = (v: unknown) => typeof v === "string" && v.length > 0 && v.length <= 256;
  if (typeof body.operation_id !== "string" || !UUID.test(body.operation_id)) return undefined;
  if (!text(body.machine_id) || !text(body.surface_id)) return undefined;
  const generation = body.generation;
  if (generation !== undefined && !text(generation) && !(typeof generation === "number" && Number.isSafeInteger(generation)))
    return undefined;
  const expires = body.expires_in_seconds ?? 3600;
  if (typeof expires !== "number" || !Number.isInteger(expires) || expires < 60 || expires > 28800) return undefined;
  const preset = body.preset ?? "720p";
  if (preset !== "720p" && preset !== "1080p") return undefined;
  return {
    operation_id: body.operation_id.toLowerCase(), machine_id: body.machine_id as string,
    surface_id: body.surface_id as string, ...(generation === undefined ? {} : { generation: generation as string | number }),
    expires_in_seconds: expires, preset,
  };
}

const internal = (path: string, body: unknown) => new Request(`https://screen-playback.internal${path}`,
  { method: "POST", body: JSON.stringify(body) });

async function hostError(response: Response): Promise<string> {
  const body = await response.json().catch(() => undefined) as { error?: unknown } | undefined;
  return typeof body?.error === "string" ? body.error : "";
}

async function ownerRoute(request: Request, env: ScreenPlaybackEnv, url: URL, options: ScreenPlaybackOptions): Promise<Response> {
  if (url.search !== "") return error("invalid_request", 400);
  const principal = await options.authenticate();
  if (!principal) return error("unauthorized", 401);
  const read = request.method === "GET";
  if (principal.connectGrant || principal.kind === "connect_grant"
    || !principal.capabilities.includes(read ? "agents:read" : "agents:write")
    || !principal.capabilities.includes("tools:use")) return error("forbidden", 403);
  if (!read && principal.kind === "account_session" && request.headers.get("origin") !== url.origin)
    return error("forbidden_origin", 403);
  const owner = env.NANOCODEX_SCREEN_PLAYBACK.getByName(`owner:${principal.userId}`);
  const tail = url.pathname.slice(LINKS.length);
  if (tail === "" || tail === "/") {
    if (request.method === "GET") return owner.fetch(internal("/owner/list", {}));
    if (request.method !== "POST") return error("invalid_request", 405, { allow: "GET, POST" });
    const raw = await request.text();
    if (raw.length > 4096) return error("invalid_request", 400);
    let parsed: unknown;
    try { parsed = JSON.parse(raw); } catch { return error("invalid_request", 400); }
    const body = parseCreate(parsed);
    if (!body) return error("invalid_request", 400);
    return createLink(env, url, options, principal.userId, body);
  }
  const id = tail.slice(1);
  if (!STREAM_ID.test(id)) return error("not_found", 404);
  if (request.method !== "DELETE") return error("invalid_request", 405, { allow: "DELETE" });
  const revoked = await owner.fetch(internal("/owner/revoke", { id }));
  if (revoked.status !== 200) return revoked;
  const result = await revoked.json() as { link: PublicLink; was_active: boolean };
  if (result.was_active) {
    await options.host({ ownerId: principal.userId, machine_id: result.link.machine_id, surface_id: result.link.surface_id,
      command: { type: "broadcast", target: "hls", action: "stop", request_id: crypto.randomUUID(),
        surface_id: result.link.surface_id, stream_id: id } }).catch(() => undefined);
  }
  return json({ id, state: "revoked", revoked_at: result.link.revoked_at });
}

async function createLink(env: ScreenPlaybackEnv, url: URL, options: ScreenPlaybackOptions, ownerId: string,
  body: CreateBody): Promise<Response> {
  const owner = env.NANOCODEX_SCREEN_PLAYBACK.getByName(`owner:${ownerId}`);
  const bodyHash = await sha256(JSON.stringify([body.machine_id, body.surface_id, body.generation ?? null,
    body.expires_in_seconds, body.preset]));
  const reserved = await owner.fetch(internal("/owner/reserve", { owner: ownerId, body, body_hash: bodyHash }));
  if (reserved.status !== 201) return reserved;
  const created = await reserved.json() as { link: PublicLink; view_token: string; upload_token: string };
  const id = created.link.id;
  const local = url.hostname === "localhost" || url.hostname === "127.0.0.1";
  const origin = local ? url.origin : `https://${url.host}`;
  let response: Response | undefined;
  try {
    response = await options.host({ ownerId, machine_id: body.machine_id, surface_id: body.surface_id,
      ...(body.generation === undefined ? {} : { generation: body.generation }),
      command: { type: "broadcast", target: "hls", action: "start", request_id: crypto.randomUUID(),
        surface_id: body.surface_id, stream_id: id, preset: body.preset,
        upload: { url: `${origin}${PUBLIC}${id}/upload/`, token: created.upload_token, expires_at: created.link.expires_at } } });
  } catch { response = undefined; }
  if (response?.ok) {
    return json({ ...created.link, url: `${origin}${PUBLIC}${id}/${created.view_token}/index.m3u8` }, 201);
  }
  // Definite host rejection: no stream started, so release the reservation and its receipt.
  if (response && (response.status === 404 || response.status === 409)) {
    const code = await hostError(response);
    await owner.fetch(internal("/owner/abort", { id }));
    if (response.status === 404) return error("not_found", 404);
    return error(code === "stale_generation" || code === "busy" ? code : "unsupported", 409);
  }
  // Unreachable or uncertain delivery: keep the receipt as a failed link so tokens can never start serving.
  await owner.fetch(internal("/owner/fail", { id, error: "host_unavailable" }));
  return error("host_unavailable", 503);
}

async function publicRoute(request: Request, env: ScreenPlaybackEnv, url: URL): Promise<Response> {
  const parts = url.pathname.slice(PUBLIC.length).split("/");
  const id = parts[0] ?? "";
  if (!STREAM_ID.test(id) || url.search !== "") return notFound();
  const stream = env.NANOCODEX_SCREEN_PLAYBACK.getByName(`stream:${id}`);
  if (parts[1] === "upload") {
    if (parts.length !== 3) return notFound();
    const auth = request.headers.get("authorization") ?? "";
    const token = auth.startsWith("Bearer ") ? auth.slice(7) : "";
    const file = parts[2] ?? "";
    const headers = new Headers({ "x-upload-token-hash": UPLOAD_TOKEN.test(token) ? await sha256(token) : "",
      "x-file": file, "x-method": request.method, "content-type": request.headers.get("content-type") ?? "" });
    const declared = Number(request.headers.get("content-length") ?? "0");
    if (declared > MAX_SEGMENT_BYTES) return error("too_large", 413);
    return stream.fetch(new Request("https://screen-playback.internal/stream/upload",
      { method: "POST", headers, body: request.body ?? null }));
  }
  if (parts.length !== 3 || !VIEW_TOKEN.test(parts[1] ?? "")) return notFound();
  if (request.method !== "GET" && request.method !== "HEAD") return notFound();
  const file = parts[2] ?? "";
  if (file !== "index.m3u8" && !SEGMENT.test(file)) return notFound();
  const response = await stream.fetch(internal("/stream/play", { view_hash: await sha256(parts[1]!), file }));
  return request.method === "HEAD" ? new Response(null, { status: response.status, headers: response.headers }) : response;
}

type PublicLink = {
  id: string; operation_id: string; machine_id: string; surface_id: string; preset: string;
  state: ScreenPlaybackState; error?: string; created_at: number; expires_at: number;
  revoked_at?: number; last_segment_at?: number;
};
type LinkRow = PublicLink & { owner: string; body_hash: string; view_hash: string; upload_hash: string; last_n: number;
  target_duration: number };
type Entry = Readonly<{ n: number; duration: string; discontinuity: boolean }>;

function publicLink(row: Record<string, unknown>): PublicLink {
  return {
    id: row.id as string, operation_id: row.operation_id as string, machine_id: row.machine_id as string,
    surface_id: row.surface_id as string, preset: row.preset as string, state: row.state as ScreenPlaybackState,
    ...(row.error ? { error: row.error as string } : {}),
    created_at: row.created_at as number, expires_at: row.expires_at as number,
    ...(row.revoked_at == null ? {} : { revoked_at: row.revoked_at as number }),
    ...(row.last_segment_at == null ? {} : { last_segment_at: row.last_segment_at as number }),
  };
}

function hasTsSync(bytes: Uint8Array): boolean {
  if (bytes.byteLength === 0 || bytes.byteLength % 188 !== 0) return false;
  for (let offset = 0; offset < bytes.byteLength; offset += 188) if (bytes[offset] !== 0x47) return false;
  return true;
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.byteLength !== b.byteLength) return false;
  for (let i = 0; i < a.byteLength; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

/** Validate a host playlist; return its segment entries (references must be already-uploaded N). */
function parsePlaylist(text: string, lastN: number): { entries: Entry[]; target: number; ended: boolean } | undefined {
  const lines = text.split(/\r?\n/).map((line) => line.trim()).filter((line) => line !== "");
  if (lines[0] !== "#EXTM3U") return undefined;
  const entries: Entry[] = [];
  let target = 0; let ended = false; let duration: string | undefined; let discontinuity = false;
  for (const line of lines.slice(1)) {
    let match: RegExpExecArray | null;
    if ((match = /^#EXTINF:([0-9]{1,3}(?:\.[0-9]{1,6})?),[^\r\n]{0,64}$/.exec(line))) { duration = match[1]; continue; }
    if ((match = /^#EXT-X-TARGETDURATION:([0-9]{1,2})$/.exec(line))) { target = Number(match[1]); continue; }
    if (/^#EXT-X-(VERSION|MEDIA-SEQUENCE|DISCONTINUITY-SEQUENCE):[0-9]{1,10}$/.test(line)) continue;
    if (line === "#EXT-X-INDEPENDENT-SEGMENTS" || line === "#EXT-X-PLAYLIST-TYPE:EVENT") continue;
    if (line === "#EXT-X-DISCONTINUITY") { discontinuity = true; continue; }
    if (line === "#EXT-X-ENDLIST") { ended = true; continue; }
    if (line.startsWith("#EXT")) return undefined;
    if (line.startsWith("#")) continue;
    match = SEGMENT.exec(line);
    if (!match || duration === undefined) return undefined;
    const n = Number(match[1]);
    if (n > lastN || Number(duration) > 10 || entries.length >= 64) return undefined;
    if (entries.length > 0 && n <= entries[entries.length - 1]!.n) return undefined;
    entries.push({ n, duration, discontinuity });
    duration = undefined; discontinuity = false;
  }
  if (target < 1 || target > 10 || duration !== undefined) return undefined;
  return { entries, target, ended };
}

export class ScreenPlayback extends DurableObject {
  private readonly segments = new Map<number, Uint8Array>();
  private entries: Entry[] = [];
  private ownerNotifiedAt = 0;

  constructor(ctx: DurableObjectState, env: unknown) {
    super(ctx, env as never);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS links (id TEXT PRIMARY KEY, owner TEXT NOT NULL,
      operation_id TEXT NOT NULL UNIQUE, body_hash TEXT NOT NULL, machine_id TEXT NOT NULL, surface_id TEXT NOT NULL,
      preset TEXT NOT NULL, state TEXT NOT NULL, error TEXT, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
      revoked_at INTEGER, last_segment_at INTEGER, view_hash TEXT NOT NULL DEFAULT '', upload_hash TEXT NOT NULL DEFAULT '',
      last_n INTEGER NOT NULL DEFAULT -1, target_duration INTEGER NOT NULL DEFAULT 0)`);
  }

  override async fetch(request: Request): Promise<Response> {
    const path = new URL(request.url).pathname;
    if (path === "/stream/upload") return this.upload(request);
    const body = await request.json() as Record<string, unknown>;
    switch (path) {
      case "/owner/reserve": return this.reserve(body);
      case "/owner/list": return this.list();
      case "/owner/revoke": return this.revokeLink(body.id as string);
      case "/owner/abort": return this.abort(body.id as string);
      case "/owner/fail": return this.fail(body.id as string, body.error as string);
      case "/owner/state": return this.ownerState(body);
      case "/stream/init": return this.init(body as unknown as LinkRow);
      case "/stream/play": return this.play(body.view_hash as string, body.file as string);
      case "/stream/terminate": return this.terminate(body.state as ScreenPlaybackState, body.error as string | undefined);
      case "/stream/host-result": return this.hostResult(body);
      default: return notFound();
    }
  }

  private row(id?: string): Record<string, unknown> | undefined {
    const cursor = id === undefined ? this.ctx.storage.sql.exec("SELECT * FROM links LIMIT 1")
      : this.ctx.storage.sql.exec("SELECT * FROM links WHERE id = ?", id);
    return cursor.toArray()[0] as Record<string, unknown> | undefined;
  }
