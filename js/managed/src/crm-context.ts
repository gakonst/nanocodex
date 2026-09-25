import { CrmError } from "./crm";

/** Provenance describes an assertion, not verified truth. Inferences always
 * retain confidence and rationale; callers must actually read cited sources.
 * Human and researched assertions coexist, and edits require an explicit ID. */
export type CrmContextOperation = "list" | "save" | "delete";
type Input = Record<string, unknown>;
type SqlValue = string | number | null;
type Row = Record<string, SqlValue> & { id: string; created_at: number; sources: string };
type Cursor = { v: 1; scope: string; at: number; id: string };
const common = ["origin", "sources", "confidence", "rationale", "effective_from", "effective_to"];
const factFields = ["record_id", "predicate", "value", ...common, "state"];
const relationshipFields = ["from_id", "to_id", "type", "role", "description", ...common];
const types = ["works_at", "worked_at", "knows", "worked_with", "referred"];
const encoder = new TextEncoder();
const has = (input: Input, key: string) => Object.hasOwn(input, key);
function invalid(message: string): never { throw new CrmError("invalid_input", message); }
function missing(): never { throw new CrmError("not_found", "CRM item not found."); }
function object(value: unknown, allowed?: readonly string[]): Input {
  if (value === null || typeof value !== "object" || Array.isArray(value) ||
      ![Object.prototype, null].includes(Object.getPrototypeOf(value)) || Object.getOwnPropertySymbols(value).length) invalid("Input must be a plain object.");
  if (allowed && Object.keys(value).some(key => !allowed.includes(key))) invalid("Unknown input field.");
  return value as Input;
}
function unicode(value: string): boolean { return !/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(value); }
function string(value: unknown, field: string, max: number, multiline = false): string {
  if (typeof value !== "string" || !value.trim() || value.length > max || !unicode(value) ||
      (multiline ? /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/ : /[\u0000-\u001f\u007f]/).test(value)) invalid(`Invalid ${field}.`);
  return value;
}
function id(value: unknown): string {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value)) invalid("Invalid ID.");
  return value;
}
function enumeration(value: unknown, choices: string[], field: string): string {
  if (typeof value !== "string" || !choices.includes(value)) invalid(`Invalid ${field}.`);
  return value;
}
function predicate(value: unknown): string {
  if (typeof value !== "string" || value.length > 128 || !/^[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)+$/.test(value)) invalid("predicate must be a bounded dotted name.");
  return value;
}
function date(value: unknown): string | null {
  if (value === null) return null;
  if (typeof value !== "string" || !/^\d{4}-\d{2}-\d{2}$/.test(value) || value.startsWith("0000") ||
      !Number.isFinite(Date.parse(value)) || new Date(value).toISOString().slice(0, 10) !== value) invalid("Dates must be valid YYYY-MM-DD values.");
  return value;
}
function json(value: unknown): string {
  // Reject lossy JS serialization (NaN, undefined, sparse arrays, non-JSON
  // objects, cycles and invalid Unicode), and bound traversal before encoding.
  let nodes = 0, bytes = 0;
  function spend(size: number): void {
    bytes += size;
    if (bytes > 16384) invalid("JSON value exceeds 16 KiB.");
  }
  function visit(item: unknown, depth: number): void {
    if (++nodes > 16384 || depth > 32) invalid("JSON value is too complex.");
    if (item === null || typeof item === "boolean") { spend(JSON.stringify(item).length); return; }
    if (typeof item === "string") {
      if (!unicode(item) || item.length > 16384) invalid("Invalid JSON string.");
      spend(encoder.encode(JSON.stringify(item)).length); return;
    }
    if (typeof item === "number" && Number.isFinite(item)) { spend(JSON.stringify(item).length); return; }
    if (Array.isArray(item)) {
      if (item.length > 16384 || Object.keys(item).length !== item.length || Object.getOwnPropertySymbols(item).length) invalid("Invalid JSON array.");
      spend(2 + Math.max(0, item.length - 1));
      for (let i = 0; i < item.length; i++) visit(item[i], depth + 1);
      return;
    }
    const entries = Object.entries(object(item));
    spend(2 + Math.max(0, entries.length - 1));
    for (const [key, child] of entries) {
      if (!unicode(key) || key.length > 16384) invalid("Invalid JSON key.");
      spend(encoder.encode(JSON.stringify(key)).length + 1);
      visit(child, depth + 1);
    }
  }
  visit(value, 0);
  const encoded = JSON.stringify(value);
  if (encoder.encode(encoded).length > 16384) invalid("JSON value exceeds 16 KiB.");
  return encoded;
}
function sources(value: unknown): string {
  if (!Array.isArray(value) || value.length > 50) invalid("sources must contain at most 50 references.");
  return json(value.map(item => {
    const args = object(item, ["kind", "reference", "detail"]);
    const kind = enumeration(args.kind, ["web", "email", "calendar", "document", "user"], "source kind");
    const reference = string(args.reference, "source reference", 2048);
    if (kind === "web" || /^[a-z][a-z0-9+.-]*:\/\//i.test(reference)) {
      let parsed: URL;
      try { parsed = new URL(reference); } catch { return invalid("Invalid source URL."); }
      if (reference !== reference.trim() || !["http:", "https:"].includes(parsed.protocol) || parsed.username || parsed.password) invalid("Source URLs must use HTTP(S) without credentials.");
    }
    return { kind, reference, ...(has(args, "detail") ? { detail: string(args.detail, "source detail", 2000, true) } : {}) };
  }));
}
function patch(args: Input, fields: string[]): Record<string, SqlValue> {
  const result: Record<string, SqlValue> = {};
  for (const field of fields) if (has(args, field)) {
    const value = args[field];
    if (["record_id", "from_id", "to_id"].includes(field)) result[field] = id(value);
    else if (field === "value") result.value_json = json(value);
    else if (field === "predicate") result[field] = predicate(value);
    else if (field === "sources") result[field] = sources(value);
    else if (field === "origin") result[field] = enumeration(value, ["user", "source", "inferred"], field);
    else if (field === "confidence") result[field] = value === null ? null : enumeration(value, ["low", "medium", "high"], field);
    else if (field === "state") result[field] = enumeration(value, ["current", "superseded"], field);
    else if (field === "type") result[field] = enumeration(value, types, field);
    else if (field.startsWith("effective_")) result[field] = date(value);
    else result[field] = value === null ? null : string(value, field, field === "role" ? 512 : 2000, field !== "role");
  }
  return result;
}
function decode(row: Row) {
  const { value_json, ...rest } = row;
  return { ...rest, sources: JSON.parse(row.sources), ...(value_json !== undefined ? { value: JSON.parse(value_json as string) } : {}) };
}
async function scope(parts: unknown[]): Promise<string> {
  const hash = await crypto.subtle.digest("SHA-256", encoder.encode(JSON.stringify(parts)));
  return Array.from(new Uint8Array(hash), byte => byte.toString(16).padStart(2, "0")).join("");
}
function cursor(value: unknown, expected: string): Cursor {
  if (typeof value !== "string" || !value || value.length > 4096 || !/^[A-Za-z0-9_-]+$/.test(value)) invalid("Invalid cursor.");
  try {
    const args = object(JSON.parse(atob(value.replace(/-/g, "+").replace(/_/g, "/"))), ["v", "scope", "at", "id"]);
    if (args.v !== 1 || args.scope !== expected || typeof args.at !== "number" || !Number.isSafeInteger(args.at) || args.at < 0) invalid("Invalid cursor.");
    return { v: 1, scope: expected, at: args.at, id: id(args.id) };
  } catch { return invalid("Invalid cursor for this query."); }
}

async function request(db: D1Database, ownerId: string, operation: CrmContextOperation, input: unknown, createId: string, facts: boolean): Promise<unknown> {
  string(ownerId, "authenticated owner", 512);
  const session = db.withSession("first-primary");
  const table = facts ? "crm_facts" : "crm_relationships";
  const fields = facts ? factFields : relationshipFields;
  const columns = ["id", ...fields.map(field => field === "value" ? "value_json" : field), "created_at", "updated_at"].join(",");
  const singular = facts ? "fact" : "relationship";
  try {
    if (operation === "list") {
      const filter = facts ? "predicate" : "type";
      const args = object(input, ["record_id", filter, "limit", "cursor"]);
      const recordId = has(args, "record_id") ? id(args.record_id) : null;
      const selection = has(args, filter) ? facts ? predicate(args[filter]) : enumeration(args[filter], types, filter) : null;
      const size = has(args, "limit") ? args.limit : 20;
      if (typeof size !== "number" || !Number.isInteger(size) || size < 1 || size > 100) invalid("Page limit must be an integer from 1 to 100.");
      const queryScope = await scope([ownerId, table, recordId, selection]);
      const after = has(args, "cursor") ? cursor(args.cursor, queryScope) : null;
      const conditions = ["owner_id = ?"], values: SqlValue[] = [ownerId];
      if (recordId) {
        conditions.push(facts ? "record_id = ?" : "(from_id = ? OR to_id = ?)");
        values.push(recordId); if (!facts) values.push(recordId);
      }
      if (selection) { conditions.push(`${filter} = ?`); values.push(selection); }
      if (after) { conditions.push("(created_at > ? OR (created_at = ? AND id > ?))"); values.push(after.at, after.at, after.id); }
      const rows = (await session.prepare(`SELECT ${columns} FROM ${table} WHERE ${conditions.join(" AND ")} ORDER BY created_at,id LIMIT ?`).bind(...values, size + 1).all<Row>()).results;
      const items = rows.slice(0, size), last = items[items.length - 1];
      const next = rows.length > size ? btoa(JSON.stringify({ v: 1, scope: queryScope, at: last.created_at, id: last.id })).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "") : null;
      return { [facts ? "facts" : "relationships"]: items.map(decode), next_cursor: next };
    }
    if (operation === "delete") {
      const args = object(input, ["id"]);
      const row = await session.prepare(`DELETE FROM ${table} WHERE owner_id = ? AND id = ? RETURNING id`).bind(ownerId, id(args.id)).first();
      if (!row) missing();
      return { deleted: true };
    }
    if (operation !== "save") invalid("Unknown CRM context operation.");
    const args = object(input, ["id", ...fields]);
    const changes = patch(args, fields), now = Date.now();
    if (has(args, "id")) {
      // SQL constraints validate the resulting row atomically, including partial
      // date/provenance edits; triggers preserve its parent/endpoints and origin.
      const row = await session.prepare(`UPDATE ${table} SET ${Object.keys(changes).map(field => `${field} = ?, `).join("")}updated_at = max(updated_at, ?) WHERE owner_id = ? AND id = ? RETURNING ${columns}`).bind(...Object.values(changes), now, ownerId, id(args.id)).first<Row>();
      if (!row) missing();
      return { [singular]: decode(row) };
    }
    const required = facts ? ["record_id", "predicate", "value", "origin"] : ["from_id", "to_id", "type", "origin"];
    if (required.some(field => !has(args, field))) invalid("Missing required CRM context fields.");
    const itemId = id(createId);
    const defaults: Record<string, SqlValue> = { sources: "[]", confidence: null, rationale: null, effective_from: null, effective_to: null, ...(facts ? { state: "current" } : { role: null, description: null }) };
    const values = { ...defaults, ...changes };
    // Host IDs make retries idempotent. No predicate/edge upsert can overwrite
    // an independent human assertion, even when another origin disagrees.
    const results = await session.batch([
      session.prepare(`INSERT INTO ${table} (owner_id,id,${Object.keys(values).join(",")},created_at,updated_at)
        SELECT ${Array(Object.keys(values).length + 4).fill("?").join(",")} WHERE NOT EXISTS (SELECT 1 FROM ${table} WHERE owner_id = ? AND id = ?)
        ON CONFLICT (owner_id,id) DO NOTHING`).bind(ownerId, itemId, ...Object.values(values), now, now, ownerId, itemId),
      session.prepare(`SELECT ${columns} FROM ${table} WHERE owner_id = ? AND id = ?`).bind(ownerId, itemId),
    ]);
    return { [singular]: decode(results[1].results[0] as Row) };
  } catch (error) {
    if (error instanceof CrmError) throw error;
    if (error instanceof Error && /crm_context_immutable|crm_context_kind|FOREIGN KEY constraint failed|CHECK constraint failed|NOT NULL constraint failed/.test(error.message)) invalid("Invalid CRM context, provenance, or immutable field change.");
    // D1 errors may contain SQL and bound values; never forward those details.
    throw new Error("CRM context storage request failed.");
  }
}
export function crmFactRequest(db: D1Database, ownerId: string, operation: CrmContextOperation, input: unknown, createId: string): Promise<unknown> {
  return request(db, ownerId, operation, input, createId, true);
}
export function crmRelationshipRequest(db: D1Database, ownerId: string, operation: CrmContextOperation, input: unknown, createId: string): Promise<unknown> {
  return request(db, ownerId, operation, input, createId, false);
}
