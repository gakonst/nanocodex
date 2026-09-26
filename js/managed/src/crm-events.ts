import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import { CrmError } from "./crm";

export type CrmEventOperation = "list" | "get" | "save" | "delete";
export type CrmParticipationOperation = "list" | "save" | "delete";
export type CrmInteractionOperation = CrmEventOperation;
type Input = Record<string, unknown>;
type Value = string | number | null;
type Row = Record<string, Value>;
type Kind = "event" | "participation" | "interaction";
const provenance = ["origin", "sources", "confidence", "rationale"];
const fields: Record<Kind, string[]> = {
  event: ["title", "description", "location", "start_at", "end_at", ...provenance],
  participation: ["event_id", "record_id", "status", "role", ...provenance],
  interaction: ["participants", "type", "summary", "person_id", "event_id", "meeting_id", "connection_id", "message_id", "occurred_at", "body", ...provenance],
};
const tables = { event: "crm_events", participation: "crm_event_participation", interaction: "crm_interactions" };
const plurals = { event: "events", participation: "participation", interaction: "interactions" };
function invalid(message: string): never { throw new CrmError("invalid_input", message); }
function missing(): never { throw new CrmError("not_found", "CRM event, person, participation or interaction not found."); }
function object(value: unknown, allowed: string[]): Input {
  if (!value || typeof value !== "object" || Array.isArray(value) || ![Object.prototype, null].includes(Object.getPrototypeOf(value)) || Object.getOwnPropertySymbols(value).length) invalid("Input must be an object.");
  if (Object.keys(value).some(key => !allowed.includes(key))) invalid("Unknown field.");
  return value as Input;
}
function text(value: unknown, field: string, max = 512, multiline = false): string {
  if (typeof value !== "string" || !value.trim() || value.length > max || (multiline ? /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/ : /[\u0000-\u001f\u007f]/).test(value)) invalid(`Invalid ${field}.`);
  return value.trim();
}
function id(value: unknown): string {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value)) invalid("Invalid id.");
  return value;
}
function date(value: unknown): { value: string; ms: number } {
  const s = text(value, "timestamp", 64);
  const m = /^(\d{4})-(\d{2})-(\d{2})[Tt](\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(?:[Zz]|[+-](\d{2}):(\d{2}))$/.exec(s);
  if (!m) invalid("Expected an absolute RFC3339 timestamp.");
  const y = Number(m[1]), month = Number(m[2]), d = Number(m[3]);
  const days = [31, y % 4 === 0 && (y % 100 !== 0 || y % 400 === 0) ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  if (month < 1 || month > 12 || d < 1 || d > days[month - 1] || Number(m[4]) > 23 || Number(m[5]) > 59 || Number(m[6]) > 59 || Number(m[7] ?? 0) > 23 || Number(m[8] ?? 0) > 59 || !Number.isFinite(Date.parse(s))) invalid("Invalid timestamp.");
  return { value: s, ms: Date.parse(s) };
}
function choice(value: unknown, allowed: string[], field: string): string {
  if (typeof value !== "string" || !allowed.includes(value)) invalid(`Invalid ${field}.`);
  return value;
}
function sources(value: unknown): string {
  if (!Array.isArray(value) || value.length > 50) invalid("sources must be an array with at most 50 entries.");
  const result = value.map(item => {
    const s = object(item, ["kind", "reference", "detail"]);
    const kind = choice(s.kind, ["web", "email", "calendar", "document", "user"], "source kind");
    const reference = text(s.reference, "source reference", 2048);
    if (kind === "web" || /^[a-z][a-z0-9+.-]*:\/\//i.test(reference)) {
      let url: URL; try { url = new URL(reference); } catch { return invalid("Invalid source URL."); }
      if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || /[\s\\]/.test(reference)) invalid("Invalid source URL.");
    }
    return { kind, reference, ...(s.detail === undefined ? {} : { detail: text(s.detail, "source detail", 2000, true) }) };
  });
  const encoded = JSON.stringify(result);
  if (Buffer.byteLength(encoded) > 16384) invalid("Sources exceed 16 KiB.");
  return encoded;
}
function view(row: Row): Input {
  const { owner_id: _, ...rest } = row;
  return { ...rest, sources: JSON.parse(String(row.sources)) };
}

async function request(db: D1Database, owner: string, kind: Kind, operation: string, input: unknown, createId: string): Promise<unknown> {
  text(owner, "authenticated owner", 512);
  const session = db.withSession("first-primary"), table = tables[kind];
  const read = async (recordId: string): Promise<Row> => {
    const row = await session.prepare(`SELECT * FROM ${table} WHERE owner_id=? AND id=?`).bind(owner, recordId).first<Row>();
    if (!row) missing();
    return row;
  };
  const participantsFor = async (key: string) => (await session.prepare("SELECT record_id,role FROM crm_interaction_participants WHERE owner_id=? AND interaction_id=? ORDER BY record_id").bind(owner, key).all<{record_id:string;role:string|null}>()).results;
  const present = async (row: Row) => ({ ...view(row), ...(kind === "interaction" ? { participants: await participantsFor(String(row.id)) } : {}) });
  const person = async (personId: string) => {
    if (!await session.prepare("SELECT id FROM crm_records WHERE owner_id=? AND id=? AND kind='person'").bind(owner, personId).first()) missing();
  };
  try {
    if (operation === "delete") {
      const args = object(input, ["id"]);
      if (!await session.prepare(`DELETE FROM ${table} WHERE owner_id=? AND id=? RETURNING id`).bind(owner, id(args.id)).first()) missing();
      return { deleted: true };
    }
    if (operation === "get" && kind !== "participation") {
      const args = object(input, kind === "event" ? ["id", "roster_limit", "roster_cursor"] : ["id"]);
      const row = await read(id(args.id));
      if (kind === "interaction") return { interaction: await present(row) };
      const roster = await request(db, owner, "participation", "list", { event_id: row.id,
        ...(args.roster_limit === undefined ? {} : { limit: args.roster_limit }), ...(args.roster_cursor === undefined ? {} : { cursor: args.roster_cursor }) }, createId) as Input;
      return { event: view(row), ...roster };
    }
    if (operation === "list") {
      const filters = kind === "event" ? ["q", "from", "to"] : kind === "participation" ? ["event_id", "record_id", "person_id"] : ["person_id", "event_id", "from", "to"];
      const args = object(input, [...filters, "limit", "cursor"]);
      const size = args.limit ?? 20;
      if (typeof size !== "number" || !Number.isInteger(size) || size < 1 || size > 100) invalid("limit must be an integer from 1 to 100.");
      const where = ["owner_id=?"], values: Value[] = [owner];
      const order = kind === "event" ? "start_ms" : kind === "interaction" ? "occurred_ms" : "created_at";
      for (const field of ["person_id", "record_id", "event_id"]) if (args[field] !== undefined) {
        const key = id(args[field]); where.push(kind === "interaction" && field === "person_id" ? "EXISTS(SELECT 1 FROM crm_interaction_participants p WHERE p.owner_id=crm_interactions.owner_id AND p.interaction_id=crm_interactions.id AND p.record_id=?)" : `${field}=?`); values.push(key);
        if (field === "person_id") await person(key);
        else if (field === "record_id") { if (!await session.prepare("SELECT id FROM crm_records WHERE owner_id=? AND id=?").bind(owner, key).first()) missing(); }
        else if (!await session.prepare("SELECT id FROM crm_events WHERE owner_id=? AND id=?").bind(owner, key).first()) missing();
      }
      if (kind === "participation" && args.person_id === undefined && args.record_id === undefined && args.event_id === undefined) invalid("event_id or person_id is required.");
      if (args.q !== undefined) { where.push("instr(lower(title),lower(?))>0"); values.push(text(args.q, "q")); }
      const from = args.from === undefined ? null : date(args.from).ms, to = args.to === undefined ? null : date(args.to).ms;
      if (from !== null && to !== null && from >= to) invalid("from must precede to.");
      if (from !== null) { where.push(`${order}>=?`); values.push(from); }
      if (to !== null) { where.push(`${order}<?`); values.push(to); }
      const scope = createHash("sha256").update(JSON.stringify([kind, where, values])).digest("hex");
      if (args.cursor !== undefined) {
        try {
          const encoded = text(args.cursor, "cursor", 4096);
          if (!/^[A-Za-z0-9_-]+$/.test(encoded)) invalid("Invalid cursor.");
          const c = object(JSON.parse(Buffer.from(encoded, "base64url").toString()), ["v", "scope", "at", "id"]);
          if (c.v !== 1 || c.scope !== scope || typeof c.at !== "number" || !Number.isSafeInteger(c.at)) invalid("Invalid cursor.");
          where.push(`(${order}>? OR (${order}=? AND id>?))`); values.push(c.at, c.at, id(c.id));
        } catch { invalid("Invalid cursor for this query."); }
      }
      const rows = (await session.prepare(`SELECT * FROM ${table} WHERE ${where.join(" AND ")} ORDER BY ${order},id LIMIT ?`).bind(...values, size + 1).all<Row>()).results;
      const page = rows.slice(0, size), last = page.at(-1);
      return { [plurals[kind]]: await Promise.all(page.map(present)), next_cursor: rows.length > size && last ? Buffer.from(JSON.stringify({ v: 1, scope, at: last[order], id: last.id })).toString("base64url") : null };
    }
    if (operation !== "save") invalid("Unknown operation.");
    const args = object(input, ["id", ...fields[kind]]), recordId = id(args.id ?? createId);
    const existing = args.id === undefined ? null : await read(recordId);
    const merged: Input = { ...(existing ?? {}), ...args };
    const immutable = kind === "event" ? ["origin"] : kind === "participation" ? ["event_id", "record_id", "person_id", "origin"] : ["person_id", "event_id", "meeting_id", "connection_id", "message_id", "origin"];
    for (const field of immutable) if (existing && args[field] !== undefined && args[field] !== existing[field]) invalid(`${field} is immutable.`);
    let participants: {record_id:string;role:string|null}[] = [];
    const row: Row = {};
    row.origin = choice(merged.origin, ["user", "source", "inferred"], "origin");
    row.sources = args.sources === undefined && existing ? String(existing.sources) : sources(merged.sources ?? []);
    row.confidence = merged.confidence == null ? null : choice(merged.confidence, ["low", "medium", "high"], "confidence");
    row.rationale = merged.rationale == null ? null : text(merged.rationale, "rationale", 2000, true);
    if (row.origin !== "user" && JSON.parse(row.sources).length === 0) invalid("Source and inferred records require evidence.");
    if (row.origin === "inferred" && (!row.confidence || !row.rationale)) invalid("Inferences require confidence and rationale.");
    if (kind === "event") {
      row.title = text(merged.title, "title");
      row.description = merged.description == null ? null : text(merged.description, "description", 20000, true);
      row.location = merged.location == null ? null : text(merged.location, "location", 2048);
      const start = date(merged.start_at), end = merged.end_at == null ? null : date(merged.end_at);
      if (end && end.ms < start.ms) invalid("end_at must not precede start_at.");
      Object.assign(row, { start_at: start.value, start_ms: start.ms, end_at: end?.value ?? null, end_ms: end?.ms ?? null });
    } else {
      if (kind === "participation") {
        row.record_id = id(merged.record_id);
        const record = await session.prepare("SELECT kind FROM crm_records WHERE owner_id=? AND id=?").bind(owner, row.record_id).first<{ kind: string }>();
        if (!record) missing();
        row.person_id = record.kind === "person" ? row.record_id : null;
        if (record.kind === "company" && merged.role !== "organizer") invalid("Companies participate only as organizers.");
      } else {
        row.person_id = merged.person_id == null ? null : id(merged.person_id);
        if (row.person_id) await person(row.person_id);
        const raw = args.participants ?? (existing ? await participantsFor(recordId) : row.person_id ? [{ record_id: row.person_id }] : []);
        if (!Array.isArray(raw) || (!existing && raw.length < 1) || raw.length > 100) invalid("Provide 1 to 100 participants.");
        participants = raw.map(item => { const p = object(item, ["record_id", "role"]); return { record_id: id(p.record_id), role: p.role == null ? null : text(p.role, "participant role", 128) }; }).sort((a,b) => a.record_id < b.record_id ? -1 : a.record_id > b.record_id ? 1 : 0);
        if (new Set(participants.map(p => p.record_id)).size !== participants.length) invalid("Duplicate participant.");
        if (row.person_id && !participants.some(p => p.record_id === row.person_id)) invalid("person_id must be a participant.");
        for (const p of participants) if (!await session.prepare("SELECT id FROM crm_records WHERE owner_id=? AND id=?").bind(owner,p.record_id).first()) missing();
        if (existing && args.participants !== undefined && JSON.stringify(participants) !== JSON.stringify(await participantsFor(recordId))) invalid("Participants are immutable.");
        row.type = text(merged.type ?? "interaction", "type", 128);
        row.summary = merged.summary == null ? null : text(merged.summary, "summary", 512);
      }
      row.event_id = merged.event_id == null && kind === "interaction" ? null : id(merged.event_id);
      if (row.event_id && !await session.prepare("SELECT id FROM crm_events WHERE owner_id=? AND id=?").bind(owner, row.event_id).first()) missing();
      if (kind === "participation") {
        row.status = choice(merged.status ?? "unknown", ["invited", "expected", "attended", "declined", "unknown"], "status");
        row.role = choice(merged.role ?? "attendee", ["attendee", "organizer"], "role");
        const duplicate = await session.prepare("SELECT id FROM crm_event_participation WHERE owner_id=? AND event_id=? AND record_id=? AND id!=?").bind(owner, row.event_id, row.record_id, recordId).first();
        if (duplicate) invalid("Participation already exists; edit its id.");
      } else {
        row.meeting_id = merged.meeting_id == null ? null : id(merged.meeting_id);
        row.connection_id = merged.connection_id == null ? null : text(merged.connection_id, "connection_id", 1024);
        row.message_id = merged.message_id == null ? null : text(merged.message_id, "message_id", 1024);
        if ((row.connection_id === null) !== (row.message_id === null)) invalid("connection_id and message_id must be supplied together.");
        const ids = JSON.stringify(participants.map(p => p.record_id));
        if (!existing && row.meeting_id && !await session.prepare("SELECT meeting_id FROM crm_meeting_attendees WHERE owner_id=? AND meeting_id=? AND person_id IN (SELECT value FROM json_each(?))").bind(owner, row.meeting_id, ids).first()) invalid("Meeting must match a participant.");
        if (!existing && row.message_id && !await session.prepare("SELECT message_id FROM crm_email_imports WHERE owner_id=? AND connection_id=? AND message_id=? AND record_id IN (SELECT value FROM json_each(?))").bind(owner, row.connection_id, row.message_id, ids).first()) invalid("Email must match a participant.");
        const dateOnly = typeof merged.occurred_at === "string" && /^\d{4}-\d{2}-\d{2}$/.test(merged.occurred_at);
        const at = date(dateOnly ? `${merged.occurred_at}T00:00:00Z` : merged.occurred_at);
        row.precision = dateOnly ? "date" : "datetime";
        Object.assign(row, { occurred_at: dateOnly ? String(merged.occurred_at) : at.value, occurred_ms: at.ms, body: text(merged.body, "body", 20000, true) });
      }
    }
    const now = Date.now(), keys = Object.keys(row);
    let saved: Row | null;
    if (existing) {
      saved = await session.prepare(`UPDATE ${table} SET ${keys.map(key => `${key}=?`).join(",")},updated_at=max(updated_at,?) WHERE owner_id=? AND id=? RETURNING *`).bind(...Object.values(row), now, owner, recordId).first<Row>();
    } else {
      const insert = session.prepare(`INSERT INTO ${table}(owner_id,id,${keys.join(",")},created_at,updated_at) VALUES (${Array(keys.length + 4).fill("?").join(",")}) ON CONFLICT(owner_id,id) DO NOTHING RETURNING *`).bind(owner, recordId, ...Object.values(row), now, now);
      const result = await session.batch([insert, ...(kind === "interaction" ? [session.prepare("INSERT INTO crm_interaction_participants(owner_id,interaction_id,record_id,role) SELECT ?,?,json_extract(value,'$.record_id'),json_extract(value,'$.role') FROM json_each(?) WHERE changes()>0").bind(owner,recordId,JSON.stringify(participants))] : [])]);
      saved = result[0].results[0] as Row | undefined ?? null;
    }
    if (!saved && !existing) {
      saved = await read(recordId);
      for (const field of immutable) if (saved[field] !== row[field]) invalid(`${field} is immutable.`);
      if (kind === "interaction" && JSON.stringify(participants) !== JSON.stringify(await participantsFor(recordId))) invalid("Participants are immutable.");
    }
    if (!saved) missing();
    return { [kind]: await present(saved) };
  } catch (error) {
    if (error instanceof CrmError) throw error;
    if (error instanceof Error && /constraint|crm_invalid|crm_event_immutable/i.test(error.message)) invalid("Invalid or conflicting CRM links or record.");
    throw new Error("CRM event storage request failed.");
  }
}
export function crmEventRequest(db: D1Database, owner: string, operation: CrmEventOperation, input: unknown, createId: string): Promise<unknown> {
  return request(db, owner, "event", operation, input, createId);
}
export function crmParticipationRequest(db: D1Database, owner: string, operation: CrmParticipationOperation, input: unknown, createId: string): Promise<unknown> {
  return request(db, owner, "participation", operation, input, createId);
}
export function crmInteractionRequest(db: D1Database, owner: string, operation: CrmInteractionOperation, input: unknown, createId: string): Promise<unknown> {
  return request(db, owner, "interaction", operation, input, createId);
}
