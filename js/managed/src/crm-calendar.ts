import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import { CrmError } from "./crm";
import { importCalendarEvents } from "./crm-meetings";

type Options = {
  db: D1Database;
  ownerId: string;
  /** The host supplies managed connector egress, never unrestricted fetch. */
  fetch(request: Request): Promise<Response>;
  authorize(): void;
  signal?: AbortSignal;
};
type Progress = { pages: number; events: number; imported: number; skipped: number; people_created: number; limited_events: number; unresolved: number };
type Query = { connection_id: string; calendar_id: string; from: string; to: string };
type State = Query & { page_token: string | null; progress: Progress };
type Failure = { code: string; message: string; status?: number; retry_after_seconds?: number };
type ImportResult = { imported: number; skipped: number; people_created: number; limited_events: number; unresolved: unknown[] };
const progressKeys = ["pages", "events", "imported", "skipped", "people_created", "limited_events", "unresolved"] as const;
const stateKeys = ["connection_id", "calendar_id", "from", "to", "page_token", "progress"] as const;
const MAX_BYTES = 1024 * 1024;
const PAGE_TIMEOUT_MS = 30_000;
const MAX_PAGES = 5;

class ProviderFailure extends Error {
  constructor(readonly failure: Failure) { super(failure.message); this.name = "CalendarProviderError"; }
}
function fail(code: string, message: string, status?: number): never {
  throw new ProviderFailure({ code, message, ...(status === undefined ? {} : { status }) });
}
function invalid(message: string): never { throw new CrmError("invalid_input", message); }
function object(value: unknown, allowed?: readonly string[]): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value) ||
      ![Object.prototype, null].includes(Object.getPrototypeOf(value)) || Object.getOwnPropertySymbols(value).length) invalid("Calendar input must be an object.");
  if (allowed && Object.keys(value).some(key => !allowed.includes(key))) invalid("Unknown Calendar input field.");
  return value as Record<string, unknown>;
}
function text(value: unknown, field: string, max: number): string {
  if (typeof value !== "string" || !value.trim() || value.length > max || /[\u0000-\u001f\u007f]/.test(value)) invalid(`Invalid ${field}.`);
  // Reject unpaired surrogates before URL/UTF-8 encoding can change identity.
  if (!value.isWellFormed()) invalid(`Invalid ${field}.`);
  return value;
}
function connection(value: unknown): string {
  if (typeof value !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(value)) invalid("connection_id must be an opaque 43-character connector ID.");
  return value;
}
function timestamp(value: unknown, field: string): string {
  const result = text(value, field, 64);
  const match = /^(\d{4})-(\d{2})-(\d{2})[Tt](\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(?:[Zz]|([+-])(\d{2}):(\d{2}))$/.exec(result);
  if (!match) invalid(`${field} must be an RFC3339 timestamp with a timezone.`);
  const [, year, month, day, hour, minute, second, , offsetHour, offsetMinute] = match;
  const y = Number(year), m = Number(month), d = Number(day);
  const leap = y % 4 === 0 && (y % 100 !== 0 || y % 400 === 0);
  const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  if (m < 1 || m > 12 || d < 1 || d > days[m - 1] || Number(hour) > 23 || Number(minute) > 59 || Number(second) > 59 ||
      Number(offsetHour ?? 0) > 23 || Number(offsetMinute ?? 0) > 59 || !Number.isFinite(Date.parse(result))) invalid(`Invalid ${field}.`);
  return result;
}
function query(value: Record<string, unknown>): Query {
  const result = { connection_id: connection(value.connection_id), calendar_id: text(value.calendar_id, "calendar_id", 1024), from: timestamp(value.from, "from"), to: timestamp(value.to, "to") };
  if (Date.parse(result.from) >= Date.parse(result.to)) invalid("from must be earlier than to.");
  return result;
}
function cursorScope(ownerId: string, state: State): string {
  // Query binding follows CRM's account-scoped pagination contract. No cursor
  // field is ever interpreted as an origin, path, authorization, or account ID.
  return createHash("sha256").update(JSON.stringify(["crm-calendar-v1", ownerId, state])).digest("base64url");
}
function encodeCursor(ownerId: string, state: State): string {
  return Buffer.from(JSON.stringify({ v: 1, state, scope: cursorScope(ownerId, state) }), "utf8").toString("base64url");
}
function decodeCursor(ownerId: string, value: unknown): State {
  if (typeof value !== "string" || !/^[A-Za-z0-9_-]{1,24000}$/.test(value)) invalid("Invalid Calendar cursor.");
  try {
    const decoded = object(JSON.parse(Buffer.from(value, "base64url").toString("utf8")), ["v", "state", "scope"]);
    const state = object(decoded.state, stateKeys);
    const p = object(state.progress, progressKeys);
    if (Object.keys(p).length !== progressKeys.length || progressKeys.some(key => typeof p[key] !== "number" || !Number.isSafeInteger(p[key]) || (p[key] as number) < 0)) invalid("Invalid Calendar cursor progress.");
    const progress = Object.fromEntries(progressKeys.map(key => [key, p[key]])) as Progress;
    if (progress.events > progress.pages * 100 || progress.imported + progress.skipped !== progress.events || progress.limited_events > progress.events) invalid("Invalid Calendar cursor progress.");
    const result: State = { ...query(state), page_token: state.page_token === null ? null : text(state.page_token, "cursor page token", 8192), progress };
    if (decoded.v !== 1 || decoded.scope !== cursorScope(ownerId, result)) invalid("Invalid Calendar cursor scope.");
    return result;
  } catch { return invalid("Invalid Calendar cursor for this account and query."); }
}
function inputState(ownerId: string, value: unknown): State {
  text(ownerId, "authenticated owner", 512);
  const args = object(value, ["connection_id", "calendar_id", "from", "to", "cursor"]);
  const connectionId = connection(args.connection_id);
  const calendarId = Object.hasOwn(args, "calendar_id") ? text(args.calendar_id, "calendar_id", 1024) : "primary";
  if (Object.hasOwn(args, "cursor")) {
    const state = decodeCursor(ownerId, args.cursor);
    if (state.connection_id !== connectionId || state.calendar_id !== calendarId ||
        (Object.hasOwn(args, "from") && timestamp(args.from, "from") !== state.from) ||
        (Object.hasOwn(args, "to") && timestamp(args.to, "to") !== state.to)) invalid("Calendar cursor does not match connection, calendar, or interval.");
    return state;
  }
  const now = Date.now();
  return { ...query({ connection_id: connectionId, calendar_id: calendarId,
    from: Object.hasOwn(args, "from") ? args.from : new Date(now - 30 * 86400_000).toISOString(),
    to: Object.hasOwn(args, "to") ? args.to : new Date(now + 14 * 86400_000).toISOString(),
  }), page_token: null, progress: { pages: 0, events: 0, imported: 0, skipped: 0, people_created: 0, limited_events: 0, unresolved: 0 } };
}
function result(ownerId: string, state: State, complete: boolean, error?: Failure) {
  return { connection_id: state.connection_id, calendar_id: state.calendar_id, from: state.from, to: state.to,
    complete, limited: state.progress.limited_events > 0, next_cursor: complete ? null : encodeCursor(ownerId, state), progress: { ...state.progress }, ...(error ? { error } : {}) };
}
function authorize(options: Options): void { options.signal?.throwIfAborted(); options.authorize(); }
function url(state: State): URL {
  const endpoint = new URL(`https://www.googleapis.com/calendar/v3/calendars/${encodeURIComponent(state.calendar_id)}/events`);
  endpoint.searchParams.set("singleEvents", "true");
  endpoint.searchParams.set("showDeleted", "true");
  endpoint.searchParams.set("maxResults", "100");
  endpoint.searchParams.set("timeMin", state.from);
  endpoint.searchParams.set("timeMax", state.to);
  if (state.page_token !== null) endpoint.searchParams.set("pageToken", state.page_token);
  return endpoint;
}
async function boundedBody(response: Response, signal: AbortSignal): Promise<string> {
  const length = response.headers.get("content-length");
  if (length !== null && /^\d+$/.test(length) && Number(length) > MAX_BYTES) {
    void response.body?.cancel().catch(() => {});
    fail("response_too_large", "Calendar response exceeded the 1 MiB limit.", response.status);
  }
  if (!response.body) return "";
  const reader = response.body.getReader();
  const cancel = () => { void reader.cancel().catch(() => {}); };
  signal.addEventListener("abort", cancel, { once: true });
  const chunks: Uint8Array[] = [];
  let bytes = 0;
  try {
    while (true) {
      signal.throwIfAborted();
      const chunk = await reader.read();
      signal.throwIfAborted();
      if (chunk.done) break;
      bytes += chunk.value.byteLength;
      if (bytes > MAX_BYTES) { cancel(); fail("response_too_large", "Calendar response exceeded the 1 MiB limit.", response.status); }
      chunks.push(chunk.value);
    }
    const body = new Uint8Array(bytes);
    let offset = 0;
    for (const chunk of chunks) { body.set(chunk, offset); offset += chunk.byteLength; }
    try { return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(body); }
    catch { return fail("invalid_response", "Calendar returned an invalid UTF-8 response.", response.status); }
  } finally { signal.removeEventListener("abort", cancel); reader.releaseLock(); }
}
function providerError(response: Response, body: unknown): ProviderFailure {
  // Inspect only structured reason codes; never return provider messages, URLs,
  // invite data, headers, or connector tokens in diagnostic details.
  const error = body && typeof body === "object" && "error" in body ? (body as { error: unknown }).error : undefined;
  const reasons = new Set<string>();
  if (error && typeof error === "object") {
    for (const key of ["errors", "details"] as const) {
      const list = (error as Record<string, unknown>)[key];
      if (Array.isArray(list)) for (const item of list) {
        if (item && typeof item === "object" && typeof item.reason === "string") reasons.add(item.reason);
      }
    }
  }
  let failure: Failure;
  if (reasons.has("SERVICE_DISABLED") || reasons.has("accessNotConfigured")) {
    failure = { code: "calendar_api_disabled", message: "Google Calendar API is disabled for this connector's Google Cloud project. Enable the Calendar API in that project, then resume this sync." };
  } else if (response.status === 429 || reasons.has("rateLimitExceeded") || reasons.has("userRateLimitExceeded") || reasons.has("quotaExceeded") || reasons.has("RATE_LIMIT_EXCEEDED")) {
    failure = { code: "rate_limited", message: "Google Calendar rate limit reached. Wait before resuming this sync." };
    const retry = response.headers.get("retry-after");
    if (retry && /^\d{1,7}$/.test(retry)) failure.retry_after_seconds = Number(retry);
  } else if (response.status === 401 || reasons.has("ACCESS_TOKEN_SCOPE_INSUFFICIENT") || reasons.has("insufficientPermissions") || reasons.has("authError")) {
    failure = { code: "reconnect_required", message: "Reconnect the selected Google Workspace account with Calendar read access, then resume this sync." };
  } else {
    failure = { code: "provider_error", message: response.status === 403 ? "Google Calendar denied access to this calendar. Check the selected connection and calendar permissions, then resume this sync." : `Google Calendar request failed (HTTP ${response.status}). Resume this sync after resolving the provider error.` };
  }
  return new ProviderFailure({ ...failure, status: response.status });
}
async function fetchPage(options: Options, state: State): Promise<{ items: unknown[]; next: string | null }> {
  const controller = new AbortController();
  let rejectAbort: (reason: unknown) => void = () => {};
  const aborted = new Promise<never>((_, reject) => { rejectAbort = reject; });
  const stop = (reason: unknown) => { rejectAbort(reason); controller.abort(reason); };
  const callerAbort = () => stop(options.signal?.reason ?? new Error("Calendar sync cancelled."));
  const timer = setTimeout(() => stop(new ProviderFailure({ code: "provider_timeout", message: "Google Calendar request timed out. Resume this sync to retry the unfinished page." })), PAGE_TIMEOUT_MS);
  options.signal?.addEventListener("abort", callerAbort, { once: true });
  try {
    authorize(options);
    return await Promise.race([aborted, (async () => {
      const response = await options.fetch(new Request(url(state), { method: "GET", redirect: "manual", headers: { "x-nanocodex-connector-connection": state.connection_id, accept: "application/json" }, signal: controller.signal }));
      controller.signal.throwIfAborted();
      if (response.status >= 300 && response.status < 400) {
        void response.body?.cancel().catch(() => {});
        fail("redirect_rejected", "Google Calendar returned an unexpected redirect.", response.status);
      }
      const body = await boundedBody(response, controller.signal);
      let parsed: unknown;
      try { parsed = JSON.parse(body); } catch {
        if (!response.ok) throw providerError(response, null);
        fail("invalid_response", "Google Calendar returned invalid JSON.", response.status);
      }
      if (!response.ok) throw providerError(response, parsed);
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed) || "error" in parsed) fail("invalid_response", "Google Calendar returned an invalid event page.", response.status);
      const data = parsed as Record<string, unknown>;
      const items = data.items === undefined && data.kind === "calendar#events" ? [] : data.items;
      if (!Array.isArray(items) || items.length > 100) fail("invalid_response", "Google Calendar returned an invalid event page.", response.status);
      // Importer validates every raw event before its transactional D1 write.
      let next: string | null = null;
      if (data.nextPageToken !== undefined) {
        try { next = text(data.nextPageToken, "provider page token", 8192); }
        catch { fail("invalid_response", "Google Calendar returned an invalid page token.", response.status); }
      }
      return { items, next };
    })()]);
  } catch (error) {
    options.signal?.throwIfAborted();
    if (error instanceof ProviderFailure) throw error;
    // Authorization errors retain their meaning. Provider transport failures do
    // not leak their message (which may contain URLs or provider response data).
    authorize(options);
    return fail("provider_error", "Google Calendar could not be reached. Resume this sync to retry the unfinished page.");
  } finally {
    clearTimeout(timer);
    options.signal?.removeEventListener("abort", callerAbort);
  }
}

/** Bounded Calendar reads with resumable, account/query-scoped progress. */
export async function syncCrmCalendar(options: Options, input: unknown): Promise<unknown> {
  authorize(options);
  const state = inputState(options.ownerId, input);
  const tokens = new Set<string>();
  if (state.page_token !== null) tokens.add(state.page_token);
  for (let pageNumber = 0; pageNumber < MAX_PAGES; pageNumber++) {
    let page: { items: unknown[]; next: string | null };
    try { page = await fetchPage(options, state); }
    catch (error) {
      authorize(options);
      if (error instanceof ProviderFailure) return result(options.ownerId, state, false, error.failure);
      throw error;
    }
    authorize(options);
    if (page.next !== null && tokens.has(page.next)) return result(options.ownerId, state, false, { code: "invalid_response", message: "Google Calendar repeated a page token. Resume the unfinished page or start a new sync." });
    let imported: ImportResult;
    try { imported = await importCalendarEvents(options.db, options.ownerId, { connection_id: state.connection_id, calendar_id: state.calendar_id, events: page.items }) as ImportResult; }
    catch (error) {
      // Malformed provider events are not malformed user input. Keep the failed
      // page available for retry, with no provider-controlled exception details.
      if (error instanceof CrmError && error.code === "invalid_input") return result(options.ownerId, state, false, { code: "invalid_response", message: "Google Calendar returned invalid event data. The unfinished page was not imported." });
      throw error;
    }
    state.progress.pages++;
    state.progress.events += page.items.length;
    state.progress.imported += imported.imported;
    state.progress.skipped += imported.skipped;
    state.progress.people_created += imported.people_created;
    state.progress.limited_events += imported.limited_events;
    state.progress.unresolved += imported.unresolved.length;
    state.page_token = page.next;
    if (page.next === null) return result(options.ownerId, state, true);
    tokens.add(page.next);
  }
  return result(options.ownerId, state, false);
}
