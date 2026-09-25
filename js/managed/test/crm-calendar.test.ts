import { applyD1Migrations, env } from "cloudflare:test";
import { afterEach, beforeAll, expect, it, vi } from "vitest";
import { syncCrmCalendar } from "../src/crm-calendar";

// Failure modes at the egress + persistence boundary: pagination may outlive the
// default interval, stop partway through, or loop; cursors must not cross owners
// or queries; repeated recurring occurrences must stay distinct and idempotent;
// authority can disappear during fetch/body I/O; a provider may redirect, hang,
// exceed the byte budget, send malformed data, or deny API/scope/quota access.
// Controlled provider responses exercise those cases against migrated workerd D1.
const bindings = env as unknown as { NANOCODEX_CRM: D1Database; CRM_MIGRATIONS: Parameters<typeof applyD1Migrations>[1] };
const db = bindings.NANOCODEX_CRM;
const connection = "C".repeat(43);
const interval = { from: "2026-09-01T00:00:00Z", to: "2026-11-01T00:00:00Z" };
const owner = () => `calendar-${crypto.randomUUID()}`;
const event = (id: string, extra: Record<string, unknown> = {}) => ({
  id, status: "confirmed", summary: "Synthetic review", description: "Private invite content must not be echoed",
  start: { dateTime: "2026-09-25T10:00:00Z" }, end: { dateTime: "2026-09-25T11:00:00Z" },
  updated: "2026-09-25T09:00:00Z", organizer: { email: "organizer@example.test" },
  attendees: [{ email: "guest@example.test", displayName: "Synthetic Guest" }], ...extra,
});
const page = (items: unknown[] = [], nextPageToken?: string) => Response.json({ kind: "calendar#events", items, ...(nextPageToken ? { nextPageToken } : {}) });
const count = async (ownerId: string) => (await db.prepare("SELECT count(*) AS count FROM crm_meetings WHERE owner_id = ?").bind(ownerId).first<{ count: number }>())!.count;
const run = (ownerId: string, fetch: (request: Request) => Promise<Response>, input: unknown = { connection_id: connection, ...interval }, extra: { authorize?: () => void; signal?: AbortSignal } = {}): Promise<any> =>
  syncCrmCalendar({ db, ownerId, fetch, authorize: () => {}, ...extra }, input);
beforeAll(async () => { await applyD1Migrations(db, bindings.CRM_MIGRATIONS); });
afterEach(() => { vi.restoreAllMocks(); vi.useRealTimers(); });

it("imports recurring occurrences through fixed GET egress and idempotently resyncs cancellations", async () => {
  const account = owner();
  const requests: Request[] = [];
  const first = event("series_20260925T100000Z", { recurringEventId: "series", iCalUID: "same-series@example.test", originalStartTime: { dateTime: "2026-09-25T10:00:00Z" } });
  const second = event("series_20260926T100000Z", { recurringEventId: "series", iCalUID: "same-series@example.test", originalStartTime: { dateTime: "2026-09-26T10:00:00Z" } });
  const calendarId = "team/calendar+private@example.test";
  const fetch = async (request: Request) => { requests.push(request); return page([first, second]); };
  const input = { connection_id: connection, calendar_id: calendarId, ...interval };
  const imported = await run(account, fetch, input);
  expect(imported).toMatchObject({ complete: true, next_cursor: null, connection_id: connection, calendar_id: calendarId, progress: { pages: 1, events: 2 } });
  expect(JSON.stringify(imported)).not.toContain(first.description);
  expect(JSON.stringify(imported)).not.toContain("guest@example.test");
  expect(await count(account)).toBe(2);
  expect((await run(account, fetch, input)).complete).toBe(true);
  expect(await count(account)).toBe(2);
  await run(account, async () => page([{ id: first.id, status: "cancelled", updated: "2026-09-26T09:00:00Z" }]), input);
  expect(await count(account)).toBe(2);
  const request = requests[0];
  const url = new URL(request.url);
  expect(request.method).toBe("GET");
  expect(request.redirect).toBe("manual");
  expect(request.headers.get("x-nanocodex-connector-connection")).toBe(connection);
  expect(request.body).toBeNull();
  expect(url.origin).toBe("https://www.googleapis.com");
  expect(url.pathname).toBe(`/calendar/v3/calendars/${encodeURIComponent(calendarId)}/events`);
  expect(Object.fromEntries(url.searchParams)).toEqual({ singleEvents: "true", showDeleted: "true", maxResults: "100", timeMin: interval.from, timeMax: interval.to });
});

it("caps a call at five pages, preserves default dates across continuation, and scopes cursors", async () => {
  const account = owner();
  const fixedNow = Date.parse("2026-09-25T12:30:00Z");
  const now = vi.spyOn(Date, "now").mockReturnValue(fixedNow);
  const requests: URL[] = [];
  const fetch = async (request: Request) => {
    const url = new URL(request.url); requests.push(url);
    const n = Number(url.searchParams.get("pageToken") ?? "0");
    return page(n === 2 ? [] : [event(`page-${n}`)], n < 6 ? `${n + 1}` : undefined);
  };
  const first = await run(account, fetch, { connection_id: connection });
  expect(first).toMatchObject({ complete: false, from: "2026-08-26T12:30:00.000Z", to: "2026-10-09T12:30:00.000Z", next_cursor: expect.any(String), progress: { pages: 5, events: 4 } });
  expect(requests).toHaveLength(5);
  const noFetch = vi.fn(async () => page());
  for (const [scopeOwner, input] of [
    [owner(), { connection_id: connection, cursor: first.next_cursor }],
    [account, { connection_id: "D".repeat(43), cursor: first.next_cursor }],
    [account, { connection_id: connection, calendar_id: "other", cursor: first.next_cursor }],
    [account, { connection_id: connection, from: "2026-01-01T00:00:00Z", cursor: first.next_cursor }],
    [account, { connection_id: connection, to: "2026-12-01T00:00:00Z", cursor: first.next_cursor }],
  ] as const) await expect(run(scopeOwner, noFetch, input)).rejects.toMatchObject({ code: "invalid_input" });
  expect(noFetch).not.toHaveBeenCalled();
  now.mockReturnValue(fixedNow + 7 * 86400_000);
  const last = await run(account, fetch, { connection_id: connection, cursor: first.next_cursor });
  expect(last).toMatchObject({ complete: true, next_cursor: null, from: first.from, to: first.to, progress: { pages: 7, events: 6 } });
  for (const request of requests) {
    expect(request.searchParams.get("timeMin")).toBe(first.from);
    expect(request.searchParams.get("timeMax")).toBe(first.to);
  }
  expect(await count(account)).toBe(6);
});

it("reports partial progress and resumes exactly the failed page without losing already imported events", async () => {
  const account = owner();
  const response = await run(account, async request => new URL(request.url).searchParams.has("pageToken")
    ? Response.json({ error: { message: "Private invite content", errors: [{ reason: "rateLimitExceeded" }] } }, { status: 429, headers: { "retry-after": "60" } })
    : page([event("first")], "second"));
  expect(response).toMatchObject({ complete: false, next_cursor: expect.any(String), progress: { pages: 1, events: 1 }, error: { code: "rate_limited", status: 429, retry_after_seconds: 60 } });
  expect(JSON.stringify(response)).not.toContain("Private invite content");
  expect(await count(account)).toBe(1);
  const last = await run(account, async request => {
    expect(new URL(request.url).searchParams.get("pageToken")).toBe("second");
    return page([event("second")]);
  }, { connection_id: connection, cursor: response.next_cursor });
  expect(last).toMatchObject({ complete: true, progress: { pages: 2, events: 2 } });
  expect(await count(account)).toBe(2);
});

it.each([
  [403, { details: [{ reason: "SERVICE_DISABLED", metadata: { service: "calendar-json.googleapis.com" } }] }, "calendar_api_disabled"],
  [403, { details: [{ reason: "ACCESS_TOKEN_SCOPE_INSUFFICIENT" }] }, "reconnect_required"],
  [401, { errors: [{ reason: "authError" }] }, "reconnect_required"],
  [403, { errors: [{ reason: "userRateLimitExceeded" }] }, "rate_limited"],
  [503, {}, "provider_error"],
])("returns sanitized incomplete failure for provider status %s (%s)", async (status, details, code) => {
  const account = owner();
  const result = await run(account, async () => Response.json({ error: { ...details, message: "PRIVATE INVITE AND TOKEN" } }, { status }));
  expect(result).toMatchObject({ complete: false, next_cursor: expect.any(String), progress: { pages: 0, events: 0 }, error: { code, status } });
  expect(JSON.stringify(result)).not.toContain("PRIVATE INVITE AND TOKEN");
  expect(await count(account)).toBe(0);
});

it("rejects malformed tool input and cursor URL injection before fetching", async () => {
  const fetch = vi.fn(async () => page());
  for (const input of [null, [], {}, { connection_id: "short" }, { connection_id: connection, owner_id: "other" },
    { connection_id: connection, calendar_id: "" }, { connection_id: connection, from: "2026-09-25" },
    { connection_id: connection, from: "2026-02-30T10:00:00Z" },
    { connection_id: connection, from: interval.to, to: interval.from },
    { connection_id: connection, cursor: "https://attacker.example/" },
    { connection_id: connection, cursor: btoa(JSON.stringify({ url: "https://attacker.example" })).replace(/=+$/, "") },
  ]) await expect(run(owner(), fetch, input)).rejects.toMatchObject({ code: "invalid_input" });
  expect(fetch).not.toHaveBeenCalled();
});

it("rechecks authority after provider body I/O and honors caller cancellation before D1 writes", async () => {
  const account = owner();
  let allowed = true;
  const authorize = () => { if (!allowed) throw new Error("authorization revoked"); };
  const payload = new TextEncoder().encode(JSON.stringify({ items: [event("forbidden")] }));
  const result = run(account, async () => new Response(new ReadableStream({ pull(controller) { allowed = false; controller.enqueue(payload); controller.close(); } })), undefined, { authorize });
  await expect(result).rejects.toThrow(/revoked/);
  expect(await count(account)).toBe(0);
  const noFetch = vi.fn(async () => page());
  await expect(run(account, noFetch, undefined, { authorize })).rejects.toThrow(/revoked/);
  expect(noFetch).not.toHaveBeenCalled();
  const controller = new AbortController();
  const aborted = run(account, async request => { controller.abort(); expect(request.signal.aborted).toBe(true); return page([event("aborted")]); }, undefined, { signal: controller.signal });
  await expect(aborted).rejects.toThrow();
  await expect(run(account, noFetch, undefined, { signal: controller.signal })).rejects.toThrow();
  expect(noFetch).not.toHaveBeenCalled();
  expect(await count(account)).toBe(0);
});

it("rechecks authority before every subsequent provider request", async () => {
  const account = owner();
  let requests = 0;
  let checks = 0;
  const authorize = () => { checks++; if (requests === 1 && checks >= 4) throw new Error("authorization revoked"); };
  await expect(run(account, async () => { requests++; return page([event("authorized-first")], "more"); }, undefined, { authorize })).rejects.toThrow(/revoked/);
  expect(requests).toBe(1);
});

it("bounds streamed bytes and rejects redirects, malformed pages and provider loops without claiming completion", async () => {
  const scenarios: [() => Response, string][] = [
    [() => new Response(null, { status: 302, headers: { location: "https://attacker.example/private" } }), "redirect_rejected"],
    [() => new Response("{}", { headers: { "content-length": String(1024 * 1024 + 1) } }), "response_too_large"],
    [() => new Response(new ReadableStream({ start(controller) { controller.enqueue(new Uint8Array(700_000)); controller.enqueue(new Uint8Array(400_000)); controller.close(); } })), "response_too_large"],
    [() => new Response("not JSON"), "invalid_response"],
    [() => Response.json({ items: "not-an-array" }), "invalid_response"],
    [() => page([event("valid"), { status: "confirmed" }]), "invalid_response"],
  ];
  for (const [response, code] of scenarios) {
    const account = owner();
    const result = await run(account, async () => response());
    expect(result).toMatchObject({ complete: false, error: { code }, progress: { pages: 0, events: 0 } });
    expect(await count(account)).toBe(0);
  }
  const account = owner();
  const loop = await run(account, async () => page([event("loop")], "same"));
  expect(loop).toMatchObject({ complete: false, error: { code: "invalid_response" }, progress: { pages: 1, events: 1 } });
  expect(await count(account)).toBe(1);
});

it("aborts timed-out fetch and body reads with resumable incomplete progress", async () => {
  for (const stalledBody of [false, true]) {
    const account = owner();
    let requestSignal: AbortSignal | undefined;
    vi.useFakeTimers();
    const pending = run(account, async request => {
      requestSignal = request.signal;
      return stalledBody ? new Response(new ReadableStream({ start() {} })) : await new Promise<Response>(() => {});
    });
    // Let cursor hashing and request creation settle without advancing D1 timers.
    for (let n = 0; n < 20 && !requestSignal; n++) await Promise.resolve();
    await vi.advanceTimersByTimeAsync(30_001);
    const result = await pending;
    vi.useRealTimers();
    expect(requestSignal?.aborted).toBe(true);
    expect(result).toMatchObject({ complete: false, next_cursor: expect.any(String), error: { code: "provider_timeout" }, progress: { pages: 0, events: 0 } });
    expect(await count(account)).toBe(0);
  }
});


it("distinguishes completed pagination from limited attendee coverage", async () => {
  const account = owner();
  const result = await run(account, async () => page([event("large", { attendees: Array.from({ length: 201 }, (_, i) => ({ email: `guest-${i}@example.test` })) }), event("ordinary")]));
  expect(result).toMatchObject({ complete: true, limited: true, progress: { events: 2, limited_events: 1, imported: 1, skipped: 1 } });
  expect(await count(account)).toBe(1);
});
