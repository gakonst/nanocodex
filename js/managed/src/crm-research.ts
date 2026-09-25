import { CrmError } from "./crm";

/** Research never changes manual CRM fields or satisfies meeting-note status.
 * The caller must ground claims in actual source reads. This boundary validates
 * reference syntax, not whether a fetch occurred or a claim follows the source.
 * Missing or ambiguous identity must be saved as needs_review, with the reason
 * in summary; such profiles require explicit review instead of automatic retry.
 */
export type CrmResearchOperation = "queue" | "get" | "save";
export type CrmResearchSource = { kind: "web" | "email" | "calendar"; reference: string; detail?: string };
export type CrmResearchProfile = {
  record_id: string; summary: string; company: string | null; title: string | null;
  website: string | null; sources: CrmResearchSource[]; status: "complete" | "needs_review"; checked_at: number;
};
type Input = Record<string, unknown>;
type ResearchRow = Omit<CrmResearchProfile, "sources"> & { sources: string };
type QueueRow = {
  id: string; kind: "person"; name: string; email: string | null; phone: string | null;
  website: string | null; title: string | null; company_id: string | null;
  tags: string; created_at: number; updated_at: number; research: string | null;
};
type Cursor = { v: 1; scope: string; at: number; id: string; cutoff: number };
const profileColumns = "record_id,summary,company,title,website,sources,status,checked_at";
const recordColumns = "id,kind,name,email,phone,website,title,company_id,tags,created_at,updated_at";
const refreshAgeMs = 30 * 24 * 60 * 60 * 1000;
function invalid(message: string): never { throw new CrmError("invalid_input", message); }
function notFound(): never { throw new CrmError("not_found", "CRM item not found."); }
function object(value: unknown, allowed: readonly string[]): Input {
  if (value === null || typeof value !== "object" || Array.isArray(value) ||
      ![Object.prototype, null].includes(Object.getPrototypeOf(value)) || Object.getOwnPropertySymbols(value).length) invalid("Input must be an object.");
  for (const key of Object.keys(value)) if (!allowed.includes(key)) invalid(`Unknown field: ${key}.`);
  return value as Input;
}
function string(value: unknown, field: string, max: number, multiline = false): string {
  if (typeof value !== "string" || value.length > max || !value.trim() ||
      (multiline ? /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/ : /[\u0000-\u001f\u007f]/).test(value)) invalid(`Invalid ${field}.`);
  return value;
}
function id(value: unknown): string {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value)) invalid("Invalid record_id.");
  return value;
}
function url(value: unknown, field: string): string {
  const result = string(value, field, 2048);
  let parsed: URL;
  try { parsed = new URL(result); } catch { return invalid(`Invalid ${field}.`); }
  if (result !== result.trim() || !["http:", "https:"].includes(parsed.protocol) || parsed.username || parsed.password) invalid(`${field} must be an HTTP(S) URL without credentials.`);
  return result;
}
function source(value: unknown): CrmResearchSource {
  const args = object(value, ["kind", "reference", "detail"]);
  if (args.kind !== "web" && args.kind !== "email" && args.kind !== "calendar") invalid("Invalid source kind.");
  const reference = string(args.reference, "source reference", 2048);
  if (args.kind === "web") url(reference, "web source");
  else if (!/^[A-Za-z0-9][A-Za-z0-9._:-]{0,511}$/.test(reference)) {
    const parsed = new URL(url(reference, "source reference"));
    if (args.kind === "email") {
      const message = parsed.searchParams.get("th") || parsed.searchParams.get("permmsgid");
      const fragment = parsed.hash.slice(1).split("/");
      if (parsed.hostname !== "mail.google.com" || !/^\/(?:mail(?:\/|$)|$)/.test(parsed.pathname) ||
          !(message && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,511}$/.test(message)) &&
          !(fragment.length >= 2 && /^[A-Za-z0-9][A-Za-z0-9._:-]{0,511}$/.test(fragment[fragment.length - 1]))) invalid("Email sources must reference a message/thread ID or Gmail message link.");
    } else {
      const event = parsed.searchParams.get("eid");
      if (!["calendar.google.com", "www.google.com"].includes(parsed.hostname) || !parsed.pathname.startsWith("/calendar/") ||
          !(event && /^[A-Za-z0-9_-]+={0,2}$/.test(event)) && !/\/eventedit\/[A-Za-z0-9_-]+\/?$/.test(parsed.pathname)) invalid("Calendar sources must reference an event ID or Google Calendar event link.");
    }
  }
  return { kind: args.kind, reference, ...(Object.hasOwn(args, "detail") ? { detail: string(args.detail, "source detail", 2000, true) } : {}) };
}
function profile(row: ResearchRow): CrmResearchProfile { return { ...row, sources: JSON.parse(row.sources) as CrmResearchSource[] }; }
async function queueScope(ownerId: string): Promise<string> {
  const hash = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify([ownerId, "research_queue"])));
  return Array.from(new Uint8Array(hash), byte => byte.toString(16).padStart(2, "0")).join("");
}
function decodeCursor(value: unknown, expected: string): Cursor {
  if (typeof value !== "string" || !value || value.length > 4096 || !/^[A-Za-z0-9_-]+$/.test(value)) invalid("Invalid cursor.");
  try {
    const bytes = Uint8Array.from(atob(value.replace(/-/g, "+").replace(/_/g, "/")), ch => ch.charCodeAt(0));
    const args = object(JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes)), ["v", "scope", "at", "id", "cutoff"]);
    if (args.v !== 1 || args.scope !== expected || typeof args.at !== "number" || !Number.isSafeInteger(args.at) || args.at < 0 ||
        typeof args.cutoff !== "number" || !Number.isSafeInteger(args.cutoff) || args.cutoff < 0 || args.cutoff > Date.now() - refreshAgeMs) invalid("Invalid cursor.");
    return { v: 1, scope: expected, at: args.at, id: id(args.id), cutoff: args.cutoff };
  } catch { return invalid("Invalid cursor for this query."); }
}
function encodeCursor(row: QueueRow, scope: string, cutoff: number): string {
  return btoa(JSON.stringify({ v: 1, scope, at: row.created_at, id: row.id, cutoff })).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export async function crmResearchRequest(db: D1Database, ownerId: string, operation: CrmResearchOperation, input: unknown): Promise<unknown> {
  string(ownerId, "authenticated owner", 512);
  const session = db.withSession("first-primary");
  try {
    switch (operation) {
      case "queue": {
        const args = object(input, ["limit", "cursor"]);
        const size = Object.hasOwn(args, "limit") ? args.limit : 20;
        if (typeof size !== "number" || !Number.isInteger(size) || size < 1 || size > 100) invalid("Page limit must be an integer from 1 to 100.");
        const scope = await queueScope(ownerId);
        const cursor = Object.hasOwn(args, "cursor") ? decodeCursor(args.cursor, scope) : null;
        // Freeze the refresh cutoff across pages; successfully saved entries can
        // leave the queue without changing the stable record creation keyset.
        const cutoff = cursor?.cutoff ?? Date.now() - refreshAgeMs;
        const values: (string | number)[] = [ownerId, cutoff];
        if (cursor) values.push(cursor.at, cursor.at, cursor.id);
        const rows = (await session.prepare(`SELECT ${recordColumns.split(",").map(column => `r.${column}`).join(",")},
          CASE WHEN p.record_id IS NULL THEN NULL ELSE json_object(
            'record_id',p.record_id,'summary',p.summary,'company',p.company,'title',p.title,
            'website',p.website,'sources',json(p.sources),'status',p.status,'checked_at',p.checked_at) END AS research
          FROM crm_records r LEFT JOIN crm_research p ON p.owner_id = r.owner_id AND p.record_id = r.id
          WHERE r.owner_id = ? AND r.kind = 'person' AND (p.record_id IS NULL OR (p.status = 'complete' AND p.checked_at < ?))
          ${cursor ? "AND (r.created_at > ? OR (r.created_at = ? AND r.id > ?))" : ""}
          ORDER BY r.created_at,r.id LIMIT ?`).bind(...values, size + 1).all<QueueRow>()).results;
        const items = rows.slice(0, size);
        return {
          records: items.map(row => ({ ...row, tags: JSON.parse(row.tags) as string[], research: row.research === null ? null : JSON.parse(row.research) as CrmResearchProfile })),
          next_cursor: rows.length > size ? encodeCursor(items[items.length - 1], scope, cutoff) : null,
        };
      }
      case "get": {
        const args = object(input, ["record_id"]);
        const recordId = id(args.record_id);
        const results = await session.batch([
          session.prepare("SELECT id FROM crm_records WHERE owner_id = ? AND id = ?").bind(ownerId, recordId),
          session.prepare(`SELECT ${profileColumns} FROM crm_research WHERE owner_id = ? AND record_id = ?`).bind(ownerId, recordId),
        ]);
        if (!results[0].results.length) notFound();
        const row = results[1].results[0] as ResearchRow | undefined;
        return { research: row ? profile(row) : null };
      }
      case "save": {
        const args = object(input, ["record_id", "summary", "company", "title", "website", "sources", "status"]);
        const recordId = id(args.record_id);
        const summary = string(args.summary, "summary", 20000, true);
        const company = args.company === null || !Object.hasOwn(args, "company") ? null : string(args.company, "company", 512).trim();
        const title = args.title === null || !Object.hasOwn(args, "title") ? null : string(args.title, "title", 512).trim();
        const website = args.website === null || !Object.hasOwn(args, "website") ? null : url(args.website, "website");
        if (args.status !== "complete" && args.status !== "needs_review") invalid("status must be complete or needs_review.");
        if (!Array.isArray(args.sources) || args.sources.length > 50 || (args.status === "complete" && !args.sources.length)) invalid("sources must contain at most 50 references; complete research requires at least one.");
        const sources = JSON.stringify(args.sources.map(source));
        // Parent existence and profile replacement happen in one statement, so
        // deletion cannot race a preflight check into creating an orphan.
        const row = await session.prepare(`INSERT INTO crm_research (owner_id,${profileColumns})
          SELECT ?,?,?,?,?,?,?,?,? FROM crm_records WHERE owner_id = ? AND id = ?
          ON CONFLICT (owner_id,record_id) DO UPDATE SET summary=excluded.summary,company=excluded.company,title=excluded.title,
            website=excluded.website,sources=excluded.sources,status=excluded.status,checked_at=max(crm_research.checked_at,excluded.checked_at)
          RETURNING ${profileColumns}`).bind(ownerId, recordId, summary, company, title, website, sources, args.status, Date.now(), ownerId, recordId).first<ResearchRow>();
        if (!row) notFound();
        return { research: profile(row) };
      }
      default: return invalid("Unknown CRM research operation.");
    }
  } catch (error) {
    if (error instanceof CrmError) throw error;
    // D1 diagnostics may include private bound values; expose no SQL or data.
    throw new Error("CRM research storage request failed.");
  }
}
