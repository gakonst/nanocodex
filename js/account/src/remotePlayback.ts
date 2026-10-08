// Owner client for short-lived, view-only HLS playback links. The bearer URL is
// returned once by create and must stay in component memory only: never persist,
// log, or put it in a query cache.
import type { RemoteHand } from "./handRemote";

export type PlaybackPreset = "720p" | "1080p";
export type PlaybackState = "starting" | "live" | "failed" | "ended" | "revoked" | "expired";
export type PlaybackLink = Readonly<{
  id: string; machine_id: string; surface_id: string; preset: PlaybackPreset; state: PlaybackState;
  created_at: number; expires_at: number; revoked_at?: number; error?: string;
}>;
/** A create receipt. `url` is absent when the server replayed an earlier receipt. */
export type PlaybackCreation = Readonly<{ link: PlaybackLink; url?: string }>;
export type PlaybackRequest = Readonly<{ operationId: string; expiresInSeconds: number; preset: PlaybackPreset }>;
export const playbackDurations = [[900, "15 minutes"], [3600, "1 hour"], [14_400, "4 hours"], [28_800, "8 hours"]] as const;
export const activePlayback = (link: PlaybackLink) => link.state === "starting" || link.state === "live";

export class PlaybackError extends Error {
  /** The server may have created a link; reconcile before creating another. */
  readonly uncertain: boolean;
  readonly code?: string;
  constructor(message: string, uncertain = false, code?: string) { super(message); this.uncertain = uncertain; this.code = code; }
}

const path = "/v1/account/hands/playback-links";
const string = (value: unknown, max = 512): value is string => typeof value === "string" && value.length > 0 && value.length <= max;
const time = (value: unknown): value is number => Number.isSafeInteger(value) && (value as number) > 0;
const states: readonly string[] = ["starting", "live", "failed", "ended", "revoked", "expired"];
function parseLink(value: any): PlaybackLink {
  if (!value || !string(value.id, 128) || !string(value.machine_id) || !string(value.surface_id)
    || !["720p", "1080p"].includes(value.preset) || !states.includes(value.state)
    || !time(value.created_at) || !time(value.expires_at)
    || (value.revoked_at !== undefined && value.revoked_at !== null && !time(value.revoked_at))
    || (value.error !== undefined && value.error !== null && !string(value.error, 128))) throw new PlaybackError("Invalid playback link response.");
  return { id: value.id, machine_id: value.machine_id, surface_id: value.surface_id, preset: value.preset, state: value.state,
    created_at: value.created_at, expires_at: value.expires_at, ...(time(value.revoked_at) ? { revoked_at: value.revoked_at } : {}),
    ...(string(value.error, 128) ? { error: value.error } : {}) };
}

/** Same-origin playlist with exactly one view token; query strings are redacted by Workers. */
export function validPlaybackUrl(value: unknown, origin = location.origin): string | undefined {
  if (typeof value !== "string" || value.length > 2048) return;
  let url: URL;
  try { url = new URL(value); } catch { return; }
  const params = [...url.searchParams];
  if (url.origin !== origin || url.username || url.password || url.hash
    || !["https:", "http:"].includes(url.protocol)
    || !/^\/v1\/screen-playback\/sp_[0-9a-f]{32}\/index\.m3u8$/.test(url.pathname)
    || params.length !== 1 || params[0]?.[0] !== "token" || !/^nsv_[A-Za-z0-9_-]{43}$/.test(params[0][1])) return;
  return url.href;
}

const messages: Record<string, string> = {
  invalid_request: "The playback request was invalid. Refresh and try again.",
  unsupported: "This Hand needs an update before it can create playback links.",
  stale_generation: "The screen changed. Refresh and try again.",
  not_found: "This screen is no longer available.",
  host_unavailable: "The Hand is offline or not responding.",
  busy: "A stream is already running on this Hand. Stop it before creating another.",
  too_many_streams: "You already have the maximum number of active playback links. Stop one first.",
  operation_conflict: "This request changed while unconfirmed. Check active links before trying again.",
  unauthorized: "Sign in again to manage playback links.",
  forbidden: "This account cannot manage playback links for this Hand.",
  forbidden_origin: "This account cannot manage playback links for this Hand.",
};

async function send(method: string, suffix = "", body?: unknown, signal?: AbortSignal): Promise<any> {
  const write = method !== "GET";
  let response: Response;
  try {
    response = await fetch(path + suffix, {
      method, credentials: "same-origin", cache: "no-store", redirect: "error",
      signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(15_000)]) : AbortSignal.timeout(15_000),
      ...(body === undefined ? {} : { body: JSON.stringify(body), headers: { "content-type": "application/json" } }),
    });
  } catch {
    if (signal?.aborted) throw new PlaybackError("Request canceled.", write);
    throw new PlaybackError(write ? "No response was received. Check active links before trying again." : "Could not load playback links.", write);
  }
  let value: any;
  try { value = await response.json(); } catch { value = undefined; }
  if (!response.ok) {
    const code = string(value?.error, 64) ? value.error : "";
    // A server failure without a definitive error code may follow the write.
    const uncertain = write && (response.status >= 500 && code !== "host_unavailable");
    throw new PlaybackError(messages[code] ?? (uncertain ? "The request did not complete. Check active links before trying again." : "The playback request failed."), uncertain, code || undefined);
  }
  if (value === undefined) throw new PlaybackError(write ? "The response was unreadable. Check active links before trying again." : "Invalid playback link response.", write);
  return value;
}

export async function listPlaybackLinks(signal?: AbortSignal): Promise<readonly PlaybackLink[]> {
  const value = await send("GET", "", undefined, signal);
  if (!value || !Array.isArray(value.data) || value.data.length > 256) throw new PlaybackError("Invalid playback link response.");
  return value.data.map(parseLink);
}

/** Never retried automatically. Reuse the same operationId only for an explicit user retry. */
export async function createPlaybackLink(hand: RemoteHand, request: PlaybackRequest): Promise<PlaybackCreation> {
  if (!hand.playback) throw new PlaybackError("This Hand needs an update before it can create playback links.");
  const value = await send("POST", "", { operation_id: request.operationId, machine_id: hand.machine_id, surface_id: hand.id,
    generation: hand.generation, expires_in_seconds: request.expiresInSeconds, preset: request.preset });
  let link: PlaybackLink;
  try { link = parseLink(value); } catch { throw new PlaybackError("The response was unreadable. Check active links before trying again.", true); }
  if (value.url === undefined && value.url_available === false) return { link };
  const url = validPlaybackUrl(value.url);
  if (!url) throw new PlaybackError("The server returned an unexpected link. Stop it in active links before creating another.", true);
  return { link, url };
}

export async function revokePlaybackLink(id: string): Promise<void> {
  if (!string(id, 128)) throw new PlaybackError("Invalid playback link.");
  try { await send("DELETE", "/" + encodeURIComponent(id)); }
  catch (error) { if (!(error instanceof PlaybackError && error.code === "not_found")) throw error; }
}
