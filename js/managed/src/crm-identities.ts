import { Buffer } from "node:buffer";
import { createHash } from "node:crypto";
import { CrmError } from "./crm";

export type CrmIdentityOperation = "list" | "save" | "delete";
type Input = Record<string, unknown>;
type Row = { id: string; record_id: string; kind: string; value: string; normalized: string;
  origin: "user" | "source"; source_ref: string | null; created_at: number; updated_at: number };
const columns = "id,record_id,kind,value,normalized,origin,source_ref,created_at,updated_at";
const kinds = ["email", "github", "x", "linkedin", "telegram", "website", "domain", "aka"];
function invalid(message: string): never { throw new CrmError("invalid_input", message); }
function missing(): never { throw new CrmError("not_found", "CRM record or identity not found."); }
function object(value: unknown, allowed: readonly string[]): Input {
  if (!value || typeof value !== "object" || Array.isArray(value) || ![Object.prototype, null].includes(Object.getPrototypeOf(value)) || Object.getOwnPropertySymbols(value).length) invalid("Input must be an object.");
  if (Object.keys(value).some(key => !allowed.includes(key))) invalid("Unknown identity field.");
  return value as Input;
}
function text(value: unknown, field: string, max = 2048): string {
  if (typeof value !== "string" || !value.trim() || value.length > max || /[\u0000-\u001f\u007f]/.test(value)) invalid(`Invalid ${field}.`);
  return value.trim();
}
function id(value: unknown): string {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(value)) invalid("Invalid id.");
  return value;
}
function hostname(value: string): string {
  const host = value.toLowerCase().replace(/^www\./, "").replace(/\.$/, "");
  if (host.length > 253 || !host.includes(".") || !host.split(".").every(label => /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(label))) invalid("Invalid hostname.");
  return host;
}
function url(value: string): URL {
  if (!/^https?:\/\//i.test(value) || /[\s\\]/.test(value)) invalid("Expected an HTTP(S) URL.");
  let parsed: URL;
  try { parsed = new URL(value); } catch { return invalid("Invalid URL."); }
  if (parsed.username || parsed.password) invalid("URL credentials are not allowed.");
  parsed.hostname = hostname(parsed.hostname);
  return parsed;
}
function source(value: unknown): string {
  const reference = text(value, "source_ref");
  if (/^https?:\/\//i.test(reference)) { url(reference); return reference; }
  if (!/^[A-Za-z0-9][A-Za-z0-9._:-]{0,2047}$/.test(reference) || /^(?:javascript|data|file|ftp|mailto):/i.test(reference)) invalid("source_ref must be an HTTP(S) URL or opaque identifier.");
  return reference;
}
function canonical(kind: string, value: unknown): string {
  const raw = text(value, "value");
  if (kind === "aka") return raw.toLowerCase().replace(/\s+/g, " ");
  if (kind === "domain") return hostname(raw);
  if (kind === "email") {
    const normalized = raw.toLowerCase();
    const match = /^([a-z0-9.!#$%&'*+/=?^_`{|}~-]+)@([^@]+)$/.exec(normalized);
    if (normalized.length > 254 || !match || match[1].length > 64 || match[1].startsWith(".") || match[1].endsWith(".") || match[1].includes("..")) invalid("Invalid email.");
    // Validate the domain without changing the email's identity (including www).
    hostname(match[2]);
    return normalized;
  }
  if (kind === "website") {
    const parsed = url(raw);
    parsed.hash = "";
    for (const key of [...parsed.searchParams.keys()]) if (/^utm_/i.test(key) || /^(?:fbclid|gclid|dclid|msclkid|mc_cid|mc_eid|igshid)$/i.test(key)) parsed.searchParams.delete(key);
    return parsed.toString();
  }
  let handle = raw.replace(/^@/, "").toLowerCase(), linkedInType = "in";
  if (/^https?:\/\//i.test(raw)) {
    const parsed = url(raw);
    const providers: Record<string, readonly string[]> = { github: ["github.com"], x: ["x.com", "twitter.com"], linkedin: ["linkedin.com"], telegram: ["t.me", "telegram.me"] };
    if (parsed.port || !providers[kind]?.includes(parsed.hostname)) invalid("Unrecognized identity provider URL.");
    const path = parsed.pathname.replace(/\/$/, "");
    if (kind === "linkedin") {
      const match = /^\/(in|company)\/([A-Za-z0-9-]+)$/.exec(path);
      if (!match) invalid("Expected a LinkedIn profile or company URL.");
      linkedInType = match[1]; handle = match[2].toLowerCase();
    } else {
      const match = /^\/([A-Za-z0-9_-]+)$/.exec(path);
      if (!match) invalid("Expected a single provider username.");
      handle = match[1].toLowerCase();
    }
  }
  if (kind === "github") {
    if (!/^[a-z0-9](?:[a-z0-9-]{0,37}[a-z0-9])?$/.test(handle) || handle.includes("--") || ["login", "logout", "settings", "signup", "features", "explore", "marketplace", "organizations", "topics", "search", "about", "pricing"].includes(handle)) invalid("Invalid GitHub username.");
  } else if (kind === "x") {
    if (!/^[a-z0-9_]{1,15}$/.test(handle) || ["home", "search", "explore", "settings", "messages", "notifications", "intent", "share", "i"].includes(handle)) invalid("Invalid X username.");
  } else if (kind === "telegram") {
    if (!/^[a-z][a-z0-9_]{4,31}$/.test(handle) || ["share", "joinchat", "addstickers", "addemoji", "proxy", "socks", "login"].includes(handle)) invalid("Invalid Telegram username.");
  } else if (kind === "linkedin") {
    if (!/^[a-z0-9](?:[a-z0-9-]{0,98}[a-z0-9])?$/.test(handle)) invalid("Invalid LinkedIn identifier.");
    return `https://linkedin.com/${linkedInType}/${handle}`;
  }
  return handle;
}

/** Identity keys are append/delete only. Canonical retries return the original
 * row unchanged, so imported observations never replace user provenance. */
export async function crmIdentityRequest(db: D1Database, ownerId: string, operation: CrmIdentityOperation, input: unknown, createId: string): Promise<unknown> {
  text(ownerId, "authenticated owner", 512);
  const session = db.withSession("first-primary");
  try {
    switch (operation) {
      case "list": {
        const args = object(input, ["record_id", "limit", "cursor"]), recordId = id(args.record_id);
        const size = args.limit ?? 20;
        if (typeof size !== "number" || !Number.isInteger(size) || size < 1 || size > 100) invalid("limit must be an integer from 1 to 100.");
        const scope = createHash("sha256").update(JSON.stringify([ownerId, recordId])).digest("hex");
        let cursor: { at: number; id: string } | null = null;
        if (args.cursor !== undefined) {
          try {
            const encoded = text(args.cursor, "cursor", 4096);
            if (!/^[A-Za-z0-9_-]+$/.test(encoded)) invalid("Invalid cursor.");
            const decoded = object(JSON.parse(Buffer.from(encoded, "base64url").toString("utf8")), ["v", "scope", "at", "id"]);
            if (decoded.v !== 1 || decoded.scope !== scope || typeof decoded.at !== "number" || !Number.isSafeInteger(decoded.at) || decoded.at < 0) invalid("Invalid cursor.");
            cursor = { at: decoded.at, id: id(decoded.id) };
          } catch { invalid("Invalid cursor for this record."); }
        }
        if (!await session.prepare("SELECT id FROM crm_records WHERE owner_id=? AND id=?").bind(ownerId, recordId).first()) missing();
        if (cursor && !await session.prepare("SELECT id FROM crm_identities WHERE owner_id=? AND record_id=? AND created_at=? AND id=?").bind(ownerId, recordId, cursor.at, cursor.id).first()) invalid("Invalid cursor anchor.");
        const values: (string | number)[] = [ownerId, recordId];
        if (cursor) values.push(cursor.at, cursor.at, cursor.id);
        const rows = (await session.prepare(`SELECT ${columns} FROM crm_identities WHERE owner_id=? AND record_id=?
          ${cursor ? "AND (created_at>? OR (created_at=? AND id>?))" : ""} ORDER BY created_at,id LIMIT ?`).bind(...values, size + 1).all<Row>()).results;
        const page = rows.slice(0, size), last = page.at(-1);
        return { identities: page, next_cursor: rows.length > size && last ? Buffer.from(JSON.stringify({ v: 1, scope, at: last.created_at, id: last.id })).toString("base64url") : null };
      }
      case "save": {
        const args = object(input, ["record_id", "kind", "value", "origin", "source_ref"]), recordId = id(args.record_id), identityId = id(createId);
        if (typeof args.kind !== "string" || !kinds.includes(args.kind)) invalid("Invalid identity kind.");
        if (args.origin !== "user" && args.origin !== "source") invalid("origin must be user or source.");
        const normalized = text(canonical(args.kind, args.value), "normalized value");
        const sourceRef = args.source_ref === undefined ? null : source(args.source_ref);
        if (args.origin === "source" && sourceRef === null) invalid("Source identities require source_ref.");
        const now = Date.now();
        const result = await session.batch([
          session.prepare("SELECT id FROM crm_records WHERE owner_id=? AND id=?").bind(ownerId, recordId),
          session.prepare(`INSERT INTO crm_identities(owner_id,id,record_id,kind,value,normalized,origin,source_ref,created_at,updated_at)
            SELECT ?,?,?,?,?,?,?,?,?,? WHERE EXISTS (SELECT 1 FROM crm_records WHERE owner_id=? AND id=?)
            ON CONFLICT(owner_id,record_id,kind,normalized) DO NOTHING`).bind(ownerId, identityId, recordId, args.kind, normalized, normalized, args.origin, sourceRef, now, now, ownerId, recordId),
          session.prepare(`SELECT ${columns} FROM crm_identities WHERE owner_id=? AND record_id=? AND kind=? AND normalized=?`).bind(ownerId, recordId, args.kind, normalized),
        ]);
        if (!result[0].results.length) missing();
        const identity = result[2].results[0];
        if (!identity) missing();
        return { identity };
      }
      case "delete": {
        const args = object(input, ["id"]), identityId = id(args.id);
        const result = await session.prepare("DELETE FROM crm_identities WHERE owner_id=? AND id=? RETURNING id").bind(ownerId, identityId).first();
        if (!result) missing();
        return { deleted: true };
      }
      default: return invalid("Unknown identity operation.");
    }
  } catch (error) {
    if (error instanceof CrmError) throw error;
    throw new Error("CRM identity storage request failed.");
  }
}
