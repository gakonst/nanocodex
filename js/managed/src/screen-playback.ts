// Screen HLS playback: owner-authorized, token-scoped, in-memory live HLS relay.
// Operator/user behavior: docs/hand-broadcasting.md (Playback links).
import { DurableObject } from "cloudflare:workers";
import type { Principal } from "./account-auth";
import { handRelayRegion, isHandRelayRegion, type HandRelayRegion } from "./regional-hand-routing";

type Stub = { fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> };
type Namespace = { getByName(name: string, options?: { locationHint?: string }): Stub };
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
/** Host status as forwarded by an authenticated broker; machine/generation come from the host socket. */
export type ScreenPlaybackHostResult = Readonly<{
  type: "broadcast_result"; target: "hls"; stream_id: string; status: string; error?: string;
  machine_id: string; generation: string;
}>;

const LINKS = "/v1/account/hands/playback-links";
const PUBLIC = "/v1/screen-playback/";
const MAX_ACTIVE = 4;
const MAX_SEGMENTS = 6;
const MAX_SEGMENT_BYTES = 4 * 1024 * 1024;
const MAX_PLAYLIST_BYTES = 16 * 1024;
const RETENTION_MS = 24 * 60 * 60 * 1000;
const OWNER_PROGRESS_MS = 15_000;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const STREAM_ID = /^sp_[0-9a-f]{32}$/;
const VIEW_TOKEN = /^nsv_[A-Za-z0-9_-]{43}$/;
const UPLOAD_TOKEN = /^nsu_[A-Za-z0-9_-]{43}$/;
const SEGMENT = /^s(0|[1-9][0-9]{0,9})\.ts$/;
const HOST_ERRORS = new Set(["invalid_request", "unsupported", "busy", "capture_failed", "encoder_failed",
  "upload_rejected", "expired", "broadcast_failed"]);
const ACTIVE = new Set<string>(["starting", "live"]);

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

/** Replace a playback view token query value so playback URLs can be logged safely. */
export function redactScreenPlaybackPath(path: string): string {
  return path.replace(/([?&]token=)[^&#]*/g, "$1[redacted]");
}

/** Host command transport through the owner's account-tools broker (index/account-tools integration). */
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

const streamStub = (env: ScreenPlaybackEnv, id: string, region?: HandRelayRegion) =>
  env.NANOCODEX_SCREEN_PLAYBACK.getByName(`stream:${id}`, region ? { locationHint: region } : undefined);
const ownerStub = (env: ScreenPlaybackEnv, owner: string) => env.NANOCODEX_SCREEN_PLAYBACK.getByName(`owner:${owner}`);

/**
 * Forward a host `broadcast_result` (target "hls") to its stream. The broker must
 * pass its trusted owner and the host socket's machine/generation; a result only
 * applies to a stream started on exactly that owner, machine and generation.
 */
export async function recordScreenPlaybackHostResult(env: ScreenPlaybackEnv, value: unknown, ownerId: string): Promise<boolean> {
  if (!value || typeof value !== "object" || typeof ownerId !== "string" || ownerId === "") return false;
  const result = value as Record<string, unknown>;
  if (result.type !== "broadcast_result" || result.target !== "hls" || typeof result.stream_id !== "string"
    || !STREAM_ID.test(result.stream_id) || typeof result.status !== "string"
    || typeof result.machine_id !== "string" || typeof result.generation !== "string") return false;
  const errorCode = typeof result.error === "string" && HOST_ERRORS.has(result.error) ? result.error
    : result.error === undefined ? undefined : "broadcast_failed";
  const response = await streamStub(env, result.stream_id).fetch(internal("/stream/host-result", {
    owner: ownerId, machine_id: result.machine_id, generation: result.generation, status: result.status, error: errorCode,
  }));
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

function internal(path: string, body: unknown): Request {
  return new Request(`https://screen-playback.internal${path}`, { method: "POST", body: JSON.stringify(body) });
}

async function hostError(response: Response): Promise<string> {
  const body = await response.json().catch(() => undefined) as { error?: unknown } | undefined;
  return typeof body?.error === "string" ? body.error : "";
}

/** Region hint for a stream: a regional publisher generation names its relay region. */
function streamRegion(request: Request, generation: string | number | undefined): HandRelayRegion | undefined {
  const match = typeof generation === "string" ? /^rs\.([a-z]+)\./.exec(generation) : null;
  if (match && isHandRelayRegion(match[1])) return match[1];
  return handRelayRegion(request);
}

async function ownerRoute(request: Request, env: ScreenPlaybackEnv, url: URL, options: ScreenPlaybackOptions): Promise<Response> {
  if (url.search !== "") return error("invalid_request", 400);
  const principal = await options.authenticate();
  if (!principal) return error("unauthorized", 401);
  const read = request.method === "GET";
  if (principal.connectGrant || principal.kind === "connect_grant"
    || !principal.capabilities.includes(read ? "agents:read" : "agents:write")
    || !principal.capabilities.includes("tools:use")) return error("forbidden", 403);
  if (!read && principal.kind !== "api_key" && principal.kind !== "service" && request.headers.get("origin") !== url.origin)
    return error("forbidden_origin", 403);
  const owner = ownerStub(env, principal.userId);
  const tail = url.pathname.slice(LINKS.length);
  if (tail === "" || tail === "/") {
    if (request.method === "GET") return owner.fetch(internal("/owner/list", {}));
    if (request.method !== "POST") return error("invalid_request", 405, { allow: "GET, POST" });
    if (Number(request.headers.get("content-length") ?? "0") > 4096) return error("invalid_request", 400);
    const raw = await boundedBytes(request.body, 4096);
    if (!raw) return error("invalid_request", 400);
    let parsed: unknown;
    try { parsed = JSON.parse(new TextDecoder().decode(raw)); } catch { return error("invalid_request", 400); }
    const body = parseCreate(parsed);
    if (!body) return error("invalid_request", 400);
    return createLink(request, env, url, options, principal.userId, body);
  }
  const id = tail.slice(1);
  if (!STREAM_ID.test(id)) return error("not_found", 404);
  if (request.method !== "DELETE") return error("invalid_request", 405, { allow: "DELETE" });
  const revoked = await owner.fetch(internal("/owner/revoke", { id }));
  if (revoked.status !== 200) return revoked;
  const result = await revoked.json() as { link: PublicLink; was_active: boolean };
  // The stream drops its buffer and refuses view/upload tokens before the host is told to stop.
  await streamStub(env, id).fetch(internal("/stream/terminate", { state: "revoked" }));
  if (result.was_active) {
    await options.host({ ownerId: principal.userId, machine_id: result.link.machine_id, surface_id: result.link.surface_id,
      command: { type: "broadcast", target: "hls", action: "stop", request_id: crypto.randomUUID(),
        surface_id: result.link.surface_id, stream_id: id } }).catch(() => undefined);
  }
  return json({ id, state: "revoked", revoked_at: result.link.revoked_at });
}

async function createLink(request: Request, env: ScreenPlaybackEnv, url: URL, options: ScreenPlaybackOptions, ownerId: string,
  body: CreateBody): Promise<Response> {
  const owner = ownerStub(env, ownerId);
  const bodyHash = await sha256(JSON.stringify([body.machine_id, body.surface_id, body.generation ?? null,
    body.expires_in_seconds, body.preset]));
  const reserved = await owner.fetch(internal("/owner/reserve", { owner: ownerId, body, body_hash: bodyHash }));
  if (reserved.status !== 201) return reserved;
  const created = await reserved.json() as { link: PublicLink; view_token: string; upload_token: string };
  const id = created.link.id;
  const stream = streamStub(env, id, streamRegion(request, body.generation));
  // Tokens exist only in this request; durable state keeps their hashes.
  const initialized = await stream.fetch(internal("/stream/init", {
    id, owner: ownerId, machine_id: body.machine_id, surface_id: body.surface_id,
    generation: body.generation === undefined ? null : String(body.generation),
    view_hash: await sha256(created.view_token), upload_hash: await sha256(created.upload_token),
    expires_at: created.link.expires_at,
  })).catch(() => undefined);
  if (initialized?.status === 410) return error("revoked", 409);
  if (!initialized?.ok) {
    await owner.fetch(internal("/owner/fail", { id, error: "host_unavailable" }));
    return error("host_unavailable", 503);
  }
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
    const value = await response.json().catch(() => undefined) as { generation?: unknown } | undefined;
    const generation = typeof value?.generation === "string" ? value.generation
      : body.generation === undefined ? undefined : String(body.generation);
    const activated = generation === undefined ? undefined
      : await stream.fetch(internal("/stream/activate", { generation })).catch(() => undefined);
    if (activated?.ok) {
      const state = await activated.json() as { state: ScreenPlaybackState; error?: string };
      // A revoke may land between reservation and activation: confirm owner authority before showing a URL.
      const confirmed = await owner.fetch(internal("/owner/confirm", { id })).then((r) => r.json() as Promise<{ state?: string }>)
        .catch(() => ({ state: undefined }));
      const ownerActive = confirmed.state === "starting" || confirmed.state === "live";
      const streamActive = state.state === "starting" || state.state === "live";
      if (!ownerActive || !streamActive) {
        // Fail closed: unknown/terminal owner or stream state never yields a URL; stop this exact stream.
        await stream.fetch(internal("/stream/terminate", { state: confirmed.state === "revoked" ? "revoked" : "failed",
          error: "broadcast_failed" })).catch(() => undefined);
        if (confirmed.state !== "revoked") await owner.fetch(internal("/owner/fail", { id, error: state.error ?? "broadcast_failed" })).catch(() => undefined);
        await stopHost(options, ownerId, body, id);
        if (confirmed.state === "revoked" || state.state === "revoked") return error("revoked", 409);
        if (state.error === "busy") return error("busy", 409);
        return error(confirmed.state === undefined ? "host_unavailable" : "host_failed", confirmed.state === undefined ? 503 : 409);
      }
      return json({ ...created.link, state: state.state, url: `${origin}${PUBLIC}${id}/index.m3u8?token=${created.view_token}` }, 201);
    }
    // The command was delivered but the stream cannot bind its publisher: never serve these tokens.
    response = undefined;
  }
  // Definite broker rejection: no stream started, so release the reservation and its receipt.
  if (response && (response.status === 404 || response.status === 409)) {
    const code = await hostError(response);
    await stream.fetch(internal("/stream/terminate", { state: "failed", error: "unsupported", drop: true })).catch(() => undefined);
    await owner.fetch(internal("/owner/abort", { id }));
    if (response.status === 404) return error("not_found", 404);
    return error(code === "stale_generation" || code === "busy" ? code : "unsupported", 409);
  }
  // Unreachable or uncertain delivery: keep the receipt as a failed link; uploads receive 410 and stop.
  await stream.fetch(internal("/stream/terminate", { state: "failed", error: "host_unavailable" })).catch(() => undefined);
  await owner.fetch(internal("/owner/fail", { id, error: "host_unavailable" }));
  await stopHost(options, ownerId, body, id);
  return error("host_unavailable", 503);
}

async function stopHost(options: ScreenPlaybackOptions, ownerId: string, body: CreateBody, id: string): Promise<void> {
  await options.host({ ownerId, machine_id: body.machine_id, surface_id: body.surface_id,
    command: { type: "broadcast", target: "hls", action: "stop", request_id: crypto.randomUUID(),
      surface_id: body.surface_id, stream_id: id } }).catch(() => undefined);
}

const viewHeaders = { "referrer-policy": "no-referrer", "access-control-allow-origin": "*" };
const publicNotFound = () => empty(404, viewHeaders);

async function publicRoute(request: Request, env: ScreenPlaybackEnv, url: URL): Promise<Response> {
  const parts = url.pathname.slice(PUBLIC.length).split("/");
  const id = parts[0] ?? "";
  if (!STREAM_ID.test(id)) return publicNotFound();
  const stream = streamStub(env, id);
  if (parts[1] === "upload") {
    if (parts.length !== 3 || url.search !== "") return notFound();
    const auth = request.headers.get("authorization") ?? "";
    const token = auth.startsWith("Bearer ") ? auth.slice(7) : "";
    const file = parts[2] ?? "";
    const method = request.method;
    if (!(method === "PUT" && (file === "index.m3u8" || SEGMENT.test(file))) && !(method === "DELETE" && file === ""))
      return error("invalid_request", 400);
    const declared = Number(request.headers.get("content-length") ?? "0");
    if (declared > MAX_SEGMENT_BYTES) return error("too_large", 413);
    const headers = new Headers({ "x-upload-token-hash": UPLOAD_TOKEN.test(token) ? await sha256(token) : "",
      "x-file": file, "x-method": method, "x-content-type": request.headers.get("content-type") ?? "" });
    return stream.fetch(new Request("https://screen-playback.internal/stream/upload",
      { method: "POST", headers, body: method === "PUT" ? request.body : null }));
  }
  // View: exactly one `token` query parameter; tokens never appear in the path.
  if (parts.length !== 2 || (request.method !== "GET" && request.method !== "HEAD")) return publicNotFound();
  const params = [...url.searchParams];
  const token = params.length === 1 && params[0]![0] === "token" ? params[0]![1] : "";
  const file = parts[1] ?? "";
  if (!VIEW_TOKEN.test(token) || (file !== "index.m3u8" && !SEGMENT.test(file))) return publicNotFound();
  const response = await stream.fetch(internal("/stream/play", { view_hash: await sha256(token), file }));
  const headers = new Headers(response.headers);
  for (const [name, value] of Object.entries(viewHeaders)) headers.set(name, value);
  if (request.method === "HEAD") return new Response(null, { status: response.status, headers });
  if (file !== "index.m3u8" || response.status !== 200) return new Response(response.body, { status: response.status, headers });
  // The DO renders only allowlisted relative `s<N>.ts` URIs; the edge appends the caller's token to exactly those.
  const query = `?token=${token}`;
  const body = (await response.text()).split("\n").map((line) => SEGMENT.test(line) ? line + query : line).join("\n");
  headers.delete("content-length");
  return new Response(body, { status: 200, headers });
}

type PublicLink = {
  id: string; operation_id: string; machine_id: string; surface_id: string; preset: string;
  state: ScreenPlaybackState; error?: string; created_at: number; expires_at: number;
  revoked_at?: number; last_segment_at?: number;
};
type StreamRow = {
  id: string; owner: string; machine_id: string; surface_id: string; generation: string | null;
  view_hash: string; upload_hash: string; state: string; error: string | null; expires_at: number;
  last_n: number; last_segment_at: number | null; pending_result: string | null;
};
type Entry = Readonly<{ n: number; duration: string; discontinuity: boolean }>;
type Playlist = { entries: Entry[]; target: number; discontinuitySequence: number; ended: boolean };

function publicLink(row: Record<string, unknown>, now = Date.now()): PublicLink {
  const state = ACTIVE.has(row.state as string) && (row.expires_at as number) <= now ? "expired" : row.state as ScreenPlaybackState;
  return {
    id: row.id as string, operation_id: row.operation_id as string, machine_id: row.machine_id as string,
    surface_id: row.surface_id as string, preset: row.preset as string, state,
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

/** Read at most `limit` bytes; undefined when the body is larger. */
async function boundedBytes(body: ReadableStream<Uint8Array> | null, limit: number): Promise<Uint8Array | undefined> {
  if (!body) return new Uint8Array();
  const reader = body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > limit) { await reader.cancel().catch(() => undefined); return undefined; }
    chunks.push(value);
  }
  const out = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) { out.set(chunk, offset); offset += chunk.byteLength; }
  return out;
}

/**
 * Validate a host playlist. Only an allowlist of live-media tags is accepted:
 * no keys, maps, byte ranges, variants or absolute/external URIs. URIs must be
 * already-accepted relative `s<N>.ts` segments. The server re-renders output.
 */
function parsePlaylist(text: string, lastN: number): Playlist | undefined {
  const lines = text.split(/\r?\n/).map((line) => line.trim()).filter((line) => line !== "");
  if (lines[0] !== "#EXTM3U") return undefined;
  const entries: Entry[] = [];
  let target = 0; let ended = false; let duration: string | undefined; let discontinuity = false;
  let discontinuitySequence = 0;
  for (const line of lines.slice(1)) {
    let match: RegExpExecArray | null;
    if ((match = /^#EXTINF:([0-9]{1,3}(?:\.[0-9]{1,6})?),[^\r\n]{0,64}$/.exec(line))) { duration = match[1]; continue; }
    if ((match = /^#EXT-X-TARGETDURATION:([0-9]{1,2})$/.exec(line))) { target = Number(match[1]); continue; }
    if ((match = /^#EXT-X-DISCONTINUITY-SEQUENCE:([0-9]{1,9})$/.exec(line))) { discontinuitySequence = Number(match[1]); continue; }
    if (/^#EXT-X-(VERSION|MEDIA-SEQUENCE):[0-9]{1,10}$/.test(line)) continue;
    if (line === "#EXT-X-INDEPENDENT-SEGMENTS" || line === "#EXT-X-PLAYLIST-TYPE:EVENT") continue;
    if (line === "#EXT-X-DISCONTINUITY") { discontinuity = true; continue; }
    if (line === "#EXT-X-ENDLIST") { ended = true; continue; }
    if (line.startsWith("#EXT")) return undefined;
    if (line.startsWith("#")) continue;
    match = SEGMENT.exec(line);
    if (!match || duration === undefined) return undefined;
    const n = Number(match[1]);
    if (n > lastN || Number(duration) > 10 || entries.length >= MAX_SEGMENTS) return undefined;
    if (entries.length > 0 && n <= entries[entries.length - 1]!.n) return undefined;
    entries.push({ n, duration, discontinuity });
    duration = undefined; discontinuity = false;
  }
  if (target < 1 || target > 10 || duration !== undefined || discontinuity) return undefined;
  if (entries.some((entry) => Math.round(Number(entry.duration)) > target)) return undefined;
  return { entries, target, discontinuitySequence, ended };
}

/**
 * One class, two roles by name: `owner:<user>` keeps link receipts (no tokens);
 * `stream:<id>` keeps token hashes, the publisher binding and the accepted
 * sequence durably, while media lives only in RAM (at most six segments).
 */
export class ScreenPlayback extends DurableObject {
  private readonly segments = new Map<number, Uint8Array>();
  private playlist: Playlist | undefined;
  private ownerNotifiedAt = 0;
  private ready = false;

  /** Tables are created only by writes so probes of unknown IDs leave no storage. */
  private ensure(): void {
    if (this.ready) return;
    this.ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS links (id TEXT PRIMARY KEY, owner TEXT NOT NULL,
      operation_id TEXT NOT NULL UNIQUE, body_hash TEXT NOT NULL, machine_id TEXT NOT NULL, surface_id TEXT NOT NULL,
      preset TEXT NOT NULL, state TEXT NOT NULL, error TEXT, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
      revoked_at INTEGER, last_segment_at INTEGER)`);
    this.ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS stream (id TEXT PRIMARY KEY, owner TEXT NOT NULL,
      machine_id TEXT NOT NULL, surface_id TEXT NOT NULL, generation TEXT, view_hash TEXT NOT NULL, upload_hash TEXT NOT NULL,
      state TEXT NOT NULL, error TEXT, expires_at INTEGER NOT NULL, last_n INTEGER NOT NULL DEFAULT -1,
      last_segment_at INTEGER, pending_result TEXT)`);
    // Durable fence for a stream terminated before (or without) init: a late init can never resurrect it.
    this.ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS tombstone (state TEXT NOT NULL, at INTEGER NOT NULL)");
    this.ready = true;
  }
  private query<T>(sql: string, ...bindings: unknown[]): T[] {
    try { return this.ctx.storage.sql.exec(sql, ...bindings).toArray() as T[]; }
    catch (cause) { if (/no such table/.test(String(cause))) return []; throw cause; }
  }

  override async fetch(request: Request): Promise<Response> {
    const path = new URL(request.url).pathname;
    if (path === "/stream/upload") return this.upload(request);
    const body = await request.json() as Record<string, unknown>;
    switch (path) {
      case "/owner/reserve": return this.reserve(body);
      case "/owner/list": return this.list();
      case "/owner/revoke": return this.revokeLink(String(body.id));
      case "/owner/abort": return this.abort(String(body.id));
      case "/owner/fail": return this.failLink(String(body.id), String(body.error));
      case "/owner/state": return this.ownerState(body);
      case "/owner/confirm": return this.confirm(String(body.id));
      case "/stream/init": return this.init(body);
      case "/stream/activate": return this.activate(String(body.generation));
      case "/stream/play": return this.play(String(body.view_hash), String(body.file));
      case "/stream/terminate": return this.terminate(body.state as ScreenPlaybackState,
        typeof body.error === "string" ? body.error : undefined, body.drop === true);
      case "/stream/host-result": return this.hostResult(body);
      default: return notFound();
    }
  }

  // ---- owner index ----------------------------------------------------------

  private sweepOwner(now: number): void {
    this.ctx.storage.sql.exec("UPDATE links SET state='expired' WHERE state IN ('starting','live') AND expires_at <= ?", now);
    this.ctx.storage.sql.exec("DELETE FROM links WHERE expires_at < ?", now - RETENTION_MS);
  }

  private reserve(body: Record<string, unknown>): Response {
    this.ensure();
    const now = Date.now();
    this.sweepOwner(now);
    const input = body.body as CreateBody;
    const existing = this.query<Record<string, unknown>>("SELECT * FROM links WHERE operation_id = ?", input.operation_id)[0];
    if (existing) {
      if (existing.body_hash !== body.body_hash) return error("operation_conflict", 409);
      return json({ ...publicLink(existing, now), url_available: false }, 200);
    }
    const active = this.query<Record<string, unknown>>("SELECT machine_id FROM links WHERE state IN ('starting','live')");
    // One encoder slot per Hand: a second stream would be busy on the host, never falsely live.
    if (active.some((row) => row.machine_id === input.machine_id)) return error("busy", 409);
    if (active.length >= MAX_ACTIVE) return error("too_many_streams", 429);
    const id = randomId();
    const expires = now + input.expires_in_seconds * 1000;
    this.ctx.storage.sql.exec(`INSERT INTO links (id, owner, operation_id, body_hash, machine_id, surface_id, preset, state,
      created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, 'starting', ?, ?)`, id, String(body.owner), input.operation_id,
      String(body.body_hash), input.machine_id, input.surface_id, input.preset, now, expires);
    const row = this.query<Record<string, unknown>>("SELECT * FROM links WHERE id = ?", id)[0]!;
    return json({ link: publicLink(row, now), view_token: randomToken("nsv_"), upload_token: randomToken("nsu_") }, 201);
  }

  private list(): Response {
    const now = Date.now();
    if (this.query("SELECT 1 FROM links LIMIT 1").length > 0) this.sweepOwner(now);
    const rows = this.query<Record<string, unknown>>("SELECT * FROM links ORDER BY created_at DESC LIMIT 100");
    return json({ data: rows.map((row) => publicLink(row, now)) });
  }

  private revokeLink(id: string): Response {
    const now = Date.now();
    const row = this.query<Record<string, unknown>>("SELECT * FROM links WHERE id = ?", id)[0];
    if (!row) return error("not_found", 404);
    const wasActive = ACTIVE.has(row.state as string) && (row.expires_at as number) > now;
    if (row.revoked_at == null) {
      this.ctx.storage.sql.exec("UPDATE links SET state='revoked', revoked_at=? WHERE id = ?", now, id);
    }
    const updated = this.query<Record<string, unknown>>("SELECT * FROM links WHERE id = ?", id)[0]!;
    return json({ link: publicLink(updated, now), was_active: wasActive });
  }

  private confirm(id: string): Response {
    const row = this.query<Record<string, unknown>>("SELECT * FROM links WHERE id = ?", id)[0];
    return json({ state: row ? publicLink(row).state : "revoked" });
  }

  private abort(id: string): Response {
    this.query("DELETE FROM links WHERE id = ? AND state = 'starting'", id);
    return empty(204);
  }

  private failLink(id: string, code: string): Response {
    this.query("UPDATE links SET state='failed', error=? WHERE id = ? AND state IN ('starting','live')", code, id);
    return empty(204);
  }

  /** Stream -> owner progress. Terminal owner states (revoked) are never overwritten. */
  private ownerState(body: Record<string, unknown>): Response {
    const state = String(body.state);
    if (!["starting", "live", "failed", "ended", "expired"].includes(state)) return error("invalid_request", 400);
    this.query(`UPDATE links SET state=?, error=COALESCE(?, error), last_segment_at=COALESCE(?, last_segment_at)
      WHERE id = ? AND state IN ('starting','live')`, state, typeof body.error === "string" ? body.error : null,
      typeof body.last_segment_at === "number" ? body.last_segment_at : null, String(body.id));
    return empty(204);
  }

  // ---- stream ----------------------------------------------------------------

  private stream(): StreamRow | undefined {
    return this.query<StreamRow>("SELECT * FROM stream LIMIT 1")[0];
  }

  private async notifyOwner(row: StreamRow, state: string, extra: { error?: string; last_segment_at?: number } = {}): Promise<void> {
    this.ownerNotifiedAt = Date.now();
    const env = this.env as unknown as ScreenPlaybackEnv;
    await ownerStub(env, row.owner).fetch(internal("/owner/state", { id: row.id, state, ...extra })).catch(() => undefined);
  }

  private init(body: Record<string, unknown>): Response {
    this.ensure();
    if (this.query("SELECT 1 FROM tombstone LIMIT 1").length > 0) return error("gone", 410);
    const existing = this.stream();
    if (existing) {
      if (existing.id !== body.id) return error("conflict", 409);
      return ACTIVE.has(existing.state) ? empty(204) : error("gone", 410);
    }
    this.ctx.storage.sql.exec(`INSERT INTO stream (id, owner, machine_id, surface_id, generation, view_hash, upload_hash,
      state, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?, 'starting', ?)`, String(body.id), String(body.owner),
      String(body.machine_id), String(body.surface_id), typeof body.generation === "string" ? body.generation : null,
      String(body.view_hash), String(body.upload_hash), Number(body.expires_at));
    void this.ctx.storage.setAlarm(Number(body.expires_at));
    return empty(204);
  }

  /** Bind the publisher generation that accepted the start command; apply a result that raced ahead of it. */
  private async activate(generation: string): Promise<Response> {
    const row = this.stream();
    if (!row) return error("gone", 410);
    if (!ACTIVE.has(row.state)) return json({ state: row.state, ...(row.error ? { error: row.error } : {}) });
    if (row.generation !== null && row.generation !== generation) {
      await this.terminate("failed", "broadcast_failed");
      return error("stale_generation", 409);
    }
    if (row.generation === null) this.ctx.storage.sql.exec("UPDATE stream SET generation = ? WHERE id = ?", generation, row.id);
    const pending = row.pending_result ? JSON.parse(row.pending_result) as { generation: string; status: string; error?: string } : undefined;
    if (pending) {
      this.ctx.storage.sql.exec("UPDATE stream SET pending_result = NULL WHERE id = ?", row.id);
      if (pending.generation === generation) await this.applyHost(pending.status, pending.error);
    }
    const current = this.stream()!;
    return json({ state: current.state, ...(current.error ? { error: current.error } : {}) });
  }

  private async hostResult(body: Record<string, unknown>): Promise<Response> {
    const row = this.stream();
    if (!row) return notFound();
    // A result binds the trusted broker owner and the host socket's machine and generation.
    if (body.owner !== row.owner || body.machine_id !== row.machine_id || typeof body.generation !== "string")
      return error("forbidden", 403);
    const status = String(body.status);
    const code = typeof body.error === "string" ? body.error : undefined;
    if (row.generation === null) {
      const parked = row.pending_result ? JSON.parse(row.pending_result) as { status: string } : undefined;
      // Keep the most decisive parked status until the start command's generation is known.
      if (!parked || status === "failed" || status === "stopped" || !["failed", "stopped"].includes(parked.status)) {
        this.ctx.storage.sql.exec("UPDATE stream SET pending_result = ? WHERE id = ?",
          JSON.stringify({ generation: body.generation, status, ...(code ? { error: code } : {}) }), row.id);
      }
      return empty(202);
    }
    if (body.generation !== row.generation) return error("forbidden", 403);
    await this.applyHost(status, code);
    return empty(204);
  }

  /** Host status never claims liveness: only accepted media makes a stream live. */
  private async applyHost(status: string, code: string | undefined): Promise<void> {
    const row = this.stream();
    if (!row || !ACTIVE.has(row.state)) return;
    if (status === "failed") await this.terminate("failed", code ?? "broadcast_failed");
    else if (status === "stopped") await this.terminate(code ? "failed" : "ended", code);
  }

  private async terminate(state: ScreenPlaybackState, code?: string, drop = false): Promise<Response> {
    this.segments.clear(); this.playlist = undefined;
    const row = this.stream();
    if (!row) {
      // Terminated before init (e.g. revoke racing create): leave a durable fence that expires with retention.
      this.ensure();
      if (this.query("SELECT 1 FROM tombstone LIMIT 1").length === 0)
        this.ctx.storage.sql.exec("INSERT INTO tombstone (state, at) VALUES (?, ?)", state, Date.now());
      await this.ctx.storage.setAlarm(Date.now() + RETENTION_MS);
      return empty(204);
    }
    if (drop && row.state === "starting" && row.last_n < 0) {
      // Definite broker rejection before any media: release, but keep the fence so the ID never revives.
      this.ctx.storage.sql.exec("DELETE FROM stream");
      this.ctx.storage.sql.exec("INSERT INTO tombstone (state, at) VALUES (?, ?)", "failed", Date.now());
      await this.ctx.storage.setAlarm(Date.now() + RETENTION_MS);
      return empty(204);
    }
    const terminal = !ACTIVE.has(row.state);
    // Revocation overrides any terminal state; other terminal states are final.
    if (terminal && !(state === "revoked" && row.state !== "revoked")) return empty(204);
    this.ctx.storage.sql.exec("UPDATE stream SET state = ?, error = ? WHERE id = ?", state,
      state === "failed" ? code ?? "broadcast_failed" : null, row.id);
    await this.ctx.storage.setAlarm(Date.now() + RETENTION_MS);
    if (!terminal && state !== "revoked") await this.notifyOwner(row, state, state === "failed" ? { error: code ?? "broadcast_failed" } : {});
    return empty(204);
  }

  override async alarm(): Promise<void> {
    const row = this.stream();
    if (!row) { if (this.query("SELECT 1 FROM tombstone LIMIT 1").length > 0) { await this.ctx.storage.deleteAll(); this.ready = false; } return; }
    if (!ACTIVE.has(row.state)) { await this.ctx.storage.deleteAll(); this.ready = false; return; }
    if (row.expires_at <= Date.now()) await this.terminate("expired");
    else await this.ctx.storage.setAlarm(row.expires_at);
  }

  /** Active row, after applying expiry. */
  private async live(row: StreamRow): Promise<boolean> {
    if (!ACTIVE.has(row.state)) return false;
    if (row.expires_at > Date.now()) return true;
    await this.terminate("expired");
    return false;
  }

  private async upload(request: Request): Promise<Response> {
    const row = this.stream();
    if (!row) { await request.body?.cancel().catch(() => undefined); return error("not_found", 404); }
    const hash = request.headers.get("x-upload-token-hash") ?? "";
    if (hash === "" || hash !== row.upload_hash) { await request.body?.cancel().catch(() => undefined); return error("unauthorized", 401); }
    if (!await this.live(row)) { await request.body?.cancel().catch(() => undefined); return error("gone", 410); }
    const file = request.headers.get("x-file") ?? "";
    if (request.headers.get("x-method") === "DELETE") { await this.terminate("ended"); return empty(204); }
    const contentType = (request.headers.get("x-content-type") ?? "").split(";")[0]!.trim().toLowerCase();
    if (file === "index.m3u8") {
      if (contentType !== "" && contentType !== "application/vnd.apple.mpegurl" && contentType !== "audio/mpegurl")
        return error("invalid_request", 400);
      const bytes = await boundedBytes(request.body, MAX_PLAYLIST_BYTES);
      if (!bytes) return error("too_large", 413);
      let text: string;
      try { text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(bytes); } catch { return error("invalid_request", 400); }
      return this.acceptPlaylist(row, text);
    }
    const match = SEGMENT.exec(file);
    if (!match || contentType !== "video/mp2t") { await request.body?.cancel().catch(() => undefined); return error("invalid_request", 400); }
    const bytes = await boundedBytes(request.body, MAX_SEGMENT_BYTES);
    if (!bytes) return error("too_large", 413);
    if (!hasTsSync(bytes)) return error("invalid_request", 400);
    const n = Number(match[1]);
    const held = this.segments.get(n);
    if (n <= row.last_n) {
      if (held) return sameBytes(held, bytes) ? empty(204) : error("sequence", 409);
      // RAM was lost (eviction/restart) while the uploader still holds its window: refill it.
      if (row.last_n - n >= MAX_SEGMENTS) return error("sequence", 409);
      this.hold(n, bytes);
      return empty(204);
    }
    // The accepted sequence is durable before acknowledgement, so it survives eviction.
    const now = Date.now();
    this.ctx.storage.sql.exec("UPDATE stream SET last_n = ?, last_segment_at = ? WHERE id = ?", n, now, row.id);
    this.hold(n, bytes);
    if (row.state === "live" && now - this.ownerNotifiedAt >= OWNER_PROGRESS_MS) await this.notifyOwner(row, "live", { last_segment_at: now });
    return empty(204);
  }

  private hold(n: number, bytes: Uint8Array): void {
    this.segments.set(n, bytes);
    const keep = [...this.segments.keys()].sort((a, b) => b - a).slice(0, MAX_SEGMENTS);
    for (const key of this.segments.keys()) if (!keep.includes(key)) this.segments.delete(key);
  }

  private async acceptPlaylist(row: StreamRow, text: string): Promise<Response> {
    const parsed = parsePlaylist(text, row.last_n);
    if (!parsed) return error("invalid_request", 400);
    const missing = parsed.entries.filter((entry) => !this.segments.has(entry.n)).map((entry) => entry.n);
    if (missing.length > 0) return json({ error: "missing_segments", missing }, 409);
    if (parsed.ended) { await this.terminate("ended"); return empty(204); }
    this.playlist = parsed;
    if (row.state === "starting" && parsed.entries.length > 0) {
      const now = Date.now();
      this.ctx.storage.sql.exec("UPDATE stream SET state = 'live' WHERE id = ? AND state = 'starting'", row.id);
      await this.notifyOwner(row, "live", { last_segment_at: row.last_segment_at ?? now });
    }
    return empty(204);
  }

  private async play(viewHash: string, file: string): Promise<Response> {
    const row = this.stream();
    if (!row || viewHash === "" || viewHash !== row.view_hash || !await this.live(row)) return notFound();
    if (file === "index.m3u8") {
      const body = this.render();
      if (!body) return empty(503, { "retry-after": "1" });
      return new Response(body, { headers: { ...noStore, "content-type": "application/vnd.apple.mpegurl" } });
    }
    const n = Number(SEGMENT.exec(file)?.[1] ?? NaN);
    const bytes = this.segments.get(n);
    if (!bytes) return notFound();
    return new Response(bytes, { headers: { ...noStore, "content-type": "video/mp2t" } });
  }

  /** Server-rendered playlist: the newest contiguous run of segments actually held in RAM. */
  private render(): string | undefined {
    const playlist = this.playlist;
    if (!playlist) return undefined;
    const entries = playlist.entries;
    let start = entries.length;
    while (start > 0 && this.segments.has(entries[start - 1]!.n)
      && (start === entries.length || entries[start - 1]!.n + 1 === entries[start]!.n)) start -= 1;
    const shown = entries.slice(start);
    if (shown.length === 0) return undefined;
    const skippedDiscontinuities = entries.slice(0, start).filter((entry) => entry.discontinuity).length
      + (shown[0]!.discontinuity ? 1 : 0);
    const lines = ["#EXTM3U", "#EXT-X-VERSION:3", `#EXT-X-TARGETDURATION:${playlist.target}`,
      `#EXT-X-MEDIA-SEQUENCE:${shown[0]!.n}`];
    const discontinuitySequence = playlist.discontinuitySequence + skippedDiscontinuities;
    if (discontinuitySequence > 0) lines.push(`#EXT-X-DISCONTINUITY-SEQUENCE:${discontinuitySequence}`);
    shown.forEach((entry, index) => {
      if (entry.discontinuity && index > 0) lines.push("#EXT-X-DISCONTINUITY");
      lines.push(`#EXTINF:${entry.duration},`, `s${entry.n}.ts`);
    });
    return lines.join("\n") + "\n";
  }
}
