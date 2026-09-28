import { Buffer } from "node:buffer";
import { suggestTodoMailReply, type TodoMailSuggestionAI } from "./todo-mail-suggest";
import { browserEgressSubject } from "./browser-egress";
import { bindAgentCredential } from "./credentials";
import { connectorConnectionId, connectorStatuses, type ConnectorConnection } from "./connector-status";
import { handleManagedEgress } from "./managed-egress";

const json = (value: unknown, status = 200) => Response.json(value, { status, headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" } });
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const identifier = /^[A-Za-z0-9_-]{1,512}$/;
const gmail = "https://gmail.googleapis.com/gmail/v1/users/me/";
const calendar = "https://www.googleapis.com/calendar/v3/";
class Failure extends Error { constructor(readonly code: string, readonly status = 400) { super(code); } }
const fail = (code: string, status = 400): never => { throw new Failure(code, status); };
const record = (v: unknown): v is Record<string, any> => !!v && typeof v === "object" && !Array.isArray(v);
const str = (v: unknown, max = 4096): string => typeof v === "string" && v.length <= max && v.isWellFormed() ? v : fail("invalid_request");
const header = (v: unknown, max = 4096): string => { const s = str(v, max); return /[\r\n\u0000-\u001f\u007f]/.test(s) ? fail("invalid_header") : s; };
const id = (v: unknown): string => typeof v === "string" && identifier.test(v) ? v : fail("invalid_id");
const connection = (v: unknown): string => connectorConnectionId(v) ?? fail("invalid_connection_id");
const escape = (s: string) => s.replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;");

async function parallel<T, R>(values: T[], visit: (value: T) => Promise<R>, concurrency = 5): Promise<R[]> {
  const results: R[] = new Array(values.length); let cursor = 0;
  await Promise.all(Array.from({ length: Math.min(concurrency, values.length) }, async () => {
    for (;;) { const index = cursor++; if (index >= values.length) return; results[index] = await visit(values[index]!); }
  })); return results;
}

async function readJSON(source: Request | Response, limit: number): Promise<any> {
  const reader = source.body?.getReader(); if (!reader) return fail("invalid_json");
  const chunks: Uint8Array[] = []; let size = 0;
  try { for (;;) { const next = await reader.read(); if (next.done) break; size += next.value.length;
    if (size > limit) return fail("body_too_large", 413); chunks.push(next.value); }
    return JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(Buffer.concat(chunks)));
  } catch (e) { if (e instanceof Failure) throw e; return fail("invalid_json"); }
  finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
}

export function initializeTodoMail(storage: DurableObjectStorage): void {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS todo_mail_drafts (
    id TEXT PRIMARY KEY, version INTEGER NOT NULL, data TEXT NOT NULL, updated_at TEXT NOT NULL
  ); CREATE TABLE IF NOT EXISTS todo_mail_sends (
    operation_id TEXT PRIMARY KEY, draft_id TEXT NOT NULL UNIQUE, version INTEGER NOT NULL,
    status TEXT NOT NULL, message_id TEXT, thread_id TEXT, created_at TEXT NOT NULL
  );`);
}
type Draft = { id: string; version: number; connection_id: string; mode: string; to: string[]; cc: string[]; bcc: string[];
  subject: string; body_text: string; thread_id: string | null; reply_message_id: string | null; updated_at: string; status: string };
type Receipt = { operation_id: string; draft_id: string; version: number; status: string; message_id: string | null; thread_id: string | null; created_at: string };
function receipt(storage: DurableObjectStorage, draftID: string): Receipt | undefined {
  return storage.sql.exec<Receipt>("SELECT * FROM todo_mail_sends WHERE draft_id = ?", draftID).toArray()[0];
}
function draft(storage: DurableObjectStorage, draftID: string): Draft {
  const row = storage.sql.exec<{ data: string }>("SELECT data FROM todo_mail_drafts WHERE id = ?", draftID).toArray()[0];
  if (!row) return fail("not_found", 404);
  const result = JSON.parse(row.data) as Draft; result.status = receipt(storage, draftID)?.status ?? "draft"; return result;
}
function addresses(value: unknown): string[] {
  if (!Array.isArray(value) || value.length > 100) return fail("invalid_recipients");
  return value.map(v => { const address = header(v, 320).trim();
    const [local, domain, extra] = address.split("@");
    if (extra !== undefined || !local || local.length > 64 || !/^[A-Za-z0-9!#$%&'*+/=?^_`{|}~-]+(?:\.[A-Za-z0-9!#$%&'*+/=?^_`{|}~-]+)*$/.test(local)
      || !domain || domain.length > 253 || !domain.includes(".") || !domain.split(".").every(label => /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?$/.test(label))) return fail("invalid_recipients");
    return address;
  });
}
// Fold on complete UTF-8 code points; encoded words stay within RFC 2047's 75 characters.
function encodedSubject(value: string): string {
  const words: string[] = []; let chunk = "", bytes = 0;
  for (const char of value) {
    const size = Buffer.byteLength(char);
    if (bytes + size > 42) { words.push("=?UTF-8?B?" + Buffer.from(chunk).toString("base64") + "?="); chunk = ""; bytes = 0; }
    chunk += char; bytes += size;
  }
  if (chunk) words.push("=?UTF-8?B?" + Buffer.from(chunk).toString("base64") + "?=");
  return words.join("\r\n ");
}
function messageID(value: string): string {
  if (!/^<[^<>\s@]+@[^<>\s@]+>$/.test(header(value, 900)) || /[^\x21-\x7e]/.test(value)) fail("invalid_message_id", 409);
  return value;
}
function validatedDraft(input: Record<string, any>): Omit<Draft, "id" | "version" | "updated_at" | "status"> {
  if (Object.keys(input).some(k => !["id", "version", "connection_id", "mode", "to", "cc", "bcc", "subject", "body_text", "thread_id", "reply_message_id"].includes(k))) fail("invalid_request");
  if (!["compose", "reply", "reply_all", "forward"].includes(input.mode)) fail("invalid_mode");
  const result = { connection_id: connection(input.connection_id), mode: input.mode as string, to: addresses(input.to),
    cc: addresses(input.cc ?? []), bcc: addresses(input.bcc ?? []), subject: header(input.subject, 998), body_text: str(input.body_text, 200_000),
    thread_id: input.thread_id == null ? null : id(input.thread_id), reply_message_id: input.reply_message_id == null ? null : id(input.reply_message_id) };
  if (["reply", "reply_all"].includes(result.mode) && (!result.thread_id || !result.reply_message_id)) fail("missing_reply_context");
  if (result.mode === "compose" && (result.thread_id || result.reply_message_id)) fail("invalid_reply_context");
  return result;
}
function htmlText(html: string): string {
  // This is a text projection, never a sanitizer used to return original HTML.
  return html.replace(/<!--[^]*?(?:-->|$)/g, "").replace(/<(script|style|head)\b[^>]*>[^]*?(?:<\/\1\s*>|$)/gi, "")
    .replace(/<\/?(?:p|div|br|li|tr)\b[^>]*>/gi, "\n").replace(/<[^>]*>/g, "")
    .replace(/&(#x[\da-f]+|#\d+|amp|lt|gt|quot|apos|nbsp);/gi, (all, entity: string) => {
      const names: Record<string, string> = { amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " " };
      if (entity[0] !== "#") return names[entity.toLowerCase()] ?? all;
      const n = entity[1].toLowerCase() === "x" ? parseInt(entity.slice(2), 16) : Number(entity.slice(1));
      return n > 0 && n <= 0x10ffff && !(n >= 0xd800 && n <= 0xdfff) ? String.fromCodePoint(n) : "�";
    }).trim();
}
function headers(payload: any): Record<string, string> {
  const result: Record<string, string> = {};
  for (const h of Array.isArray(payload?.headers) ? payload.headers : []) {
    if (typeof h?.name === "string" && typeof h?.value === "string") {
      const name = h.name.toLowerCase(); if (!(name in result)) result[name] = h.value.slice(0, 8192);
    }
  } return result;
}
function threadSummary(thread: any, connectionID: string, fallback: any = {}) {
  const messages = thread.messages ?? [], last = messages.at(-1), h = headers(last?.payload);
  return { id: thread.id ?? fallback.id, connection_id: connectionID, subject: h.subject ?? "",
    snippet: thread.snippet ?? fallback.snippet ?? last?.snippet ?? "", from: h.from ?? "",
    date: Number(last?.internalDate) ? new Date(Number(last.internalDate)).toISOString() : h.date ?? "",
    unread: messages.some((m: any) => (m.labelIds ?? []).includes("UNREAD")), message_count: messages.length,
    in_inbox: messages.some((m: any) => (m.labelIds ?? []).includes("INBOX")) };
}
type Provider = (url: string, connectionID: string, body?: unknown) => Promise<any>;
async function message(raw: any, connectionID: string, provider: Provider) {
  const h = headers(raw.payload), attachments: { id: string; filename: string; mime_type: string; size: number }[] = [];
  let nodes = 0, external = 0, truncated = false;
  async function visit(part: any, depth = 0): Promise<string> {
    if (++nodes > 256 || depth > 20) { truncated = true; return ""; }
    if (!record(part)) return "";
    if (part.filename || /^attachment\b/i.test(headers(part)["content-disposition"] ?? "")) {
      if (typeof part.body?.attachmentId === "string") attachments.push({ id: part.body.attachmentId,
        filename: String(part.filename || "attachment"), mime_type: String(part.mimeType || "application/octet-stream"), size: Number(part.body?.size) || 0 });
      return "";
    }
    if (Array.isArray(part.parts)) {
      if (part.mimeType === "multipart/alternative") {
        const preferred = part.parts.find((p: any) => p.mimeType === "text/plain") ?? part.parts.find((p: any) => p.mimeType === "text/html") ?? part.parts[0];
        return preferred ? visit(preferred, depth + 1) : "";
      }
      const texts = []; for (const child of part.parts.slice(0, 256)) texts.push(await visit(child, depth + 1));
      if (part.parts.length > 256) truncated = true; return texts.filter(Boolean).join("\n\n");
    }
    if (!["text/plain", "text/html"].includes(part.mimeType)) return "";
    let data = part.body?.data;
    if (!data && part.body?.attachmentId) {
      if (++external > 8) { truncated = true; return ""; }
      data = (await provider(`${gmail}messages/${id(raw.id)}/attachments/${id(part.body.attachmentId)}`, connectionID)).data;
    }
    if (typeof data !== "string" || !/^[A-Za-z0-9_-]*={0,2}$/.test(data) || data.replace(/=+$/, "").length % 4 === 1) { truncated = true; return ""; }
    const charset = /charset\s*=\s*["']?([^\s;"']+)/i.exec(headers(part)["content-type"] ?? "")?.[1] ?? "utf-8";
    let text: string;
    try { text = new TextDecoder(charset, { fatal: true, ignoreBOM: false }).decode(Buffer.from(data, "base64url")); }
    catch { truncated = true; return ""; }
    return part.mimeType === "text/html" ? htmlText(text) : text;
  }
  const body = await visit(raw.payload);
  return { id: raw.id, thread_id: raw.threadId, from: h.from ?? "", to: h.to ?? "", cc: h.cc ?? "", bcc: h.bcc ?? "",
    reply_to: h["reply-to"] ?? "", subject: h.subject ?? "", date: Number(raw.internalDate) ? new Date(Number(raw.internalDate)).toISOString() : h.date ?? "",
    message_id: h["message-id"] ?? "", body_text: body, body_html: `<pre>${escape(body)}</pre>`, body_truncated: truncated,
    unread: (raw.labelIds ?? []).includes("UNREAD"), attachments };
}

/** Called only behind the account route's principal/origin gate in UserAccount. */
export async function handleTodoMail(request: Request, storage: DurableObjectStorage, binding: Fetcher | undefined, ownerID: string, ai?: TodoMailSuggestionAI): Promise<Response> {
  try {
    if (!binding) return fail("connector_unavailable", 503);
    const url = new URL(request.url), path = url.pathname, params = url.searchParams;
    if (!["GET", "POST"].includes(request.method)) return fail("method_not_allowed", 405);
    const statuses = async () => {
      const response = await binding.fetch(`https://broker.internal/users/${encodeURIComponent(ownerID)}/connectors`);
      if (!response.ok) return fail("connector_unavailable", 503);
      return connectorStatuses(await readJSON(response, 256_000));
    };
    let subject: string | undefined, preparation: Promise<void> | undefined;
    function prepare() {
      return preparation ??= (async () => { subject = await browserEgressSubject(ownerID, "todo-mail-v1"); await bindAgentCredential(binding!, subject, ownerID); })();
    }
    const provider: Provider = async (target, connectionID, body) => {
      await prepare();
      const response = await handleManagedEgress(new Request(target, { method: body === undefined ? "GET" : "POST", redirect: "manual",
        headers: { "x-nanocodex-connector-connection": connectionID, "content-type": "application/json" },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(30_000) }), binding!, subject,
      (capability, selected) => ["gmail", "gcalendar"].includes(capability) && selected === connectionID);
      if (!response.ok) { await response.body?.cancel(); return fail(response.status === 404 ? "not_found" : response.status === 401 || response.status === 403 ? "connector_permission_required" : "provider_unavailable", response.status === 404 ? 404 : 502); }
      return readJSON(response, 12 * 1024 * 1024);
    };
    async function selected(connectionID: string, capability: "gmail" | "gcalendar" = "gmail"): Promise<ConnectorConnection> {
      return (await statuses())[capability].connections?.find(c => c.id === connectionID) ?? fail("connection_not_found", 404);
    }
    if (path === "/todo/mail/accounts" && request.method === "GET") {
      const accounts = await parallel([...(await statuses()).gmail.connections ?? []], async c => {
        let email = ""; try { email = (await provider(`${gmail}profile`, c.id)).emailAddress ?? ""; } catch { /* Keep reconnectable accounts visible. */ }
        return { connection_id: c.id, label: c.label, email, capabilities: c.capabilities ?? ["gmail"], scopes: c.scopes ?? [] };
      });
      return json({ accounts });
    }
    if (path === "/todo/mail/threads" && request.method === "GET") {
      const connectionID = connection(params.get("connection_id")); await selected(connectionID);
      const target = new URL(`${gmail}threads`); target.searchParams.set("maxResults", "25"); target.searchParams.set("q", str(params.get("q") ?? "in:inbox", 2048));
      if (params.has("page_token")) target.searchParams.set("pageToken", str(params.get("page_token"), 8192));
      const list = await provider(target.href, connectionID);
      const threads = await parallel<any, unknown>((list.threads ?? []).slice(0, 25), async entry => {
        const thread = await provider(`${gmail}threads/${id(entry.id)}?format=metadata&metadataHeaders=From&metadataHeaders=Subject&metadataHeaders=Date`, connectionID);
        return threadSummary(thread, connectionID, entry);
      });
      return json({ threads, next_page_token: list.nextPageToken ?? null });
    }
    const threadPath = /^\/todo\/mail\/threads\/([A-Za-z0-9_-]+)$/.exec(path);
    if (threadPath && request.method === "GET") {
      const connectionID = connection(params.get("connection_id")); await selected(connectionID);
      const format = params.get("format") ?? "full";
      if (!["full", "metadata"].includes(format)) fail("invalid_format");
      if (format === "metadata") {
        const thread = await provider(gmail + "threads/" + id(threadPath[1]) + "?format=metadata&metadataHeaders=From&metadataHeaders=Subject&metadataHeaders=Date", connectionID);
        return json({ summary: threadSummary(thread, connectionID) });
      }
      const thread = await provider(`${gmail}threads/${id(threadPath[1])}?format=full`, connectionID), messages = [];
      for (const raw of thread.messages ?? []) messages.push(await message(raw, connectionID, provider));
      return json({ thread: { id: thread.id, connection_id: connectionID, subject: messages.at(-1)?.subject ?? "", messages } });
    }
    const attachment = /^\/todo\/mail\/messages\/([A-Za-z0-9_-]+)\/attachments\/([A-Za-z0-9_-]+)$/.exec(path);
    if (attachment && request.method === "GET") {
      const connectionID = connection(params.get("connection_id")); await selected(connectionID);
      const result = await provider(`${gmail}messages/${id(attachment[1])}/attachments/${id(attachment[2])}`, connectionID);
      return json({ data: result.data, size: result.size });
    }
    if (path === "/todo/mail/drafts" && request.method === "GET") {
      const connectionID = connection(params.get("connection_id"));
      const threadID = params.has("thread_id") ? id(params.get("thread_id")) : null;
      return json({ drafts: storage.sql.exec<{id: string}>("SELECT id FROM todo_mail_drafts WHERE json_extract(data, '$.connection_id') = ? AND (? IS NULL OR json_extract(data, '$.thread_id') = ?) ORDER BY updated_at DESC LIMIT 100", connectionID, threadID, threadID).toArray().map(row => draft(storage, row.id)) });
    }
    const draftPath = /^\/todo\/mail\/drafts\/([0-9a-f-]{36})$/i.exec(path);
    if (draftPath && request.method === "GET") return json({ draft: draft(storage, draftPath[1]!.toLowerCase()) });
    if (path === "/todo/schedule" && request.method === "GET") {
      const connections = (await statuses()).gcalendar.connections ?? [], events: any[] = [], errors: any[] = [];
      const from = params.get("from") ?? new Date().toISOString(), to = params.get("to") ?? new Date(Date.now() + 14 * 86400_000).toISOString();
      if (![from, to].every(value => /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/.test(value)) || !Number.isFinite(Date.parse(from)) || !Number.isFinite(Date.parse(to)) || Date.parse(to) <= Date.parse(from) || Date.parse(to) - Date.parse(from) > 93 * 86400_000) fail("invalid_interval");
      const requested = params.get("connection_id"); if (requested && !connections.some(c => c.id === requested)) fail("connection_not_found", 404);
      const tasks: { connection_id: string; calendar_id: string; calendar_name: string }[] = [];
      const selectedConnections = connections.filter(c => !requested || c.id === requested);
      for (const c of selectedConnections.slice(10)) errors.push({ connection_id: c.id, calendar_id: null, error: "connection_limit" });
      // Bound each interactive request; surface every coverage limit rather than silently omitting it.
      await parallel(selectedConnections.slice(0, 10), async c => {
        try {
          const calendars = await provider(`${calendar}users/me/calendarList?maxResults=100`, c.id);
          if (calendars.nextPageToken) errors.push({ connection_id: c.id, calendar_id: null, error: "calendar_limit" });
          for (const cal of (calendars.items ?? []).slice(0, 100)) {
            tasks.push({ connection_id: c.id, calendar_id: cal.id, calendar_name: cal.summary ?? cal.id });
          }
        } catch (e) { errors.push({ connection_id: c.id, calendar_id: null, error: e instanceof Failure ? e.code : "provider_unavailable" }); }
      });
      for (const task of tasks.slice(40)) errors.push({ ...task, error: "calendar_limit" });
      await parallel(tasks.slice(0, 40), async task => {
        try {
          const target = new URL(`${calendar}calendars/${encodeURIComponent(task.calendar_id)}/events`);
          Object.entries({ singleEvents: "true", orderBy: "startTime", timeMin: from, timeMax: to, maxResults: "100" }).forEach(([k, v]) => target.searchParams.set(k, v));
          const result = await provider(target.href, task.connection_id);
          if (result.nextPageToken || result.items?.length > 100) errors.push({ ...task, error: "event_limit" });
          for (const e of (result.items ?? []).slice(0, 100)) {
            const self = (e.attendees ?? []).find((a: any) => a.self);
            if (e.status === "cancelled" || self?.responseStatus === "declined") continue;
            events.push({ id: e.id, ...task, title: e.summary ?? "(Untitled event)",
              start: e.start?.dateTime ?? e.start?.date ?? "", end: e.end?.dateTime ?? e.end?.date ?? "", all_day: !!e.start?.date,
              description: htmlText(e.description ?? ""), response_status: self?.responseStatus ?? null,
              location: e.location ?? "", html_url: typeof e.htmlLink === "string" && e.htmlLink.startsWith("https://") ? e.htmlLink : "" });
          }
        } catch (e) { errors.push({ ...task, error: e instanceof Failure ? e.code : "provider_unavailable" }); }
      });
      events.sort((a, b) => a.start.localeCompare(b.start)); return json({ events, partial: errors.length > 0, errors, from, to });
    }
    if (request.method !== "POST") return fail("not_found", 404);
    if (request.headers.get("content-type")?.split(";", 1)[0]?.trim().toLowerCase() !== "application/json") return fail("invalid_content_type", 415);
    const input = await readJSON(request, 256_000); if (!record(input)) return fail("invalid_request");
    if (path === "/todo/mail/suggest") {
      if (Object.keys(input).some(k => !["connection_id", "thread_id", "reply_message_id", "instructions"].includes(k))) fail("invalid_request");
      const connectionID = connection(input.connection_id), threadID = id(input.thread_id), messageID = id(input.reply_message_id);
      const instructions = str(input.instructions ?? "", 2000);
      await selected(connectionID);
      if (!ai) fail("suggestion_unavailable", 503);
      const thread = await provider(gmail + "threads/" + threadID + "?format=full", connectionID);
      const sourceIndex = (thread.messages ?? []).findIndex((m: any) => m.id === messageID && m.threadId === threadID);
      if (thread.id !== threadID || sourceIndex < 0) fail("reply_thread_mismatch", 409);
      const messages = await parallel<any, Awaited<ReturnType<typeof message>>>((thread.messages ?? []).slice(Math.max(0, sourceIndex - 19), sourceIndex + 1), raw => message(raw, connectionID, provider));
      return json(await suggestTodoMailReply(ai!, messages, instructions));
    }
    if (path === "/todo/mail/drafts") {
      const data = validatedDraft(input); await selected(data.connection_id);
      if (typeof input.id !== "string" || !uuid.test(input.id)) fail("invalid_id");
      if (!Number.isSafeInteger(input.version) || input.version < 0) fail("invalid_version");
      const draftID = input.id.toLowerCase();
      const exists = storage.sql.exec<{id: string}>("SELECT id FROM todo_mail_drafts WHERE id = ?", draftID).toArray()[0];
      const previous = exists ? draft(storage, draftID) : undefined;
      if (previous && input.version === 0) {
        const { id: _id, version: _version, status: _status, updated_at: _at, ...previousData } = previous;
        if (previous.version === 1 && JSON.stringify(previousData) === JSON.stringify(data)) return json({ draft: previous });
        fail("draft_conflict", 409);
      }
      if (input.id && !previous && input.version !== 0) fail("not_found", 404);
      if (previous && (previous.version !== input.version || previous.connection_id !== data.connection_id)) fail("stale_draft", 409);
      if (receipt(storage, draftID)) fail("draft_locked", 409);
      if (!previous && input.version != null && input.version !== 0) fail("invalid_version");
      const saved: Draft = { ...data, id: draftID, version: (previous?.version ?? 0) + 1, status: "draft", updated_at: new Date().toISOString() };
      storage.sql.exec("INSERT INTO todo_mail_drafts(id, version, data, updated_at) VALUES (?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET version=excluded.version, data=excluded.data, updated_at=excluded.updated_at",
        saved.id, saved.version, JSON.stringify(saved), saved.updated_at);
      return json({ draft: saved }, previous ? 200 : 201);
    }
    if (path === "/todo/mail/send") {
      if (Object.keys(input).some(k => !["draft_id", "version", "operation_id"].includes(k))) fail("invalid_request");
      if (typeof input.draft_id !== "string" || !uuid.test(input.draft_id) || typeof input.operation_id !== "string" || !uuid.test(input.operation_id) || !Number.isSafeInteger(input.version)) fail("invalid_request");
      const draftID = input.draft_id.toLowerCase(), operationID = input.operation_id.toLowerCase();
      const priorOperation = storage.sql.exec<Receipt>("SELECT * FROM todo_mail_sends WHERE operation_id = ?", operationID).toArray()[0];
      if (priorOperation) return priorOperation.draft_id === draftID && priorOperation.version === input.version ? json({ receipt: priorOperation }) : fail("operation_conflict", 409);
      const saved = draft(storage, draftID); if (saved.version !== input.version) fail("stale_draft", 409);
      const prior = receipt(storage, draftID); if (prior) return json({ receipt: prior });
      if (!saved.to.length && !saved.cc.length && !saved.bcc.length) fail("missing_recipients");
      await selected(saved.connection_id);
      const profile = await provider(`${gmail}profile`, saved.connection_id);
      const from = addresses([profile.emailAddress])[0]!;
      const mimeHeaders = [`From: ${from}`, `To: ${saved.to.join(",\r\n ")}`, ...(saved.cc.length ? [`Cc: ${saved.cc.join(",\r\n ")}`] : []), ...(saved.bcc.length ? [`Bcc: ${saved.bcc.join(",\r\n ")}`] : []),
        `Subject: ${encodedSubject(saved.subject)}`, "MIME-Version: 1.0", 'Content-Type: text/plain; charset="UTF-8"', "Content-Transfer-Encoding: base64"];
      if (["reply", "reply_all"].includes(saved.mode)) {
        const original = await provider(`${gmail}messages/${id(saved.reply_message_id)}?format=metadata&metadataHeaders=Message-ID&metadataHeaders=References`, saved.connection_id);
        if (original.threadId !== saved.thread_id) fail("reply_thread_mismatch", 409);
        const originalHeaders = headers(original.payload), originalID = messageID(originalHeaders["message-id"] ?? "");
        const references = header(originalHeaders.references ?? "", 4096).trim().split(/ +/).filter(Boolean).map(messageID);
        mimeHeaders.push(`In-Reply-To: ${originalID}`, `References: ${[...references, originalID].join("\r\n ")}`);
      }
      const raw = Buffer.from(`${mimeHeaders.join("\r\n")}\r\n\r\n${Buffer.from(saved.body_text).toString("base64").match(/.{1,76}/g)?.join("\r\n") ?? ""}`).toString("base64url");
      // Recheck after provider reads: edits and sends may have interleaved while awaiting I/O.
      if (draft(storage, draftID).version !== saved.version) fail("stale_draft", 409);
      const concurrent = receipt(storage, draftID); if (concurrent) return json({ receipt: concurrent });
      const operationConflict = storage.sql.exec<Receipt>("SELECT * FROM todo_mail_sends WHERE operation_id = ?", operationID).toArray()[0];
      if (operationConflict) fail("operation_conflict", 409);
      storage.sql.exec("INSERT INTO todo_mail_sends(operation_id,draft_id,version,status,created_at) VALUES (?,?,?,'unknown',?)", operationID, draftID, saved.version, new Date().toISOString());
      // The durable output gate flushes this intent before the network send. Never retry this write,
      // even after a timeout, non-2xx, process restart, or a different operation ID for this draft.
      try {
        const sent = await provider(`${gmail}messages/send`, saved.connection_id, { raw, ...(["reply", "reply_all"].includes(saved.mode) ? { threadId: saved.thread_id } : {}) });
        if (typeof sent.id === "string" && identifier.test(sent.id)) storage.transactionSync(() => {
          storage.sql.exec("UPDATE todo_mail_sends SET status='sent',message_id=?,thread_id=? WHERE operation_id=?", sent.id, typeof sent.threadId === "string" ? sent.threadId : null, operationID);
          if (["reply", "reply_all"].includes(saved.mode)) {
            storage.sql.exec("UPDATE todo_decisions SET status='resolved',version=version+1 WHERE status='needs_you' AND source_connection_id=? AND source_message_id=?", saved.connection_id, saved.reply_message_id);
          }
        });
      } catch { /* The provider may have accepted the message; preserve the durable unknown receipt. */ }
      return json({ receipt: receipt(storage, draftID) });
    }
    const modify = /^\/todo\/mail\/threads\/([A-Za-z0-9_-]+)\/modify$/.exec(path);
    if (modify) {
      const connectionID = connection(input.connection_id); await selected(connectionID);
      if (Object.keys(input).some(k => !["connection_id", "archive", "unread"].includes(k)) || input.archive !== undefined && typeof input.archive !== "boolean" || input.unread !== undefined && typeof input.unread !== "boolean" || input.archive === undefined && input.unread === undefined) fail("invalid_modification");
      await provider(`${gmail}threads/${id(modify[1])}/modify`, connectionID, { addLabelIds: [...(input.archive === false ? ["INBOX"] : []), ...(input.unread === true ? ["UNREAD"] : [])], removeLabelIds: [...(input.archive ? ["INBOX"] : []), ...(input.unread === false ? ["UNREAD"] : [])] });
      return json({ status: "updated" });
    }
    return fail("not_found", 404);
  } catch (e) { return json({ error: e instanceof Failure ? e.code : "provider_unavailable" }, e instanceof Failure ? e.status : 502); }
}
