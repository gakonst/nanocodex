import { createHash, timingSafeEqual } from "node:crypto";
import { stageCalendarNotifications, drainCalendarNotifications } from "./calendar-notifications";
import { importCalendarEvents } from "./crm-meetings";

/** All provider calls must use an owner-scoped managed connector egress capability.
 * authorize must recheck the live owner/grant, including after provider I/O.
 * The public callback may only call receiveCalendarPush; it cannot supply owners,
 * connector IDs, URLs, events, or sync tokens. A private scheduled worker consumes
 * due sources with the corresponding owner's capability. */
export type CalendarPushOptions = {
  db: D1Database;
  ownerId: string;
  agentId: string;
  authorize(): void;
  fetch(request: Request): Promise<Response>;
  /** Trusted deployment configuration, never a request-provided URL. */
  callbackUrl: string;
  deliver?(id: string, input: string): Promise<"accepted" | "duplicate" | "busy">;
  enqueue?(sourceId: string, agentId: string): Promise<void>;
};
type Source = { notifications_initialized: number; id: string; owner_id: string; agent_id: string; window_from: number; window_to: number; rebuild_at: number; connection_id: string; calendar_id: string;
  enabled: number; sync_token: string | null; page_token: string | null; generation: string;
  dirty: number; check_at: number; renew_at: number; lease: string | null; lease_until: number };
const HOUR = 3600000;
const hash = (s: string) => createHash("sha256").update(s).digest("hex");
const opaque = (s: unknown): s is string => typeof s === "string" && s.length > 0 && s.length <= 8192 && !/[\u0000-\u001f\u007f]/.test(s);
const sourceId = (owner: string, connection: string, calendar: string) => hash(JSON.stringify([owner, connection, calendar]));
function authorize(o: CalendarPushOptions) { if (!o.ownerId) throw new Error("calendar_push_owner_required"); o.authorize(); }
function endpoint(s: Source) { return `https://www.googleapis.com/calendar/v3/calendars/${encodeURIComponent(s.calendar_id)}/events`; }
async function source(o: CalendarPushOptions, id: string): Promise<Source> {
  authorize(o);
  const s = await o.db.withSession("first-primary").prepare("SELECT * FROM crm_calendar_push_sources WHERE id=? AND owner_id=? AND agent_id=? AND enabled=1").bind(id, o.ownerId, o.agentId).first<Source>();
  if (!s) throw new Error("calendar_push_source_unavailable");
  return s;
}
async function provider(o: CalendarPushOptions, s: Source, url: string, body?: unknown, allowMissing = false): Promise<{ status: number; data: Record<string, unknown> }> {
  authorize(o);
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 30000);
  try {
    const response = await o.fetch(new Request(url, { method: body === undefined ? "GET" : "POST", redirect: "manual",
      headers: { "x-nanocodex-connector-connection": s.connection_id, accept: "application/json", ...(body === undefined ? {} : { "content-type": "application/json" }) },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: controller.signal }));
    authorize(o);
    if (response.status === 410 || (allowMissing && response.status === 404)) { await response.body?.cancel(); return { status: response.status, data: {} }; }
    if (!response.ok) { await response.body?.cancel(); throw new Error(`calendar_push_provider_${response.status}`); }
    const reader = response.body?.getReader();
    if (!reader) throw new Error("calendar_push_invalid_response");
    let bytes = 0; const chunks: Uint8Array[] = [];
    try {
      while (true) {
        controller.signal.throwIfAborted();
        const chunk = await reader.read();
        if (chunk.done) break;
        bytes += chunk.value.byteLength;
        if (bytes > 1048576) { await reader.cancel(); throw new Error("calendar_push_response_too_large"); }
        chunks.push(chunk.value);
      }
    } finally { reader.releaseLock(); }
    authorize(o);
    const joined = new Uint8Array(bytes); let offset = 0;
    for (const chunk of chunks) { joined.set(chunk, offset); offset += chunk.byteLength; }
    const data = JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(joined));
    if (!data || typeof data !== "object" || Array.isArray(data) || "error" in data) throw new Error("calendar_push_invalid_response");
    return { status: response.status, data };
  } finally { clearTimeout(timer); }
}
async function acquire(o: CalendarPushOptions, id: string): Promise<string | null> {
  authorize(o);
  const lease = crypto.randomUUID(); const now = Date.now();
  const row = await o.db.prepare("UPDATE crm_calendar_push_sources SET lease=?,lease_until=? WHERE id=? AND owner_id=? AND enabled=1 AND lease_until<? RETURNING id")
    .bind(lease, now + 120000, id, o.ownerId, now).first();
  return row ? lease : null;
}
async function fence(o: CalendarPushOptions, id: string, lease: string) {
  authorize(o);
  const row = await o.db.prepare("UPDATE crm_calendar_push_sources SET lease_until=? WHERE id=? AND owner_id=? AND enabled=1 AND lease=? AND lease_until>? RETURNING id")
    .bind(Date.now() + 120000, id, o.ownerId, lease, Date.now()).first();
  if (!row) throw new Error("calendar_push_lease_lost");
}
async function release(o: CalendarPushOptions, id: string, lease: string) {
  await o.db.prepare("UPDATE crm_calendar_push_sources SET lease=NULL,lease_until=0 WHERE id=? AND owner_id=? AND lease=?").bind(id, o.ownerId, lease).run();
}

/** Idempotent enable, followed by a renewable watch. No calendar event writes. */
export async function configureCalendarPush(o: CalendarPushOptions, input: { connection_id: string; calendar_id?: string }): Promise<{ id: string; enabled: true }> {
  authorize(o);
  const calendar = input.calendar_id ?? "primary";
  if (!/^[A-Za-z0-9_-]{43}$/.test(input.connection_id) || !opaque(calendar) || calendar.length > 1024 || !calendar.isWellFormed()) throw new Error("calendar_push_invalid_source");
  const callback = new URL(o.callbackUrl);
  if (callback.protocol !== "https:" || callback.username || callback.password || callback.hash || callback.search) throw new Error("calendar_push_invalid_callback");
  const id = sourceId(o.ownerId, input.connection_id, calendar);
  await o.db.prepare(`INSERT INTO crm_calendar_push_sources(id,owner_id,agent_id,connection_id,calendar_id,generation,window_from,window_to,rebuild_at) VALUES(?,?,?,?,?,?,?,?,?)
    ON CONFLICT(id) DO UPDATE SET enabled=1,dirty=1,check_at=0 WHERE crm_calendar_push_sources.agent_id=excluded.agent_id`).bind(id, o.ownerId, o.agentId, input.connection_id, calendar, crypto.randomUUID(), Date.now()-30*24*HOUR, Date.now()+14*24*HOUR, Date.now()+24*HOUR).run();
  await source(o,id);
  await o.enqueue?.(id,o.agentId);
  await renewCalendarPush(o, id);
  return { id, enabled: true };
}

/** Retain overlapping channels until their provider expiration; the new channel
 * is recorded before watch so an early sync is safely acknowledged. */
export async function renewCalendarPush(o: CalendarPushOptions, id: string): Promise<void> {
  const s = await source(o, id);
  if (s.renew_at > Date.now()) return;
  const lease = await acquire(o, id);
  if (!lease) return;
  try {
    const current = await source(o, id);
    if (current.renew_at > Date.now()) return;
    const channel = crypto.randomUUID(), token = `${crypto.randomUUID()}${crypto.randomUUID()}`;
    const expiration = Date.now() + 6 * 24 * HOUR;
    await o.db.prepare("INSERT INTO crm_calendar_push_channels(id,source_id,token_hash,expires_at) VALUES(?,?,?,?)").bind(channel, id, hash(token), expiration).run();
    // Do not retry this POST on an ambiguous transport failure. This pending
    // channel expires; later reconciliation registers a fresh unique channel.
    const { data } = await provider(o, current, `${endpoint(current)}/watch`, { id: channel, type: "web_hook", address: o.callbackUrl, token, expiration: String(expiration) });
    await fence(o, id, lease);
    const expires = Number(data.expiration);
    if (data.id !== channel || !opaque(data.resourceId) || !Number.isSafeInteger(expires) || expires <= Date.now()) throw new Error("calendar_push_invalid_watch");
    await o.db.batch([
      o.db.prepare("UPDATE crm_calendar_push_channels SET resource_id=?,expires_at=? WHERE id=?").bind(data.resourceId, expires, channel),
      o.db.prepare("UPDATE crm_calendar_push_sources SET renew_at=?,renewal_error=NULL,last_error=CASE WHEN last_error='watch_renewal_failed' THEN NULL ELSE last_error END WHERE id=? AND lease=?").bind(Math.max(Date.now() + 1000, expires - Math.min(HOUR, (expires - Date.now()) / 2)), id, lease),
      o.db.prepare("DELETE FROM crm_calendar_push_channels WHERE source_id=? AND expires_at<?").bind(id, Date.now()),
    ]);
  } catch (error) {
    // Renewal has its own durable retry clock. Pagination may continue every
    // second without retrying watch, and skipped renewals must retain the error.
    await o.db.prepare("UPDATE crm_calendar_push_sources SET renew_at=?,renewal_error='watch_renewal_failed',last_error='watch_renewal_failed' WHERE id=? AND owner_id=? AND enabled=1 AND lease=?")
      .bind(Date.now()+60000,id,o.ownerId,lease).run();
    throw error;
  } finally { await release(o, id, lease); }
}

/** Public HTTPS ingress. Hints carry no event body and grant no CRM authority. */
export async function receiveCalendarPush(db: D1Database, request: Request, enqueue?: (sourceId: string, agentId: string) => Promise<void>): Promise<Response> {
  if (request.method !== "POST") return new Response(null, { status: 405 });
  const id = request.headers.get("x-goog-channel-id"), token = request.headers.get("x-goog-channel-token");
  const resource = request.headers.get("x-goog-resource-id"), state = request.headers.get("x-goog-resource-state");
  const number = request.headers.get("x-goog-message-number");
  if (!id || id.length > 64 || !token || token.length > 256 || !resource || resource.length > 8192 || !number || !/^[1-9][0-9]{0,39}$/.test(number) || !["sync", "exists", "not_exists"].includes(state ?? "")) return new Response(null, { status: 400 });
  const row = await db.withSession("first-primary").prepare(`SELECT c.*,s.enabled,s.agent_id FROM crm_calendar_push_channels c JOIN crm_calendar_push_sources s ON s.id=c.source_id WHERE c.id=?`).bind(id)
    .first<{ source_id: string; agent_id: string; token_hash: string; resource_id: string | null; expires_at: number; enabled: number }>();
  if (!row || !row.enabled || row.expires_at <= Date.now() || !timingSafeEqual(new TextEncoder().encode(row.token_hash), new TextEncoder().encode(hash(token)))) return new Response(null, { status: 403 });
  // The watch response is authoritative for resource binding. Never learn it
  // from a callback, even one carrying the secret channel token.
  if (row.resource_id === null) return new Response(null, { status: state === "sync" ? 204 : 503 });
  if (row.resource_id !== resource) return new Response(null, { status: 403 });
  await db.prepare("UPDATE crm_calendar_push_sources SET dirty=dirty+1,check_at=0 WHERE id=? AND enabled=1").bind(row.source_id).run();
  await enqueue?.(row.source_id, row.agent_id);
  return new Response(null, { status: 204 });
}

/** Bounded pages, durable continuation, and periodic repair even without hints.
 * Cursor commits follow idempotent CRM imports: a crash replays the unfinished
 * page. New attendees enter the existing crm_research queue automatically. */
export async function reconcileCalendarPush(o: CalendarPushOptions, id: string, maxPages = 5, respectSchedule = false): Promise<{ complete: boolean; busy?: boolean; reset?: boolean }> {
  if (!Number.isInteger(maxPages) || maxPages < 1 || maxPages > 5) throw new Error("calendar_push_invalid_budget");
  await source(o, id);
  const lease = await acquire(o, id);
  if (!lease) return { complete: false, busy: true };
  try {
    let s = await source(o, id);
    const drain = () => o.deliver ? drainCalendarNotifications(o.db,id,()=>fence(o,id,lease),o.deliver) : Promise.resolve(true);
    const drained = await drain();
    // Delivery retries must not turn a clean sync token into one provider poll
    // per second. Callback hints and scheduled repair still bypass this gate.
    if (respectSchedule && !s.dirty && !s.page_token && s.check_at > Date.now() && s.rebuild_at > Date.now()) return {complete:drained};
    if (!s.page_token && s.rebuild_at <= Date.now()) {
      await o.db.prepare("UPDATE crm_calendar_push_sources SET sync_token=NULL,generation=?,window_from=?,window_to=?,rebuild_at=? WHERE id=? AND lease=?")
        .bind(crypto.randomUUID(), Date.now()-30*24*HOUR, Date.now()+14*24*HOUR, Date.now()+24*HOUR, id, lease).run();
      s = await source(o, id);
    }
    const dirty = s.dirty;
    const tokens = new Set<string>();
    if (s.page_token) tokens.add(s.page_token);
    for (let page = 0; page < maxPages; page++) {
      const url = new URL(endpoint(s));
      url.searchParams.set("singleEvents", "true");
      url.searchParams.set("showDeleted", "true");
      url.searchParams.set("maxResults", "100");
      if (s.sync_token) url.searchParams.set("syncToken", s.sync_token);
      else { url.searchParams.set("timeMin", new Date(s.window_from).toISOString()); url.searchParams.set("timeMax", new Date(s.window_to).toISOString()); }
      if (s.page_token) url.searchParams.set("pageToken", s.page_token);
      const { status, data } = await provider(o, s, url.href);
      await fence(o, id, lease);
      if (status === 410) {
        await o.db.prepare("UPDATE crm_calendar_push_sources SET sync_token=NULL,page_token=NULL,generation=?,check_at=0 WHERE id=? AND lease=?").bind(crypto.randomUUID(), id, lease).run();
        return { complete: false, reset: true };
      }
      const items = data.items ?? (data.kind === "calendar#events" ? [] : undefined);
      const next = data.nextPageToken, sync = data.nextSyncToken;
      if (!Array.isArray(items) || items.length > 100 || (next !== undefined && (!opaque(next) || tokens.has(next))) ||
        (next === undefined && !opaque(sync)) || (next !== undefined && sync !== undefined)) throw new Error("calendar_push_invalid_page");
      await importCalendarEvents(o.db, o.ownerId, { connection_id: s.connection_id, calendar_id: s.calendar_id, events: items });
      await fence(o, id, lease);
      await stageCalendarNotifications(o.db,s,items,()=>fence(o,id,lease));
      const statements = items.map(item => o.db.prepare(`INSERT INTO crm_calendar_push_seen(source_id,event_id,generation) VALUES(?,?,?)
        ON CONFLICT(source_id,event_id) DO UPDATE SET generation=excluded.generation`).bind(id, (item as { id: string }).id, s.generation));
      if (statements.length) await o.db.batch(statements);
      if (next === undefined && !s.sync_token) {
        // A bounded snapshot can omit a moved event. Resolve each missing known
        // ID rather than treating absence as deletion. Repair is bounded/replayed.
        const missing = (await o.db.prepare(`SELECT event_id FROM crm_meetings WHERE owner_id=? AND connection_id=? AND calendar_id=?
          AND end_ms>? AND start_ms<? AND NOT EXISTS (SELECT 1 FROM crm_calendar_push_seen v WHERE v.source_id=? AND v.event_id=crm_meetings.event_id AND v.generation=?) LIMIT 20`)
          .bind(o.ownerId,s.connection_id,s.calendar_id,s.window_from,s.window_to,id,s.generation).all<{event_id:string}>()).results;
        for (const absent of missing) {
          const lookup = await provider(o,s,`${endpoint(s)}/${encodeURIComponent(absent.event_id)}`,undefined,true);
          await fence(o,id,lease);
          const event = lookup.status === 404 || lookup.status === 410 ? {id:absent.event_id,status:"cancelled"} : lookup.data;
          if (event.id !== absent.event_id) throw new Error("calendar_push_invalid_event");
          await importCalendarEvents(o.db,o.ownerId,{connection_id:s.connection_id,calendar_id:s.calendar_id,events:[event]});
          await fence(o,id,lease);
          await stageCalendarNotifications(o.db,s,[event],()=>fence(o,id,lease));
          await o.db.prepare(`INSERT INTO crm_calendar_push_seen(source_id,event_id,generation) VALUES(?,?,?) ON CONFLICT(source_id,event_id) DO UPDATE SET generation=excluded.generation`).bind(id,absent.event_id,s.generation).run();
        }
        if (missing.length === 20) return {complete:false};
      }
      statements.length = 0;
      statements.push(o.db.prepare(`UPDATE crm_calendar_push_sources SET notifications_initialized=CASE WHEN ?=1 THEN 1 ELSE notifications_initialized END,page_token=?,sync_token=?,check_at=?,dirty=CASE WHEN dirty=? AND ?=1 THEN 0 ELSE dirty END
        WHERE id=? AND lease=?`).bind(Number(next === undefined), next ?? null, next === undefined ? sync : s.sync_token,
        next === undefined ? Date.now() + HOUR : 0, dirty, Number(next === undefined), id, lease));
      await o.db.batch(statements);
      if (next === undefined) return { complete: drained && await drain() };
      tokens.add(next as string); s = { ...s, page_token: next as string };
    }
    return { complete: false };
  } finally { await release(o, id, lease); }
}

/** Private scheduler discovery only; rows never go to public callback responses. */
export async function dueCalendarPushSources(db: D1Database, limit = 50): Promise<{ id: string; owner_id: string }[]> {
  if (!Number.isInteger(limit) || limit < 1 || limit > 100) throw new Error("calendar_push_invalid_budget");
  return (await db.withSession("first-primary").prepare(`SELECT id,owner_id FROM crm_calendar_push_sources
    WHERE enabled=1 AND lease_until<? AND (dirty>0 OR check_at<=? OR renew_at<=?) ORDER BY check_at,id LIMIT ?`)
    .bind(Date.now(), Date.now(), Date.now(), limit).all<{ id: string; owner_id: string }>()).results;
}
export async function disableCalendarPush(o: CalendarPushOptions, id: string): Promise<void> {
  authorize(o);
  // Immediate local revocation. Existing provider channels expire naturally;
  // avoiding stop requests also avoids depending on calendar write scopes.
  await o.db.prepare("UPDATE crm_calendar_push_sources SET enabled=0 WHERE id=? AND owner_id=? AND agent_id=?").bind(id, o.ownerId, o.agentId).run();
}
