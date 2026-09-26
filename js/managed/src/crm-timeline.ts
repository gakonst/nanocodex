import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import { CrmError } from "./crm";

export interface CrmTimelineEntry {
  kind: string;
  id: string;
  person_id: string | null;
  occurred_at: string;
  [field: string]: unknown;
}
export interface CrmTimelinePage { entries: CrmTimelineEntry[]; next_cursor: string | null }
type Input = Record<string, unknown>;
function invalid(message = "Invalid timeline input."): never { throw new CrmError("invalid_input", message); }
function object(value: unknown, keys: string[]): Input {
  if (!value || typeof value !== "object" || Array.isArray(value) || ![Object.prototype, null].includes(Object.getPrototypeOf(value)) || Object.getOwnPropertySymbols(value).length || Object.keys(value).some(k => !keys.includes(k))) invalid();
  return value as Input;
}
function id(value: unknown): string {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value)) invalid("Invalid person or entry id.");
  return value;
}
function date(value: unknown): number | null {
  if (value === undefined) return null;
  if (typeof value !== "string" || value.length > 64) invalid("Expected an absolute RFC3339 timestamp.");
  const m = /^(\d{4})-(\d{2})-(\d{2})[Tt](\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(?:[Zz]|[+-](\d{2}):(\d{2}))$/.exec(value);
  if (!m) invalid("Expected an absolute RFC3339 timestamp.");
  const year = Number(m[1]), month = Number(m[2]), day = Number(m[3]);
  const days = [31, year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0) ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  const ms = Date.parse(value);
  if (month < 1 || month > 12 || day < 1 || day > days[month - 1] || Number(m[4]) > 23 || Number(m[5]) > 59 || Number(m[6]) > 59 || Number(m[7] ?? 0) > 23 || Number(m[8] ?? 0) > 59 || !Number.isSafeInteger(ms)) invalid("Invalid timestamp.");
  return ms;
}
const kinds = ["calendar_meeting", "email", "event_participation", "interaction", "note", "meeting_note"];
// Each source retains its provenance. EXISTS avoids multiplying a meeting when
// several email aliases resolve to the same person. Email import receipts alone
// are deliberately insufficient: a deleted note must stay deleted.
// Conflicting responses across aliases stay unresolved rather than selecting an
// arbitrary attendee row. Calendar responses never establish actual attendance.
const calendarResponse = `(SELECT CASE WHEN count(DISTINCT coalesce(a.response_status,''))=1 THEN min(a.response_status) ELSE NULL END FROM crm_meeting_attendees a WHERE a.owner_id=m.owner_id AND a.meeting_id=m.id AND a.person_id=?2)`;
const union = `
 SELECT m.start_ms AS at,'calendar_meeting' AS kind,m.id,
 json_object('title',m.title,'meeting_id',m.id,'status',m.status,'participation_status',CASE WHEN ${calendarResponse}='declined' THEN 'declined' ELSE 'invited' END,'response_status',${calendarResponse},'self_declined',json(CASE WHEN m.self_declined=1 THEN 'true' ELSE 'false' END),'attendance_status','unknown','origin','source',
 'sources',json_array(json_object('kind','calendar','reference',m.event_id)),
 'connection_id',m.connection_id,'calendar_id',m.calendar_id,'event_id',m.event_id,'source_url',m.html_link) AS payload
 FROM crm_meetings m WHERE m.owner_id=?1 AND ?3 IS NULL AND (?2 IS NULL OR EXISTS
 (SELECT 1 FROM crm_meeting_attendees a WHERE a.owner_id=m.owner_id AND a.meeting_id=m.id AND a.person_id=?2))
 UNION ALL
 SELECT CASE WHEN i.note_id IS NULL THEN n.created_at ELSE coalesce(i.received_ms,i.imported_at) END,
 CASE WHEN i.note_id IS NULL THEN 'note' ELSE 'email' END,n.id,
 CASE WHEN i.note_id IS NULL THEN json_object('body',n.body,'record_id',n.record_id,'timestamp_basis','created_at','source_url',n.source_url,'sources',json_array())
 ELSE json_object('body',n.body,'record_id',n.record_id,'origin','source','timestamp_basis',CASE WHEN i.received_ms IS NULL THEN 'imported_at' ELSE 'received_at' END,'sources',json_array(json_object('kind','email','reference',i.message_id)),
 'connection_id',i.connection_id,'message_id',i.message_id,'source_url',n.source_url) END
 FROM crm_notes n LEFT JOIN crm_email_imports i ON i.owner_id=n.owner_id AND i.note_id=n.id AND i.record_id=n.record_id
 WHERE n.owner_id=?1 AND ?3 IS NULL AND (?2 IS NULL OR n.record_id=?2)
 UNION ALL
 SELECT e.start_ms,'event_participation',p.id,
 json_object('title',e.title,'event_id',e.id,'record_id',p.record_id,'person_id',p.person_id,'participation_status',p.status,'role',p.role,'origin',p.origin,
 'sources',json(p.sources),'confidence',p.confidence,'rationale',p.rationale)
 FROM crm_event_participation p JOIN crm_events e ON e.owner_id=p.owner_id AND e.id=p.event_id
 WHERE p.owner_id=?1 AND (?2 IS NULL OR p.person_id=?2) AND (?3 IS NULL OR p.event_id=?3)
 UNION ALL
 SELECT i.occurred_ms,'interaction',i.id,
 json_object('body',i.body,'type',i.type,'summary',i.summary,'occurred_at',i.occurred_at,'precision',i.precision,'participants',json((SELECT json_group_array(json_object('record_id',p.record_id,'role',p.role)) FROM (SELECT record_id,role FROM crm_interaction_participants WHERE owner_id=i.owner_id AND interaction_id=i.id ORDER BY record_id LIMIT 100) p)),'event_id',i.event_id,'meeting_id',i.meeting_id,'connection_id',i.connection_id,'message_id',i.message_id,
 'origin',i.origin,'sources',json(i.sources),'confidence',i.confidence,'rationale',i.rationale)
 FROM crm_interactions i WHERE i.owner_id=?1 AND (?3 IS NULL OR i.event_id=?3) AND (?2 IS NULL OR EXISTS (SELECT 1 FROM crm_interaction_participants p WHERE p.owner_id=i.owner_id AND p.interaction_id=i.id AND p.record_id=?2))
 UNION ALL
 SELECT n.created_at,'meeting_note',n.id,json_object('body',n.body,'meeting_id',n.meeting_id,'origin','user','sources',json_array())
 FROM crm_meeting_notes n WHERE n.owner_id=?1 AND ?3 IS NULL AND (?2 IS NULL OR EXISTS
 (SELECT 1 FROM crm_meeting_attendees a WHERE a.owner_id=n.owner_id AND a.meeting_id=n.meeting_id AND a.person_id=?2))`;

/** Account-private, descending keyset timeline. Calendar invitations do not prove attendance. */
export async function crmTimelineRequest(db: D1Database, owner: string, input: unknown): Promise<CrmTimelinePage> {
  if (typeof owner !== "string" || !owner.trim() || owner.length > 512 || /[\u0000-\u001f\u007f]/.test(owner)) invalid("Invalid authenticated owner.");
  const args = object(input, ["person_id", "event_id", "limit", "cursor", "from", "to"]);
  const person = args.person_id === undefined ? null : id(args.person_id), event = args.event_id === undefined ? null : id(args.event_id), from = date(args.from), to = date(args.to), limit = args.limit ?? 20;
  if (typeof limit !== "number" || !Number.isInteger(limit) || limit < 1 || limit > 100) invalid("limit must be an integer from 1 to 100.");
  if (from !== null && to !== null && from >= to) invalid("from must precede to.");
  const scope = createHash("sha256").update(JSON.stringify([owner, person, event, from, to])).digest("hex");
  let cursor: { at: number; kind: string; id: string } | null = null;
  if (args.cursor !== undefined) {
    try {
      if (typeof args.cursor !== "string" || args.cursor.length > 4096 || !/^[A-Za-z0-9_-]+$/.test(args.cursor)) invalid();
      const c = object(JSON.parse(Buffer.from(args.cursor, "base64url").toString("utf8")), ["v", "scope", "at", "kind", "id"]);
      if (c.v !== 1 || c.scope !== scope || typeof c.at !== "number" || !Number.isSafeInteger(c.at) || typeof c.kind !== "string" || !kinds.includes(c.kind)) invalid();
      cursor = { at: c.at, kind: c.kind, id: id(c.id) };
    } catch { invalid("Invalid cursor for this timeline query."); }
  }
  try {
    const session = db.withSession("first-primary");
    if (person !== null && !await session.prepare("SELECT id FROM crm_records WHERE owner_id=? AND id=? AND kind='person'").bind(owner, person).first()) throw new CrmError("not_found", "CRM person not found.");
    if (event !== null && !await session.prepare("SELECT id FROM crm_events WHERE owner_id=? AND id=?").bind(owner, event).first()) throw new CrmError("not_found", "CRM event not found.");
    const where: string[] = [], values: (string | number | null)[] = [owner, person, event];
    if (from !== null) { where.push("at>=?"); values.push(from); }
    if (to !== null) { where.push("at<?"); values.push(to); }
    if (cursor) {
      where.push("(at<? OR (at=? AND (kind<? OR (kind=? AND id<?))))");
      values.push(cursor.at, cursor.at, cursor.kind, cursor.kind, cursor.id);
    }
    const rows = (await session.prepare(`WITH timeline AS (${union}) SELECT at,kind,id,payload FROM timeline ${where.length ? `WHERE ${where.join(" AND ")}` : ""} ORDER BY at DESC,kind DESC,id DESC LIMIT ?`).bind(...values, limit + 1).all<{ at: number; kind: string; id: string; payload: string }>()).results;
    const page = rows.slice(0, limit), last = page.at(-1);
    return {
      entries: page.map(row => ({ kind: row.kind, id: row.id, person_id: person, occurred_at: new Date(row.at).toISOString(), ...JSON.parse(row.payload) as Input })),
      next_cursor: rows.length > limit && last ? Buffer.from(JSON.stringify({ v: 1, scope, at: last.at, kind: last.kind, id: last.id })).toString("base64url") : null,
    };
  } catch (error) {
    if (error instanceof CrmError) throw error;
    throw new Error("CRM timeline storage request failed.");
  }
}
