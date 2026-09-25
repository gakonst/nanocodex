import { createHash } from "node:crypto";

export type CrmEmailPushOptions = {
  db: D1Database;
  ownerId: string;
  /** Host-managed connector egress; caller must opt in via trusted configuration. */
  fetch(request: Request): Promise<Response>;
  /** Recheck authenticated ownership/epoch before reads and persistence. */
  authorize(): void;
};
type Event = { startHistoryId: string; historyId: string; connectionId: string; email: string; type: "gmail.history" | "gmail.resync"; messageIds: string[]; truncated: boolean };
const MAX_BYTES = 65_536;
function invalid(): never { throw new Error("invalid_crm_email_event"); }
function parse(input: string): Event {
  let value: any;
  try { value = JSON.parse(input); } catch { return invalid(); }
  if (!value || typeof value !== "object" || Array.isArray(value)
    || Object.keys(value).some(key => !["connectionId", "email", "type", "startHistoryId", "historyId", "messageIds", "truncated", "crm"].includes(key))
    || (value.crm !== undefined && typeof value.crm !== "boolean")
    || typeof value.connectionId !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(value.connectionId)
    || typeof value.email !== "string" || !address(value.email)
    || !["gmail.history", "gmail.resync"].includes(value.type)
    || typeof value.truncated !== "boolean"
    || typeof value.startHistoryId !== "string" || !/^\d{1,40}$/.test(value.startHistoryId)
    || typeof value.historyId !== "string" || !/^\d{1,40}$/.test(value.historyId)
    || !Array.isArray(value.messageIds) || value.messageIds.length > 100
    || value.messageIds.some((id: unknown) => typeof id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(id))) invalid();
  return value;
}
// Conservative single mailbox parsing: never infer identities from display names,
// plus stripping, Gmail dot folding, groups, or multiple sender addresses.
function address(value: string): string | null {
  if (value.length > 1024 || /[\r\n\u0000,;]/.test(value)) return null;
  const match = /^(?:[^<>]*<([^<>]+)>|([^<>]+))$/.exec(value.trim());
  const email = (match?.[1] ?? match?.[2] ?? "").trim().toLowerCase();
  if (email.length > 254 || !/^[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?\.[a-z]{2,63}$/.test(email)) return null;
  const local = email.split("@")[0];
  return local.length <= 64 && !local.startsWith(".") && !local.endsWith(".") && !local.includes("..") ? email : null;
}
async function metadata(options: CrmEmailPushOptions, event: Event, id: string): Promise<any | null> {
  const controller = new AbortController();
  let reject!: (error: Error) => void;
  const timeout = new Promise<never>((_, fail) => { reject = fail; });
  const timer = setTimeout(() => { controller.abort(); reject(new Error("crm_email_provider_timeout")); }, 2_000);
  try {
    return await Promise.race([timeout, (async () => {
      options.authorize();
      const url = new URL(`https://gmail.googleapis.com/gmail/v1/users/me/messages/${id}`);
      url.searchParams.set("format", "metadata");
      for (const header of ["From", "Subject"]) url.searchParams.append("metadataHeaders", header);
      url.searchParams.set("fields", "id,internalDate,labelIds,payload/headers");
      const response = await options.fetch(new Request(url, { method: "GET", redirect: "manual", signal: controller.signal,
        headers: { "x-nanocodex-connector-connection": event.connectionId, accept: "application/json" } }));
      if (response.status === 404 || response.status === 410) { await response.body?.cancel(); return null; }
      if (!response.ok) { await response.body?.cancel(); throw new Error("crm_email_provider_error"); }
      if (Number(response.headers.get("content-length")) > MAX_BYTES) { await response.body?.cancel(); throw new Error("crm_email_response_too_large"); }
      if (!response.body) throw new Error("crm_email_invalid_response");
      const reader = response.body.getReader();
      const stop = () => { void reader.cancel().catch(() => {}); };
      controller.signal.addEventListener("abort", stop, { once: true });
      let bytes = 0; const chunks: Uint8Array[] = [];
      try {
        while (true) {
          controller.signal.throwIfAborted();
          const chunk = await reader.read();
          if (chunk.done) break;
          bytes += chunk.value.byteLength;
          if (bytes > MAX_BYTES) { stop(); throw new Error("crm_email_response_too_large"); }
          chunks.push(chunk.value);
        }
      } finally { controller.signal.removeEventListener("abort", stop); reader.releaseLock(); }
      const body = new Uint8Array(bytes); let offset = 0;
      for (const chunk of chunks) { body.set(chunk, offset); offset += chunk.byteLength; }
      let data: any;
      try { data = JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(body)); } catch { throw new Error("crm_email_invalid_response"); }
      if (!data || data.id !== id || !Array.isArray(data.labelIds) || !Array.isArray(data.payload?.headers)
        || data.payload.headers.length > 100 || !/^\d{1,16}$/.test(data.internalDate ?? "")
        || !Number.isSafeInteger(Number(data.internalDate)) || Number(data.internalDate) > 8.64e15) throw new Error("crm_email_invalid_response");
      return data;
    })()]);
  } finally { clearTimeout(timer); }
}

/** Deterministic import of an authenticated, CRM-opted-in Gmail push envelope.
 * No inbox scan, body/attachment reads, people creation, or outbound mail.
 * Failed messages throw so durable push delivery can retry the same envelope.
 */
export async function importCrmEmailPush(options: CrmEmailPushOptions, input: string) {
  options.authorize();
  if (!options.ownerId || options.ownerId.length > 512 || typeof input !== "string" || input.length > 32768) invalid();
  const event = parse(input);
  const result = { imported: 0, skipped: 0, limited: event.truncated || event.type === "gmail.resync", complete: true };
  const session = options.db.withSession("first-primary");
  const eventKey = createHash("sha256").update(JSON.stringify([event.connectionId, event.type,
    event.startHistoryId,
    event.historyId, event.messageIds])).digest("hex");
  const receipts = (await session.prepare("SELECT message_id FROM crm_email_push_receipts WHERE owner_id=? AND event_key=?")
    .bind(options.ownerId, eventKey).all<{ message_id: string }>()).results;
  const completed = new Set(receipts.map(row => row.message_id));
  const markComplete = async (id: string) => {
    options.authorize();
    await session.prepare("INSERT INTO crm_email_push_receipts(owner_id,event_key,message_id,completed_at) VALUES(?,?,?,?) ON CONFLICT DO NOTHING")
      .bind(options.ownerId, eventKey, id, Date.now()).run();
  };
  let reads = 0;
  const started = Date.now();
  for (const id of new Set(event.messageIds)) {
    if (completed.has(id)) { result.skipped++; continue; }
    if (reads >= 5 || Date.now() - started >= 10_000) { result.complete = false; break; }
    options.authorize();
    if (await session.prepare("SELECT 1 FROM crm_email_imports WHERE owner_id=? AND connection_id=? AND message_id=?")
      .bind(options.ownerId, event.connectionId, id).first()) { await markComplete(id); result.skipped++; continue; }
    reads++;
    const data = await metadata(options, event, id);
    options.authorize();
    if (!data || !data.labelIds.includes("INBOX") || data.labelIds.includes("DRAFT")) { await markComplete(id); result.skipped++; continue; }
    const headers = data.payload.headers;
    const from = headers.filter((h: any) => typeof h?.name === "string" && h.name.toLowerCase() === "from");
    const sender = from.length === 1 && typeof from[0].value === "string" ? address(from[0].value) : null;
    if (!sender) { await markComplete(id); result.skipped++; continue; }
    const matches = (await session.prepare(`SELECT id FROM crm_records WHERE owner_id=? AND
      (lower(trim(email))=? OR id IN (SELECT record_id FROM crm_identities WHERE owner_id=? AND kind='email' AND normalized=?)) LIMIT 2`)
      .bind(options.ownerId, sender, options.ownerId, sender).all<{ id: string }>()).results;
    options.authorize();
    if (matches.length !== 1) { await markComplete(id); result.skipped++; continue; }
    const recordId = matches[0].id;
    const noteId = "gmail-" + createHash("sha256").update(JSON.stringify([options.ownerId, event.connectionId, id])).digest("hex");
    const subjectHeader = headers.find((h: any) => typeof h?.name === "string" && h.name.toLowerCase() === "subject");
    const subject = typeof subjectHeader?.value === "string" ? subjectHeader.value.replace(/[\u0000-\u001f\u007f]/g, " ").slice(0, 2000) : "(no subject)";
    const body = `Received email (untrusted source metadata)\nFrom: ${sender}\nDate: ${new Date(Number(data.internalDate)).toISOString()}\nSubject: ${subject}\nGmail connection: ${event.connectionId}\nGmail message: ${id}`;
    const source = `https://mail.google.com/mail/u/?authuser=${encodeURIComponent(event.email)}#all/${id}`;
    const now = Date.now();
    // D1 batches serialize both writes atomically. A repeat never edits a note,
    // including notes edited or deleted by a user after the original import.
    const writes = await session.batch([
      session.prepare(`INSERT INTO crm_notes(owner_id,id,record_id,body,source_url,created_at,updated_at)
        SELECT ?,?,?,?,?,?,? WHERE EXISTS (SELECT 1 FROM crm_records WHERE owner_id=? AND id=?) AND NOT EXISTS (SELECT 1 FROM crm_email_imports WHERE owner_id=? AND connection_id=? AND message_id=?)
        ON CONFLICT(owner_id,id) DO NOTHING`).bind(options.ownerId, noteId, recordId, body, source, now, now, options.ownerId, recordId, options.ownerId, event.connectionId, id),
      session.prepare(`INSERT INTO crm_email_imports(owner_id,connection_id,message_id,record_id,note_id,imported_at) SELECT ?,?,?,?,?,? WHERE EXISTS (SELECT 1 FROM crm_records WHERE owner_id=? AND id=?)
        ON CONFLICT(owner_id,connection_id,message_id) DO NOTHING`).bind(options.ownerId, event.connectionId, id, recordId, noteId, now, options.ownerId, recordId),
    ]);
    await markComplete(id);
    if (writes[1].meta.changes) result.imported++; else result.skipped++;
  }
  return result;
}
