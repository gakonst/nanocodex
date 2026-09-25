import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import { CrmError } from "./crm";

export type CrmMeetingOperation = "list" | "get" | "note" | "skip";
type Input = Record<string, unknown>;
type Value = string | number | null;
type Guest = { email: string | null; name: string | null; response_status: string | null; create_id: string };
type Event = { event_id: string; status: string; source_updated: string | null; revision: number;
  title: string; description: string | null; location: string | null; htmlLink: string | null;
  start: string; end: string; start_ms: number; end_ms: number; all_day: boolean; organizer: Input;
  eligible: boolean; self_declined: boolean; self_known: boolean; omitted: boolean; guests: Guest[]; valid: boolean };
type Row = { id: string; connection_id: string; calendar_id: string; event_id: string; title: string;
  description: string | null; location: string | null; htmlLink: string | null; start: string; end: string;
  all_day: number; attendees_complete: number; organizer: string; status: string; source_updated: string | null; skipped: number;
  skip_reason: string | null; created_at: number; updated_at: number; needs_notes: number; start_ms: number };
const columns = "m.id,m.connection_id,m.calendar_id,m.event_id,m.title,m.description,m.location,m.html_link AS htmlLink,m.start_time AS start,m.end_time AS end,m.all_day,m.attendees_complete,m.organizer,m.status,m.source_updated,m.skipped,m.skip_reason,m.created_at,m.updated_at,m.start_ms";
const pending = `(m.status='confirmed' AND m.eligible=1 AND m.self_declined=0 AND m.all_day=0 AND m.end_ms <= ? AND m.skipped=0
  AND EXISTS (SELECT 1 FROM crm_meeting_attendees a WHERE a.owner_id=m.owner_id AND a.meeting_id=m.id AND coalesce(a.response_status,'')!='declined')
  AND NOT EXISTS (SELECT 1 FROM crm_meeting_notes n WHERE n.owner_id=m.owner_id AND n.meeting_id=m.id))`;
const noteColumns = "id,meeting_id,body,created_at,updated_at";
function invalid(message: string): never { throw new CrmError("invalid_input", message); }
function missing(): never { throw new CrmError("not_found", "CRM meeting or note not found."); }
function object(value: unknown, allowed?: readonly string[]): Input {
  if (!value || typeof value !== "object" || Array.isArray(value) || ![Object.prototype, null].includes(Object.getPrototypeOf(value)) || Object.getOwnPropertySymbols(value).length) invalid("Input must be an object.");
  if (allowed && Object.keys(value).some(key => !allowed.includes(key))) invalid("Unknown meeting field.");
  return value as Input;
}
function text(value: unknown, field: string, max = 512, multiline = false): string {
  if (typeof value !== "string" || !value.trim() || value.length > max ||
    (multiline ? /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/ : /[\u0000-\u001f\u007f]/).test(value)) invalid(`Invalid ${field}.`);
  return value;
}
function id(value: unknown): string {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value)) invalid("Invalid id.");
  return value;
}
function hash(value: unknown): string { return createHash("sha256").update(JSON.stringify(value)).digest("hex"); }
function boolean(value: unknown): boolean { if (typeof value !== "boolean") invalid("Expected a boolean."); return value; }
function date(value: unknown, dateOnly = true): { value: string; ms: number; all_day: boolean } {
  const s = text(value, "date", 64);
  const match = /^(\d{4})-(\d{2})-(\d{2})(?:[Tt](\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(?:[Zz]|[+-](\d{2}):(\d{2})))?$/.exec(s);
  if (!match || (!dateOnly && !match[4])) invalid("Date requires an absolute RFC3339 timestamp or YYYY-MM-DD.");
  const [, y, m, d, h, min, sec, oh, om] = match;
  const year = Number(y), month = Number(m), day = Number(d);
  const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  if (month < 1 || month > 12 || day < 1 || day > days[month - 1] || Number(h ?? 0) > 23 || Number(min ?? 0) > 59 || Number(sec ?? 0) > 59 || Number(oh ?? 0) > 23 || Number(om ?? 0) > 59 || !Number.isFinite(Date.parse(s))) invalid("Invalid date.");
  return { value: s, ms: Date.parse(s), all_day: !h };
}
function providerText(value: unknown, max = 512): string | null {
  if (typeof value !== "string") return null;
  const s = value.replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g, "").trim().slice(0, max);
  return s || null;
}
function email(value: unknown): string | null {
  const s = providerText(value)?.toLowerCase();
  return s && /^[^\s@]+@[^\s@]+$/.test(s) ? s : null;
}
function normalize(owner: string, value: unknown, now: number): Event {
  const raw = object(value);
  const event_id = text(raw.id, "Calendar event id", 1024);
  if (raw.attendees !== undefined && !Array.isArray(raw.attendees)) invalid("Calendar attendees must be an array.");
  const attendees = ((raw.attendees ?? []) as unknown[]).slice(0, 200).map(item => object(item));
  const org = raw.organizer && typeof raw.organizer === "object" && !Array.isArray(raw.organizer) ? raw.organizer as Input : {};
  const organizer = { email: email(org.email), displayName: providerText(org.displayName), self: org.self === true };
  const status = raw.status === "cancelled" ? "cancelled" : raw.status === "tentative" ? "tentative" : "confirmed";
  let source_updated: string | null = null, revision = 0;
  if (raw.updated !== undefined) { const parsed = date(raw.updated, false); source_updated = parsed.value; revision = parsed.ms; }
  // A versionless cancellation is a tombstone observed now, not revision zero.
  if (status === "cancelled" && !source_updated) revision = now;
  const guests: Guest[] = [];
  const seen = new Set<string>();
  const selfEmails = new Set(attendees.filter(a => a.self === true).map(a => email(a.email)).filter(Boolean));
  if (org.self === true) selfEmails.add(email(org.email));
  const candidates = [...attendees];
  if (org.self !== true && org.resource !== true && email(org.email) && !attendees.some(a => email(a.email) === email(org.email))) candidates.push(org);
  for (const guest of candidates) {
    const address = email(guest.email);
    if (guest.self === true || guest.resource === true || (address !== null && selfEmails.has(address))) continue;
    if (address && seen.has(address)) continue;
    if (address) seen.add(address);
    guests.push({ email: address, name: providerText(guest.displayName),
      response_status: typeof guest.responseStatus === "string" && ["accepted", "declined", "tentative", "needsAction"].includes(guest.responseStatus) ? guest.responseStatus : null,
      create_id: hash([owner, "calendar-person", address]) });
  }
  const self = attendees.find(a => a.self === true);
  let start = "", end = "", start_ms = 0, end_ms = 0, all_day = false, valid = false;
  try {
    const startRaw = object(raw.start), endRaw = object(raw.end);
    const a = date(startRaw.dateTime ?? startRaw.date), b = date(endRaw.dateTime ?? endRaw.date);
    if (a.all_day === b.all_day && a.ms < b.ms) { start = a.value; end = b.value; start_ms = a.ms; end_ms = b.ms; all_day = a.all_day; valid = true; }
  } catch { /* Missing/invalid provider time cannot imply a completed meeting. */ }
  const link = providerText(raw.htmlLink, 2048);
  let htmlLink: string | null = null;
  if (link) try { const u = new URL(link); if (u.protocol === "https:" && !u.username && !u.password) htmlLink = link; } catch { /* Ignore invalid provider links. */ }
  return { event_id, status, source_updated, revision, title: providerText(raw.summary, 512) ?? "Untitled meeting", description: providerText(raw.description, 20000),
    location: providerText(raw.location, 2048), htmlLink, start, end, start_ms, end_ms, all_day, organizer,
    eligible: (raw.eventType === undefined || raw.eventType === "default") && raw.endTimeUnspecified !== true,
    self_declined: self?.responseStatus === "declined", self_known: self !== undefined, omitted: raw.attendeesOmitted === true || (Array.isArray(raw.attendees) && raw.attendees.length > 200), guests, valid };
}
function view(row: Row) {
  const { start_ms: _, ...rest } = row;
  return { ...rest, all_day: Boolean(row.all_day), attendees_complete: Boolean(row.attendees_complete), organizer: JSON.parse(row.organizer), skipped: Boolean(row.skipped), needs_notes: Boolean(row.needs_notes) };
}

/** A page is validated before any writes. Each event snapshot and its links are
 * replaced in one D1 transaction; a revision/token guard also fences racing imports. */
export async function importCalendarEvents(db: D1Database, ownerId: string, input: { connection_id: string; calendar_id: string; events: unknown[] }, now = Date.now()): Promise<unknown> {
  text(ownerId, "authenticated owner");
  const connection = text(input.connection_id, "connection_id", 1024), calendar = text(input.calendar_id, "calendar_id", 1024);
  if (!Array.isArray(input.events) || input.events.length > 100 || !Number.isSafeInteger(now) || now < 0) invalid("Invalid Calendar page.");
  const events = input.events.map(item => normalize(ownerId, item, now));
  const result = { imported: 0, skipped: 0, people_created: 0, limited_events: 0, unresolved: [] as { event_id: string; email: string | null; reason: string }[] };
  const session = db.withSession("first-primary");
  try {
    for (const event of events) {
      if (event.omitted && event.status !== "cancelled") result.limited_events++;
      const meetingId = hash([ownerId, connection, calendar, event.event_id]);
      const token = crypto.randomUUID();
      const statements: D1PreparedStatement[] = [];
      if (event.status === "cancelled") {
        const changed = await session.batch([
          session.prepare(`INSERT INTO crm_calendar_tombstones(owner_id,meeting_id,source_revision) VALUES (?,?,?)
            ON CONFLICT(owner_id,meeting_id) DO UPDATE SET source_revision=max(source_revision,excluded.source_revision)`).bind(ownerId, meetingId, event.revision),
          session.prepare(`UPDATE crm_meetings SET status='cancelled',source_updated=?,source_revision=?,import_token=?,updated_at=max(updated_at,?)
            WHERE owner_id=? AND id=? AND source_revision < ? RETURNING id`).bind(event.source_updated, event.revision, token, now, ownerId, meetingId, event.revision),
        ]);
        if (changed[1].results.length) result.imported++; else result.skipped++;
        continue;
      }
      if (!event.valid) { result.skipped++; continue; }
      // Ineligible new events aren't contacts. Existing events must still learn
      // about declines/non-meeting changes and retain all user-authored data.
      const allowNew = event.eligible && !event.self_declined && event.guests.length > 0 && !event.omitted;
      statements.push(session.prepare(`INSERT INTO crm_meetings (owner_id,id,connection_id,calendar_id,event_id,title,description,location,html_link,start_time,end_time,start_ms,end_ms,all_day,organizer,status,eligible,self_declined,attendees_complete,source_updated,source_revision,import_token,created_at,updated_at)
        SELECT ?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,? WHERE (? OR EXISTS (SELECT 1 FROM crm_meetings WHERE owner_id=? AND id=?))
        AND NOT EXISTS (SELECT 1 FROM crm_calendar_tombstones WHERE owner_id=? AND meeting_id=? AND source_revision>=?)
        ON CONFLICT(owner_id,id) DO UPDATE SET title=excluded.title,description=excluded.description,location=excluded.location,html_link=excluded.html_link,
          start_time=excluded.start_time,end_time=excluded.end_time,start_ms=excluded.start_ms,end_ms=excluded.end_ms,all_day=excluded.all_day,
          organizer=excluded.organizer,status=excluded.status,eligible=excluded.eligible,
          self_declined=CASE WHEN ? THEN crm_meetings.self_declined ELSE excluded.self_declined END,
          attendees_complete=excluded.attendees_complete,source_updated=excluded.source_updated,source_revision=excluded.source_revision,import_token=excluded.import_token,updated_at=max(crm_meetings.updated_at,excluded.updated_at)
        WHERE excluded.source_revision > crm_meetings.source_revision
          OR (excluded.source_revision=crm_meetings.source_revision AND crm_meetings.attendees_complete=0 AND excluded.attendees_complete=1) RETURNING id`).bind(ownerId, meetingId, connection, calendar, event.event_id, event.title, event.description, event.location, event.htmlLink,
          event.start, event.end, event.start_ms, event.end_ms, Number(event.all_day), JSON.stringify(event.organizer), event.status, Number(event.eligible), Number(event.self_declined),
          Number(!event.omitted), event.source_updated, event.revision, token, now, now, Number(allowNew), ownerId, meetingId, ownerId, meetingId, event.revision, Number(event.omitted && !event.self_known)));
      if (event.omitted) {
        const changed = await session.batch(statements);
        if (changed[0].results.length) result.imported++; else result.skipped++;
        continue;
      }
      const guests = JSON.stringify(event.guests);
      const guard = "EXISTS (SELECT 1 FROM crm_meetings WHERE owner_id=? AND id=? AND import_token=?)";
      statements.push(session.prepare(`DELETE FROM crm_meeting_attendees WHERE owner_id=? AND meeting_id=? AND ${guard}`).bind(ownerId, meetingId, ownerId, meetingId, token));
      statements.push(session.prepare(`INSERT INTO crm_records(owner_id,id,kind,name,email,tags,created_at,updated_at)
        SELECT ?,json_extract(g.value,'$.create_id'),'person',coalesce(json_extract(g.value,'$.name'),json_extract(g.value,'$.email')),json_extract(g.value,'$.email'),'[]',?,?
        FROM json_each(?) g WHERE json_extract(g.value,'$.email') IS NOT NULL AND ${guard}
        AND NOT EXISTS (SELECT 1 FROM crm_records r WHERE r.owner_id=? AND r.kind='person' AND (lower(trim(r.email))=json_extract(g.value,'$.email') OR EXISTS (
          SELECT 1 FROM crm_identities i WHERE i.owner_id=r.owner_id AND i.record_id=r.id AND i.kind='email' AND i.normalized=json_extract(g.value,'$.email'))))
        ON CONFLICT(owner_id,id) DO NOTHING`).bind(ownerId, now, now, guests, ownerId, meetingId, token, ownerId));
      statements.push(session.prepare(`INSERT INTO crm_meeting_attendees(owner_id,meeting_id,ordinal,email,name,response_status,person_id)
        SELECT ?,?,CAST(g.key AS INTEGER),json_extract(g.value,'$.email'),json_extract(g.value,'$.name'),json_extract(g.value,'$.response_status'),
          (SELECT CASE WHEN count(*)=1 THEN min(r.id) ELSE NULL END FROM crm_records r WHERE r.owner_id=? AND r.kind='person' AND (lower(trim(r.email))=json_extract(g.value,'$.email') OR EXISTS (
          SELECT 1 FROM crm_identities i WHERE i.owner_id=r.owner_id AND i.record_id=r.id AND i.kind='email' AND i.normalized=json_extract(g.value,'$.email'))))
        FROM json_each(?) g WHERE ${guard}`).bind(ownerId, meetingId, ownerId, guests, ownerId, meetingId, token));
      statements.push(session.prepare(`SELECT a.email,CASE WHEN a.email IS NULL THEN 'missing_email' ELSE 'ambiguous_email' END AS reason
        FROM crm_meeting_attendees a WHERE a.owner_id=? AND a.meeting_id=? AND a.person_id IS NULL AND ${guard}`).bind(ownerId, meetingId, ownerId, meetingId, token));
      const changed = await session.batch(statements);
      if (changed[0].results.length) {
        result.imported++;
        result.people_created += changed[2].meta.changes;
        for (const row of changed[4].results as { email: string | null; reason: string }[]) result.unresolved.push({ event_id: event.event_id, ...row });
      } else result.skipped++;
    }
    return result;
  } catch (error) {
    if (error instanceof CrmError) throw error;
    throw new Error("CRM meeting import failed.");
  }
}

export async function crmMeetingRequest(db: D1Database, ownerId: string, operation: CrmMeetingOperation, input: unknown, createId: string): Promise<unknown> {
  text(ownerId, "authenticated owner");
  const session = db.withSession("first-primary");
  const read = async (meetingId: string) => {
    const row = await session.prepare(`SELECT ${columns},${pending} AS needs_notes FROM crm_meetings m WHERE m.owner_id=? AND m.id=?`).bind(Date.now(), ownerId, meetingId).first<Row>();
    if (!row) missing();
    return row;
  };
  try {
    switch (operation) {
      case "list": {
        const args = object(input, ["q", "person_id", "needs_notes", "from", "to", "limit", "cursor"]);
        const q = args.q === undefined || args.q === "" ? "" : text(args.q, "q");
        const personId = args.person_id === undefined ? null : id(args.person_id);
        const needs = args.needs_notes === undefined ? null : boolean(args.needs_notes);
        const from = args.from === undefined ? null : date(args.from).ms, to = args.to === undefined ? null : date(args.to).ms;
        if (from !== null && to !== null && from >= to) invalid("from must precede to.");
        const size = args.limit ?? 20;
        if (typeof size !== "number" || !Number.isInteger(size) || size < 1 || size > 100) invalid("limit must be an integer from 1 to 100.");
        const scope = hash([ownerId, q, personId, needs, from, to]);
        let cursor: { at: number; id: string } | null = null;
        if (args.cursor !== undefined) {
          try {
            const encoded = text(args.cursor, "cursor", 4096);
            if (!/^[A-Za-z0-9_-]+$/.test(encoded)) invalid("Invalid cursor.");
            const decoded = object(JSON.parse(Buffer.from(encoded, "base64url").toString("utf8")), ["v", "scope", "at", "id"]);
            if (decoded.v !== 1 || decoded.scope !== scope || typeof decoded.at !== "number" || !Number.isSafeInteger(decoded.at)) invalid("Invalid cursor.");
            cursor = { at: decoded.at, id: id(decoded.id) };
          } catch { invalid("Invalid cursor for this query."); }
        }
        const now = Date.now(), where = ["m.owner_id=?"], values: Value[] = [now, ownerId];
        if (needs !== null) { where.push(`${pending}=?`); values.push(now, Number(needs)); }
        if (personId) { where.push("EXISTS (SELECT 1 FROM crm_meeting_attendees a WHERE a.owner_id=m.owner_id AND a.meeting_id=m.id AND a.person_id=?)"); values.push(personId); }
        if (q) {
          where.push(`(instr(lower(m.title),lower(?))>0 OR EXISTS (SELECT 1 FROM crm_meeting_attendees a LEFT JOIN crm_records r ON r.owner_id=a.owner_id AND r.id=a.person_id
            WHERE a.owner_id=m.owner_id AND a.meeting_id=m.id AND (instr(lower(coalesce(a.name,'')),lower(?))>0 OR instr(lower(coalesce(a.email,'')),lower(?))>0 OR instr(lower(coalesce(r.name,'')),lower(?))>0)))`);
          values.push(q, q, q, q);
        }
        if (from !== null) { where.push("m.start_ms>=?"); values.push(from); }
        if (to !== null) { where.push("m.start_ms<?"); values.push(to); }
        if (cursor) { where.push("(m.start_ms>? OR (m.start_ms=? AND m.id>?))"); values.push(cursor.at, cursor.at, cursor.id); }
        const rows = (await session.prepare(`SELECT ${columns},${pending} AS needs_notes FROM crm_meetings m WHERE ${where.join(" AND ")} ORDER BY m.start_ms,m.id LIMIT ?`).bind(...values, size + 1).all<Row>()).results;
        const page = rows.slice(0, size), last = page.at(-1);
        return { meetings: page.map(view), next_cursor: rows.length > size && last ? Buffer.from(JSON.stringify({ v: 1, scope, at: last.start_ms, id: last.id })).toString("base64url") : null };
      }
      case "get": {
        const args = object(input, ["id"]), meetingId = id(args.id);
        const row = await read(meetingId);
        const results = await session.batch([
          session.prepare("SELECT email,name,response_status,person_id FROM crm_meeting_attendees WHERE owner_id=? AND meeting_id=? ORDER BY ordinal LIMIT 201").bind(ownerId, meetingId),
          session.prepare(`SELECT ${noteColumns} FROM crm_meeting_notes WHERE owner_id=? AND meeting_id=? ORDER BY created_at,id LIMIT 101`).bind(ownerId, meetingId),
        ]);
        return { meeting: view(row), attendees: results[0].results, notes: results[1].results.slice(0, 100), notes_truncated: results[1].results.length > 100 };
      }
      case "skip": {
        const args = object(input, ["id", "reason", "skipped"]), meetingId = id(args.id);
        const skipped = args.skipped === undefined ? true : boolean(args.skipped);
        const reason = args.reason === undefined ? null : text(args.reason, "reason", 1000, true);
        const result = await session.prepare("UPDATE crm_meetings SET skipped=?,skip_reason=?,updated_at=max(updated_at,?) WHERE owner_id=? AND id=? RETURNING id").bind(Number(skipped), skipped ? reason : null, Date.now(), ownerId, meetingId).first();
        if (!result) missing();
        return { meeting: view(await read(meetingId)) };
      }
      case "note": {
        const args = object(input, ["id", "meeting_id", "body"]), meetingId = id(args.meeting_id), body = text(args.body, "body", 20000, true), now = Date.now();
        const noteId = id(args.id ?? createId);
        if (args.id !== undefined) {
          const existing = await session.prepare(`SELECT ${noteColumns} FROM crm_meeting_notes WHERE owner_id=? AND id=?`).bind(ownerId, noteId).first<{ meeting_id: string }>();
          if (!existing) missing();
          if (existing.meeting_id !== meetingId) invalid("A note cannot move between meetings.");
          const note = await session.prepare(`UPDATE crm_meeting_notes SET body=?,updated_at=max(updated_at,?) WHERE owner_id=? AND id=? AND meeting_id=? RETURNING ${noteColumns}`).bind(body, now, ownerId, noteId, meetingId).first();
          if (!note) missing();
          return { note };
        }
        const results = await session.batch([
          session.prepare("SELECT id FROM crm_meetings WHERE owner_id=? AND id=?").bind(ownerId, meetingId),
          session.prepare(`INSERT INTO crm_meeting_notes(owner_id,id,meeting_id,body,created_at,updated_at)
            SELECT ?,?,?,?,?,? WHERE EXISTS (SELECT 1 FROM crm_meetings WHERE owner_id=? AND id=?) ON CONFLICT(owner_id,id) DO NOTHING`).bind(ownerId, noteId, meetingId, body, now, now, ownerId, meetingId),
          session.prepare(`SELECT ${noteColumns} FROM crm_meeting_notes WHERE owner_id=? AND id=?`).bind(ownerId, noteId),
        ]);
        if (!results[0].results.length) missing();
        const note = results[2].results[0] as { meeting_id: string } | undefined;
        if (!note) missing();
        if (note.meeting_id !== meetingId) invalid("A note cannot move between meetings.");
        return { note };
      }
      default: return invalid("Unknown meeting operation.");
    }
  } catch (error) {
    if (error instanceof CrmError) throw error;
    throw new Error("CRM meeting storage request failed.");
  }
}
