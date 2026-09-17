import { DurableObject, WorkerEntrypoint } from "cloudflare:workers";
import PostalMime from "postal-mime";

export interface Env {
  MAILBOX: DurableObjectNamespace<Mailbox>;
  MAILBOX_ADDRESS?: string;
  MAILBOX_OWNER_ID?: string;
  MAILBOX_ADMIN_ID?: string;
  EMAIL_SEND_ENABLED?: string;
  EMAIL?: SendEmail;
}
const RAW_LIMIT = 5 * 1024 * 1024;
const TEXT_LIMIT = 128 * 1024;
const MESSAGE_LIMIT = 10000;
const STORAGE_LIMIT = 256 * 1024 * 1024;
const REJECTED_SEND_CODES = new Set([
  "E_VALIDATION_ERROR", "E_FIELD_MISSING", "E_TOO_MANY_RECIPIENTS", "E_TOO_MANY_ATTACHMENTS",
  "E_SENDER_NOT_VERIFIED", "E_RECIPIENT_NOT_ALLOWED", "E_RECIPIENT_SUPPRESSED",
  "E_SENDER_DOMAIN_NOT_AVAILABLE", "E_CONTENT_TOO_LARGE", "E_RATE_LIMIT_EXCEEDED",
  "E_DAILY_LIMIT_EXCEEDED", "E_HEADER_NOT_ALLOWED", "E_HEADER_USE_API_FIELD",
  "E_HEADER_VALUE_INVALID", "E_HEADER_VALUE_TOO_LONG", "E_HEADER_NAME_INVALID",
  "E_HEADERS_TOO_LARGE", "E_HEADERS_TOO_MANY",
]);
const encoder = new TextEncoder();
const decoder = new TextDecoder();
class MailboxError extends Error {}
function fail(code: string): never { throw new MailboxError(code); }
function safeError(error: unknown) {
  if (error instanceof MailboxError) return {status: "error", error: {code: error.message}};
  // Unexpected storage/RPC failures can follow a send. Let the caller treat them as ambiguous.
  throw new Error("mailbox_service_unavailable");
}
function bounded(value: unknown, max: number): string {
  if (typeof value !== "string" || !value.length || encoder.encode(value).length > max) fail("invalid_input");
  return value;
}
export function address(value: unknown): string {
  const s = bounded(value, 254);
  if (!/^[A-Za-z0-9!#$%&'*+/=?^_`{|}~-]+(?:\.[A-Za-z0-9!#$%&'*+/=?^_`{|}~-]+)*@[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)+$/.test(s)) fail("invalid_address");
  return s.toLowerCase();
}
function config(env: Env) {
  const owner = bounded(env.MAILBOX_OWNER_ID, 256);
  if (!env.MAILBOX_ADMIN_ID || owner !== env.MAILBOX_ADMIN_ID) fail("mailbox_not_configured");
  return { owner, address: address(env.MAILBOX_ADDRESS) };
}
function identity(env: Env, input: Record<string, unknown>) {
  const c = config(env);
  if (input.owner_id !== c.owner) fail("owner_mismatch");
  bounded(input.agent_id, 256);
  return c;
}
function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) fail("invalid_input");
  return value as Record<string, unknown>;
}
function messageId(value: unknown): string | undefined {
  return typeof value === "string" && /^<[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]{1,200}@[A-Za-z0-9.-]{1,253}>$/.test(value) ? value : undefined;
}
function providerMessageId(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  // Only add delimiters to an already complete addr-spec; never invent a domain.
  return messageId(value) ?? messageId(`<${value}>`);
}
function boundedReferences(values: string[]): string[] {
  const refs = values.filter(value => messageId(value)).slice(-20);
  while (encoder.encode(refs.join(" ")).length > 2048) refs.shift();
  return refs;
}
function ids(value: string | undefined): string[] {
  return (value?.match(/<[^<>]*>/g) ?? []).slice(-20).filter((x) => messageId(x));
}
function clipped(value: string, max: number): string { return decoder.decode(encoder.encode(value).slice(0, max)); }
async function digest(value: Uint8Array): Promise<string> {
  return Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", value))).map(x => x.toString(16).padStart(2,"0")).join("");
}
interface StoredMessage {
  id: string; direction: "incoming" | "outgoing"; from: string; to: string[];
  subject: string; text: string; created_at: string; message_id?: string;
  references: string[]; agent_id?: string; related_agent_id?: string;
  auto_submitted?: boolean; attachments: { filename: string; type: string; size: number }[];
}
interface Operation { fingerprint: string; outcome: { status: "unknown" | "accepted" | "rejected"; operation_id: string; message_id: string; error?: string }; }
export class Mailbox extends DurableObject<Env> {
  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS messages (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL, wire_id TEXT, data TEXT NOT NULL)");
    ctx.storage.sql.exec("CREATE INDEX IF NOT EXISTS messages_wire ON messages(wire_id)");
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS operations (id TEXT PRIMARY KEY, data TEXT NOT NULL)");
  }
  private count(): number { return this.ctx.storage.sql.exec<{n:number}>("SELECT COUNT(*) AS n FROM messages").one().n; }
  private capacity(bytes: number) {
    const used = this.ctx.storage.sql.exec<{n:number}>("SELECT COALESCE(SUM(length(CAST(data AS BLOB))),0) AS n FROM messages").one().n
      + this.ctx.storage.sql.exec<{n:number}>("SELECT COALESCE(SUM(length(CAST(data AS BLOB))),0) AS n FROM operations").one().n;
    if (this.count() >= MESSAGE_LIMIT || used + bytes > STORAGE_LIMIT) fail("mailbox_full");
  }
  private insert(m: StoredMessage) { this.ctx.storage.sql.exec("INSERT INTO messages (id,wire_id,data) VALUES (?,?,?)",m.id,m.message_id ?? null,JSON.stringify(m)); }
  private read(id: string): StoredMessage | undefined {
    const rows = this.ctx.storage.sql.exec<{data:string}>("SELECT data FROM messages WHERE id=?", id).toArray();
    return rows[0] ? JSON.parse(rows[0].data) : undefined;
  }
  async ingest(owner: string, m: StoredMessage) {
    if (owner !== config(this.env).owner || m.to.length !== 1 || m.to[0] !== config(this.env).address) fail("owner_mismatch");
    if (this.read(m.id)) return { status: "duplicate" };
    for (const ref of [...m.references].reverse()) {
      const row = this.ctx.storage.sql.exec<{data:string}>("SELECT data FROM messages WHERE wire_id=? ORDER BY seq DESC LIMIT 1",ref).toArray()[0];
      if (row) { const prior: StoredMessage = JSON.parse(row.data); m.related_agent_id = prior.agent_id ?? prior.related_agent_id; break; }
    }
    this.capacity(encoder.encode(JSON.stringify(m)).length);
    this.insert(m);
    return { status: "stored" };
  }
  async execute(value: unknown): Promise<unknown> {
    try { return await this.executeOperation(value); }
    catch (error) { return safeError(error); }
  }
  private async executeOperation(value: unknown): Promise<unknown> {
    const input = object(value); const c = identity(this.env,input);
    const fields: Record<string, string[]> = {
      status: [], list: ["cursor", "limit"], read: ["message_id"],
      send: ["operation_id", "to", "subject", "text", "reply_to_message_id"],
    };
    const allowed = typeof input.operation === "string" && Object.hasOwn(fields,input.operation) ? fields[input.operation] : undefined;
    if (!allowed || Object.keys(input).some(k => !["owner_id","agent_id","operation",...allowed].includes(k))) fail("invalid_input");
    switch (input.operation) {
      case "status": return { configured: true, address: c.address, send_enabled: this.env.EMAIL_SEND_ENABLED === "true" && !!this.env.EMAIL, message_count: this.count() };
      case "read": return { message: this.read(bounded(input.message_id,256)) ?? null, untrusted_content: true };
      case "list": {
        const limit = input.limit ?? 25;
        if (!Number.isInteger(limit) || Number(limit) < 1 || Number(limit) > 50) fail("invalid_limit");
        const cursor = input.cursor ?? "0";
        if (typeof cursor !== "string" || !/^(0|[1-9][0-9]{0,14})$/.test(cursor)) fail("invalid_cursor");
        const rows = this.ctx.storage.sql.exec<{seq:number;data:string}>("SELECT seq,data FROM messages WHERE seq>? ORDER BY seq LIMIT ?",Number(cursor),Number(limit)+1).toArray();
        const page = rows.slice(0,Number(limit));
        return { messages: page.map(r => { const {text, ...m}: StoredMessage = JSON.parse(r.data); return m; }), next_cursor: rows.length > Number(limit) ? String(page.at(-1)!.seq) : null, untrusted_content: true };
      }
      case "send": {
        const operationId = bounded(input.operation_id,36).toLowerCase();
        if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(operationId)) fail("invalid_operation_id");
        if (!Array.isArray(input.to) || input.to.length < 1 || input.to.length > 10) fail("invalid_recipients");
        const to = input.to.map(address);
        const subject = bounded(input.subject,998);
        if (/[\r\n\x00-\x1f\x7f]/.test(subject)) fail("invalid_subject");
        const text = bounded(input.text,TEXT_LIMIT);
        const reply = input.reply_to_message_id === undefined ? undefined : bounded(input.reply_to_message_id,256);
        const fingerprint = JSON.stringify({agent_id:input.agent_id,to,subject,text,reply});
        const old = this.ctx.storage.sql.exec<{data:string}>("SELECT data FROM operations WHERE id=?",operationId).toArray()[0];
        if (old) { const op:Operation = JSON.parse(old.data); if (op.fingerprint !== fingerprint) fail("operation_conflict"); return op.outcome; }
        if (this.env.EMAIL_SEND_ENABLED !== "true" || !this.env.EMAIL) fail("send_disabled");
        const prior = reply ? this.read(reply) : undefined;
        if (reply && !prior) fail("reply_not_found");
        if (prior?.auto_submitted) fail("auto_submitted_reply_blocked");
        const refs = prior ? boundedReferences([...prior.references,...(prior.message_id ? [prior.message_id] : [])]) : [];
        const m:StoredMessage = {id:operationId,direction:"outgoing",from:c.address,to,subject,text,created_at:new Date().toISOString(),references:refs,agent_id:String(input.agent_id),attachments:[]};
        const op:Operation = {fingerprint,outcome:{status:"unknown",operation_id:operationId,message_id:m.id}};
        this.capacity(encoder.encode(JSON.stringify(m)).length + encoder.encode(JSON.stringify(op)).length);
        this.ctx.storage.transactionSync(() => { this.ctx.storage.sql.exec("INSERT INTO operations(id,data) VALUES (?,?)",operationId,JSON.stringify(op)); this.insert(m); });
        // Commit the ambiguity marker before any external side effect. Concurrent/restarted calls never resend.
        await this.ctx.storage.sync();
        const headers:Record<string,string> = {"Auto-Submitted":"auto-generated"};
        if (prior?.message_id) headers["In-Reply-To"] = prior.message_id;
        if (refs.length) headers.References = refs.join(" ");
        let result: EmailSendResult;
        try { result = await this.env.EMAIL.send({from:c.address,to,subject,text,headers}); }
        catch (error) {
          const code = error && typeof error === "object" && "code" in error ? error.code : undefined;
          if (typeof code === "string" && REJECTED_SEND_CODES.has(code)) {
            op.outcome.status = "rejected";
            op.outcome.error = code;
            this.ctx.storage.sql.exec("UPDATE operations SET data=? WHERE id=?",JSON.stringify(op),operationId);
          }
          return op.outcome;
        }
        op.outcome.status = "accepted";
        m.message_id = providerMessageId(result?.messageId);
        this.ctx.storage.transactionSync(() => {
          this.ctx.storage.sql.exec("UPDATE messages SET wire_id=?,data=? WHERE id=?",m.message_id ?? null,JSON.stringify(m),m.id);
          this.ctx.storage.sql.exec("UPDATE operations SET data=? WHERE id=?",JSON.stringify(op),operationId);
        });
        return op.outcome;
      }
      default: return fail("invalid_operation");
    }
  }
}
export class EmailService extends WorkerEntrypoint<Env> {
  async execute(value: unknown): Promise<unknown> {
    try {
      const input = object(value); const c = identity(this.env,input);
      return await this.env.MAILBOX.get(this.env.MAILBOX.idFromName(c.owner)).execute(input);
    } catch (error) { return safeError(error); }
  }
}
export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    if (request.method !== "GET" || new URL(request.url).pathname !== "/health") return new Response("Not found",{status:404});
    let ready = false; try { config(env); ready = true; } catch {}
    return Response.json({ready,send_enabled:ready && env.EMAIL_SEND_ENABLED === "true" && !!env.EMAIL});
  },
  async email(message: ForwardableEmailMessage, env: Env): Promise<void> {
    let c; try { c = config(env); if (address(message.to) !== c.address) fail("wrong_destination"); } catch { message.setReject("Mailbox unavailable"); return; }
    if (message.rawSize > RAW_LIMIT) { message.setReject("Message too large"); return; }
    const chunks:Uint8Array[] = []; let size = 0; const reader = message.raw.getReader();
    while (true) { const part = await reader.read(); if (part.done) break; size += part.value.byteLength; if (size > RAW_LIMIT) { await reader.cancel(); message.setReject("Message too large"); return; } chunks.push(part.value); }
    const raw = new Uint8Array(size); let offset=0; for (const chunk of chunks) {raw.set(chunk,offset);offset+=chunk.length;}
    let parsed; try { parsed = await PostalMime.parse(raw, { maxNestingDepth: 20, maxHeadersSize: 64 * 1024, maxRfc822NestingDepth: 3, forceRfc822Attachments: true }); } catch { message.setReject("Invalid message"); return; }
    const envelope = encoder.encode(JSON.stringify([message.from,message.to]));
    const hashBytes = new Uint8Array(envelope.length+1+raw.length); hashBytes.set(envelope);hashBytes.set(raw,envelope.length+1);
    const m:StoredMessage = {id:await digest(hashBytes),direction:"incoming",from:clipped(message.from,254),to:[c.address],subject:clipped(parsed.subject ?? "",998),text:clipped(parsed.text ?? "",TEXT_LIMIT),created_at:new Date().toISOString(),message_id:messageId(parsed.messageId),references:[...ids(parsed.references),...ids(parsed.inReplyTo)].slice(-20),auto_submitted:parsed.headers.some(h => h.key.toLowerCase() === "auto-submitted" && h.value.trim().toLowerCase() !== "no"),attachments:parsed.attachments.slice(0,100).map(a => ({filename:clipped((a.filename ?? "").replace(/[\x00-\x1f\x7f]/g,""),256),type:clipped(a.mimeType,128),size: typeof a.content === "string" ? encoder.encode(a.content).length : a.content.byteLength}))};
    await env.MAILBOX.get(env.MAILBOX.idFromName(c.owner)).ingest(c.owner,m);
  },
} satisfies ExportedHandler<Env>;
