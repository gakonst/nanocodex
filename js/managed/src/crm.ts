/** Account-private CRM persistence. The caller supplies the authenticated owner
 * and a stable, host-generated create ID for retrying the same invocation. */
export type CrmOperation = "search" | "get" | "save" | "delete" | "save_note" | "delete_note";

type Input = Record<string, unknown>;
type Kind = "person" | "company";
type RecordRow = {
  id: string; kind: Kind; name: string; email: string | null; phone: string | null;
  website: string | null; title: string | null; company_id: string | null;
  tags: string; created_at: number; updated_at: number;
};
type NoteRow = { id: string; record_id: string; body: string; source_url: string | null; created_at: number; updated_at: number };
type Cursor = { v: 1; scope: string; at: number; id: string };
type SqlValue = string | number | null;
const recordColumns = "id,kind,name,email,phone,website,title,company_id,tags,created_at,updated_at";
// Keep manual columns intact. Complete research fills empty display fields;
// a company ID additionally requires an explicit current employment edge.
const sourcedCompanyId = `(SELECT CASE WHEN count(DISTINCT l.to_id) = 1 THEN min(l.to_id) END
  FROM crm_relationships l JOIN crm_records c ON c.owner_id = l.owner_id AND c.id = l.to_id AND c.kind = 'company'
  WHERE l.owner_id = r.owner_id AND l.from_id = r.id AND l.type = 'works_at'
    AND l.origin IN ('user','source') AND (l.effective_to IS NULL OR l.effective_to >= date('now'))
    AND p.status = 'complete' AND p.company IS NOT NULL AND lower(trim(c.name)) = lower(trim(p.company)))`;
const effectiveColumns = `r.id,r.kind,r.name,r.email,r.phone,
  coalesce(r.website,CASE WHEN p.status = 'complete' THEN p.website END) AS website,
  coalesce(r.title,CASE WHEN p.status = 'complete' THEN p.title END) AS title,
  coalesce(r.company_id,${sourcedCompanyId}) AS company_id,
  r.tags,r.created_at,r.updated_at,
  CASE WHEN r.website IS NULL AND p.status = 'complete' AND p.website IS NOT NULL THEN 'research' END AS website_origin,
  CASE WHEN r.title IS NULL AND p.status = 'complete' AND p.title IS NOT NULL THEN 'research' END AS title_origin,
  CASE WHEN r.company_id IS NULL AND ${sourcedCompanyId} IS NOT NULL THEN 'relationship' END AS company_id_origin`;
const effectiveJoin = `FROM crm_records r LEFT JOIN crm_research p ON p.owner_id = r.owner_id AND p.record_id = r.id`;
const noteColumns = "id,record_id,body,source_url,created_at,updated_at";
const recordFields = ["kind", "name", "email", "phone", "website", "title", "company_id", "tags"] as const;
const noteFields = ["record_id", "body", "source_url"] as const;

export class CrmError extends Error {
  constructor(readonly code: "invalid_input" | "not_found", message: string) { super(message); this.name = "CrmError"; }
}
function invalid(message: string): never { throw new CrmError("invalid_input", message); }
function notFound(): never { throw new CrmError("not_found", "CRM item not found."); }
function object(value: unknown, allowed: readonly string[]): Input {
  if (value === null || typeof value !== "object" || Array.isArray(value) ||
      ![Object.prototype, null].includes(Object.getPrototypeOf(value)) || Object.getOwnPropertySymbols(value).length) invalid("Input must be an object.");
  for (const key of Object.keys(value)) if (!allowed.includes(key)) invalid(`Unknown field: ${key}.`);
  return value as Input;
}
function has(input: Input, key: string): boolean { return Object.hasOwn(input, key); }
function string(value: unknown, field: string, max: number, multiline = false, empty = false): string {
  if (typeof value !== "string" || value.length > max || (!empty && value.trim().length === 0) ||
      (multiline ? /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/ : /[\u0000-\u001f\u007f]/).test(value)) invalid(`Invalid ${field}.`);
  return value;
}
function id(value: unknown, field = "id"): string {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value)) invalid(`Invalid ${field}.`);
  return value;
}
function kind(value: unknown): Kind {
  if (value !== "person" && value !== "company") invalid("kind must be person or company.");
  return value;
}
function limit(value: unknown): number {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 1 || value > 100) invalid("Page limit must be an integer from 1 to 100.");
  return value;
}
function url(value: unknown, field: string): string {
  const result = string(value, field, 2048);
  let parsed: URL;
  try { parsed = new URL(result); } catch { return invalid(`Invalid ${field}.`); }
  if (!["https:", "http:"].includes(parsed.protocol) || parsed.username || parsed.password) invalid(`${field} must be an HTTP(S) URL without credentials.`);
  return result;
}
function tags(value: unknown): string {
  if (!Array.isArray(value) || value.length > 100) invalid("tags must be an array of at most 100 strings.");
  return JSON.stringify([...new Set(value.map(tag => string(tag, "tag", 64).trim()))]);
}
function recordPatch(input: Input): Record<string, SqlValue> {
  const patch: Record<string, SqlValue> = {};
  for (const field of recordFields) if (has(input, field)) {
    const value = input[field];
    if (field === "kind") patch[field] = kind(value);
    else if (field === "name") patch[field] = string(value, field, 512).trim();
    else if (field === "tags") patch[field] = tags(value);
    else if (value === null) patch[field] = null;
    else if (field === "company_id") patch[field] = id(value, field);
    else if (field === "website") patch[field] = url(value, field);
    else patch[field] = string(value, field, 512).trim();
  }
  return patch;
}
function notePatch(input: Input): Record<string, SqlValue> {
  const patch: Record<string, SqlValue> = {};
  if (has(input, "record_id")) patch.record_id = id(input.record_id, "record_id");
  if (has(input, "body")) patch.body = string(input.body, "body", 20000, true);
  if (has(input, "source_url")) patch.source_url = input.source_url === null ? null : url(input.source_url, "source_url");
  return patch;
}
type EffectiveRecordRow = RecordRow & { website_origin?: string | null; title_origin?: string | null; company_id_origin?: string | null };
function record(row: EffectiveRecordRow) {
  const { website_origin, title_origin, company_id_origin, ...fields } = row;
  const field_origins = { ...(website_origin ? { website: website_origin } : {}),
    ...(title_origin ? { title: title_origin } : {}),
    ...(company_id_origin ? { company_id: company_id_origin } : {}) };
  return { ...fields, tags: JSON.parse(row.tags) as string[], ...(Object.keys(field_origins).length ? { field_origins } : {}) };
}
async function scope(parts: unknown[]): Promise<string> {
  const hash = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify(parts)));
  return Array.from(new Uint8Array(hash), byte => byte.toString(16).padStart(2, "0")).join("");
}
function decodeCursor(value: unknown, expected: string): Cursor {
  if (typeof value !== "string" || value.length === 0 || value.length > 4096 || !/^[A-Za-z0-9_-]+$/.test(value)) invalid("Invalid cursor.");
  try {
    const bytes = Uint8Array.from(atob(value.replace(/-/g, "+").replace(/_/g, "/")), ch => ch.charCodeAt(0));
    const input = object(JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes)), ["v", "scope", "at", "id"]);
    if (input.v !== 1 || input.scope !== expected || typeof input.at !== "number" || !Number.isSafeInteger(input.at) || input.at < 0) invalid("Invalid cursor.");
    return { v: 1, scope: expected, at: input.at, id: id(input.id) };
  } catch { return invalid("Invalid cursor for this query."); }
}
function encodeCursor(row: { created_at: number; id: string }, queryScope: string): string {
  return btoa(JSON.stringify({ v: 1, scope: queryScope, at: row.created_at, id: row.id })).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
function page<T extends { created_at: number; id: string }>(rows: T[], size: number, queryScope: string) {
  const more = rows.length > size;
  const items = rows.slice(0, size);
  return { items, next_cursor: more ? encodeCursor(items[items.length - 1], queryScope) : null };
}

export async function crmRequest(db: D1Database, ownerId: string, operation: CrmOperation, input: unknown, createId: string): Promise<unknown> {
  string(ownerId, "authenticated owner", 512);
  // Each call starts on the primary; subsequent queries observe its writes even
  // when the production database has read replication enabled.
  const session = db.withSession("first-primary");
  try {
    switch (operation) {
      case "search": {
        const args = object(input, ["q", "kind", "tag", "company_id", "limit", "cursor"]);
        const q = has(args, "q") ? string(args.q, "q", 512, false, true) : "";
        const filterKind = has(args, "kind") ? kind(args.kind) : null;
        const tag = has(args, "tag") ? string(args.tag, "tag", 64).trim() : null;
        const companyId = has(args, "company_id") ? id(args.company_id, "company_id") : null;
        const size = has(args, "limit") ? limit(args.limit) : 20;
        const queryScope = await scope([ownerId, "search", q, filterKind, tag, companyId]);
        const cursor = has(args, "cursor") ? decodeCursor(args.cursor, queryScope) : null;
        const conditions = ["r.owner_id = ?"];
        const values: SqlValue[] = [ownerId];
        if (filterKind) { conditions.push("r.kind = ?"); values.push(filterKind); }
        if (tag) { conditions.push("EXISTS (SELECT 1 FROM json_each(r.tags) WHERE value = ?)"); values.push(tag); }
        if (companyId) { conditions.push(`coalesce(r.company_id,${sourcedCompanyId}) = ?`); values.push(companyId); }
        if (q) {
          // instr treats %, _ and backslashes literally; no LIKE metacharacters.
          conditions.push(`(instr(lower(r.name), lower(?)) > 0 OR instr(lower(coalesce(r.email,'')), lower(?)) > 0
            OR instr(lower(coalesce(r.phone,'')), lower(?)) > 0 OR instr(lower(coalesce(r.website,'')), lower(?)) > 0
            OR instr(lower(coalesce(r.title,'')), lower(?)) > 0
            OR EXISTS (SELECT 1 FROM json_each(r.tags) WHERE instr(lower(value), lower(?)) > 0)
            OR EXISTS (SELECT 1 FROM crm_notes n WHERE n.owner_id = ? AND n.record_id = r.id AND
              (instr(lower(n.body), lower(?)) > 0 OR instr(lower(coalesce(n.source_url,'')), lower(?)) > 0))
            OR EXISTS (SELECT 1 FROM crm_records c WHERE c.owner_id = ? AND c.id = r.company_id AND instr(lower(c.name), lower(?)) > 0)
            OR EXISTS (SELECT 1 FROM crm_research p WHERE p.owner_id = r.owner_id AND p.record_id = r.id AND
              (instr(lower(p.summary), lower(?)) > 0 OR instr(lower(coalesce(p.company,'')), lower(?)) > 0
               OR instr(lower(coalesce(p.title,'')), lower(?)) > 0 OR instr(lower(coalesce(p.website,'')), lower(?)) > 0))
            OR EXISTS (SELECT 1 FROM crm_identities i WHERE i.owner_id=r.owner_id AND i.record_id=r.id AND
              (instr(lower(i.value),lower(?))>0 OR instr(lower(i.normalized),lower(?))>0))
            OR EXISTS (SELECT 1 FROM crm_facts f WHERE f.owner_id=r.owner_id AND f.record_id=r.id AND f.state='current' AND
              (instr(lower(f.predicate),lower(?))>0 OR instr(lower(f.value_json),lower(?))>0))
            OR EXISTS (SELECT 1 FROM crm_relationships l JOIN crm_records other ON other.owner_id=l.owner_id AND
              other.id=CASE WHEN l.from_id=r.id THEN l.to_id ELSE l.from_id END
              WHERE l.owner_id=r.owner_id AND (l.from_id=r.id OR l.to_id=r.id) AND
              (instr(lower(other.name),lower(?))>0 OR instr(lower(coalesce(l.role,'')),lower(?))>0 OR instr(lower(coalesce(l.description,'')),lower(?))>0)))`);
          values.push(q, q, q, q, q, q, ownerId, q, q, ownerId, q, q, q, q, q, q, q, q, q, q, q, q);
        }
        if (cursor) { conditions.push("(r.created_at > ? OR (r.created_at = ? AND r.id > ?))"); values.push(cursor.at, cursor.at, cursor.id); }
        const result = await session.prepare(`SELECT ${effectiveColumns} ${effectiveJoin} WHERE ${conditions.join(" AND ")} ORDER BY r.created_at, r.id LIMIT ?`).bind(...values, size + 1).all<EffectiveRecordRow>();
        const resultPage = page(result.results, size, queryScope);
        return { records: resultPage.items.map(record), next_cursor: resultPage.next_cursor };
      }
      case "get": {
        const args = object(input, ["id", "notes_limit", "notes_cursor"]);
        const recordId = id(args.id);
        const size = has(args, "notes_limit") ? limit(args.notes_limit) : 20;
        const queryScope = await scope([ownerId, "notes", recordId]);
        const cursor = has(args, "notes_cursor") ? decodeCursor(args.notes_cursor, queryScope) : null;
        const values: SqlValue[] = [ownerId, recordId];
        if (cursor) values.push(cursor.at, cursor.at, cursor.id);
        const results = await session.batch([
          session.prepare(`SELECT ${effectiveColumns} ${effectiveJoin} WHERE r.owner_id = ? AND r.id = ?`).bind(ownerId, recordId),
          session.prepare(`SELECT ${noteColumns} FROM crm_notes WHERE owner_id = ? AND record_id = ? ${cursor ? "AND (created_at > ? OR (created_at = ? AND id > ?))" : ""} ORDER BY created_at, id LIMIT ?`).bind(...values, size + 1),
        ]);
        const row = results[0].results[0] as EffectiveRecordRow | undefined;
        if (!row) notFound();
        const notes = page(results[1].results as NoteRow[], size, queryScope);
        return { record: record(row), notes: notes.items, next_cursor: notes.next_cursor };
      }
      case "save": {
        const args = object(input, ["id", ...recordFields]);
        const patch = recordPatch(args);
        const now = Date.now();
        if (has(args, "id")) {
          const recordId = id(args.id);
          const fields = Object.keys(patch);
          const row = await session.prepare(`UPDATE crm_records SET ${fields.map(field => `${field} = ?, `).join("")}updated_at = max(updated_at, ?) WHERE owner_id = ? AND id = ? RETURNING ${recordColumns}`).bind(...Object.values(patch), now, ownerId, recordId).first<RecordRow>();
          if (!row) notFound();
          const visible = await session.prepare(`SELECT ${effectiveColumns} ${effectiveJoin} WHERE r.owner_id = ? AND r.id = ?`)
            .bind(ownerId, recordId).first<EffectiveRecordRow>();
          return { record: record(visible!) };
        }
        if (!has(args, "kind") || !has(args, "name")) invalid("Creating a record requires kind and name.");
        const recordId = id(createId, "create ID");
        const results = await session.batch([
          session.prepare(`INSERT INTO crm_records (owner_id,id,${recordColumns.split(",").slice(1).join(",")})
            SELECT ?,?,?,?,?,?,?,?,?,?,?,? WHERE NOT EXISTS (SELECT 1 FROM crm_records WHERE owner_id = ? AND id = ?)
            ON CONFLICT (owner_id,id) DO NOTHING`).bind(ownerId, recordId, patch.kind, patch.name, patch.email ?? null, patch.phone ?? null, patch.website ?? null, patch.title ?? null, patch.company_id ?? null, patch.tags ?? "[]", now, now, ownerId, recordId),
          session.prepare(`SELECT ${recordColumns} FROM crm_records WHERE owner_id = ? AND id = ?`).bind(ownerId, recordId),
        ]);
        const visible = await session.prepare(`SELECT ${effectiveColumns} ${effectiveJoin} WHERE r.owner_id = ? AND r.id = ?`)
          .bind(ownerId, recordId).first<EffectiveRecordRow>();
        return { record: record(visible!) };
      }
      case "save_note": {
        const args = object(input, ["id", ...noteFields]);
        const patch = notePatch(args);
        const now = Date.now();
        if (has(args, "id")) {
          const noteId = id(args.id);
          const fields = Object.keys(patch);
          const row = await session.prepare(`UPDATE crm_notes SET ${fields.map(field => `${field} = ?, `).join("")}updated_at = max(updated_at, ?) WHERE owner_id = ? AND id = ? RETURNING ${noteColumns}`).bind(...Object.values(patch), now, ownerId, noteId).first<NoteRow>();
          if (!row) notFound();
          return { note: row };
        }
        if (!has(args, "record_id") || !has(args, "body")) invalid("Creating a note requires record_id and body.");
        const noteId = id(createId, "create ID");
        const results = await session.batch([
          session.prepare(`INSERT INTO crm_notes (owner_id,id,record_id,body,source_url,created_at,updated_at)
            SELECT ?,?,?,?,?,?,? WHERE NOT EXISTS (SELECT 1 FROM crm_notes WHERE owner_id = ? AND id = ?)
            ON CONFLICT (owner_id,id) DO NOTHING`).bind(ownerId, noteId, patch.record_id, patch.body, patch.source_url ?? null, now, now, ownerId, noteId),
          session.prepare(`SELECT ${noteColumns} FROM crm_notes WHERE owner_id = ? AND id = ?`).bind(ownerId, noteId),
        ]);
        return { note: results[1].results[0] as NoteRow };
      }
      case "delete": {
        const args = object(input, ["id"]);
        const recordId = id(args.id);
        // D1 batch is a transaction: concurrent links either precede the unlink
        // or fail their FK/trigger after deletion. Notes cascade in that batch.
        const results = await session.batch([
          session.prepare("UPDATE crm_records SET company_id = NULL, updated_at = max(updated_at, ?) WHERE owner_id = ? AND company_id = ?").bind(Date.now(), ownerId, recordId),
          session.prepare("DELETE FROM crm_records WHERE owner_id = ? AND id = ? RETURNING id").bind(ownerId, recordId),
        ]);
        if (!results[1].results.length) notFound();
        return { id: recordId, deleted: true };
      }
      case "delete_note": {
        const args = object(input, ["id"]);
        const noteId = id(args.id);
        const row = await session.prepare("DELETE FROM crm_notes WHERE owner_id = ? AND id = ? RETURNING id").bind(ownerId, noteId).first();
        if (!row) notFound();
        return { id: noteId, deleted: true };
      }
      default: return invalid("Unknown CRM operation.");
    }
  } catch (error) {
    if (error instanceof CrmError) throw error;
    const message = error instanceof Error ? error.message : "";
    if (/crm_invalid_company|crm_immutable_kind|crm_immutable_record|FOREIGN KEY constraint failed|CHECK constraint failed/.test(message)) invalid("Invalid CRM link or immutable field change.");
    // D1 diagnostics can contain bound values. Keep account data and SQL out of
    // the tool-visible exception while allowing infrastructure failures to fail.
    throw new Error("CRM storage request failed.");
  }
}
