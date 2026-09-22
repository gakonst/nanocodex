import { DurableObject, WorkerEntrypoint } from "cloudflare:workers";
import PostalMime from "postal-mime";

export interface Env {
  MAILBOX: DurableObjectNamespace<Mailbox>;
  MAILBOX_ADDRESS?: string;
  MAILBOX_OWNER_ID?: string;
  MAILBOX_ADMIN_ID?: string;
  EMAIL_SEND_ENABLED?: string;
  EMAIL?: SendEmail;
  NANOCODEX_EMAIL_AGENT?: { resumeEmail(input: {owner_id:string;agent_id:string;workflow_id:string;message_id:string;goal:string;message:{from:string;subject:string;text:string};expires_at:number}): Promise<{state:"accepted"|"completed"|"failed";turn_id:string;agent_id?:string;reply_text?:string}> };

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
function clipped(value: string, max: number): string { let bytes = encoder.encode(value).slice(0,max); let text = decoder.decode(bytes); while (encoder.encode(text).length > max) { bytes = bytes.slice(0,-1); text = decoder.decode(bytes); } return text; }
async function digest(value: Uint8Array): Promise<string> {
  return Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", value))).map(x => x.toString(16).padStart(2,"0")).join("");
}
interface StoredMessage {
  id: string; direction: "incoming" | "outgoing"; from: string; to: string[];
  subject: string; text: string; created_at: string; message_id?: string;
  references: string[]; agent_id?: string; related_agent_id?: string;
  auto_submitted?: boolean; attachments: { filename: string; type: string; size: number }[];
}
interface Watch { id:string; agent_id:string; message_id:string; wire_id:string; recipient:string; goal:string; expires_at:number; max_replies:number; replies:number; revoked:boolean; }
type ResumePayload = Parameters<NonNullable<Env["NANOCODEX_EMAIL_AGENT"]>["resumeEmail"]>[0];
interface Job { payload:ResumePayload; id:string; watch_id:string; message_id:string; operation_id:string; done:boolean; state?:"queued"|"held"|"failed"|"cancelled"|"dispatch_unknown"; }
interface Operation { fingerprint: string; outcome: { status: "unknown" | "accepted" | "rejected"; operation_id: string; message_id: string; error?: string }; }
export class Mailbox extends DurableObject<Env> {
  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS messages (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL, wire_id TEXT, data TEXT NOT NULL)");
    ctx.storage.sql.exec("CREATE INDEX IF NOT EXISTS messages_wire ON messages(wire_id)");
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS operations (id TEXT PRIMARY KEY, data TEXT NOT NULL)");
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS email_watches (id TEXT PRIMARY KEY, data TEXT NOT NULL)");
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS email_jobs (id TEXT PRIMARY KEY, data TEXT NOT NULL)");
  }
  private watches(): Watch[] { return this.ctx.storage.sql.exec<{data:string}>("SELECT data FROM email_watches").toArray().map(r => JSON.parse(r.data)); }
  private saveWatch(w:Watch) { this.ctx.storage.sql.exec("INSERT OR REPLACE INTO email_watches(id,data) VALUES (?,?)",w.id,JSON.stringify(w)); }
  private saveJob(j:Job) { this.ctx.storage.sql.exec("INSERT OR REPLACE INTO email_jobs(id,data) VALUES (?,?)",j.id,JSON.stringify(j)); }
  private live(w:Watch) { return !w.revoked && w.expires_at > Date.now() && w.replies < w.max_replies; }
  private async queue(m:StoredMessage) {
    if (m.direction !== "incoming" || m.auto_submitted || !m.message_id) return;
    for (const w of this.watches()) {
      if (!this.live(w) || m.from.toLowerCase() !== w.recipient || !m.references.includes(w.wire_id)) continue;
      const pending = this.ctx.storage.sql.exec<{n:number}>("SELECT COUNT(*) AS n FROM email_jobs WHERE json_extract(data,'$.watch_id')=? AND json_extract(data,'$.done')=0",w.id).one().n;
      if (pending + w.replies >= w.max_replies) continue;
      // Wire ID dedup catches retransmissions whose MIME bytes differ.
      const id = `${w.id}:${m.message_id}`;
      if (this.ctx.storage.sql.exec("SELECT id FROM email_jobs WHERE id=?",id).toArray().length) continue;
      const job:Job = {id,watch_id:w.id,message_id:m.id,operation_id:crypto.randomUUID(),done:false,payload:{owner_id:config(this.env).owner,agent_id:w.agent_id,workflow_id:w.id,message_id:m.id,goal:w.goal,message:{from:m.from,subject:m.subject,text:m.text},expires_at:w.expires_at}};
      this.capacity(encoder.encode(JSON.stringify(job)).length);
      await this.ctx.storage.setAlarm(Date.now()+1000);
      const current = this.watches().find(item => item.id === w.id);
      const queued = this.ctx.storage.sql.exec<{n:number}>("SELECT COUNT(*) AS n FROM email_jobs WHERE json_extract(data,'$.watch_id')=? AND json_extract(data,'$.done')=0",w.id).one().n;
      if (!current || !this.live(current) || queued + current.replies >= current.max_replies) continue;
      this.capacity(encoder.encode(JSON.stringify(job)).length);
      if (!this.ctx.storage.sql.exec("SELECT id FROM email_jobs WHERE id=?",id).toArray().length) this.saveJob(job);
    }
  }
  async alarm() {
    const jobs = this.ctx.storage.sql.exec<{data:string}>("SELECT data FROM email_jobs").toArray().map(r => JSON.parse(r.data) as Job);
    for (const j of jobs) {
      if (j.done) continue;
      let w = this.watches().find(w => w.id === j.watch_id);
      const m = this.read(j.message_id);
      if (!w || !this.live(w) || !m || m.auto_submitted || m.from.toLowerCase() !== w.recipient) { j.done=true; j.state="cancelled"; this.saveJob(j); continue; }
      if (!this.env.NANOCODEX_EMAIL_AGENT) continue;
      if (config(this.env).owner !== j.payload.owner_id) { j.done=true; j.state="cancelled"; this.saveJob(j); continue; }
      try {
        const result = await this.env.NANOCODEX_EMAIL_AGENT.resumeEmail(j.payload);
        if (result.state === "accepted") continue;
        // Another alarm may have finished this job while the RPC was pending.
        const current = this.ctx.storage.sql.exec<{data:string}>("SELECT data FROM email_jobs WHERE id=?",j.id).toArray()[0];
        if (!current || JSON.parse(current.data).done) continue;
        // Read authorization again after the asynchronous model turn.
        w = this.watches().find(w => w.id === j.watch_id);
        if (result.state === "completed" && result.reply_text && w && this.live(w)) {
          const text = bounded(result.reply_text,TEXT_LIMIT);
          const subject = clipped(/^re:/i.test(m.subject) ? m.subject : `Re: ${m.subject}`,998).replace(/[\r\n\x00-\x1f\x7f]/g," ") || "Re: reply";
          // Reserve budget and finish the job before yielding to the send path.
          // The send path journals ambiguity before the provider call; no fresh retry is possible.
          w.replies++; this.saveWatch(w); j.done=true; j.state="dispatch_unknown"; this.saveJob(j);
          await this.executeOperation({owner_id:config(this.env).owner,agent_id:w.agent_id,operation:"send",operation_id:j.operation_id,to:[w.recipient],subject,text,reply_to_message_id:m.id},w.id);
        } else { j.done=true; j.state=result.state === "failed" ? "failed" : !w || !this.live(w) ? "cancelled" : "held"; this.saveJob(j); }
      } catch (error) {
        if (error instanceof MailboxError) { j.done=true; j.state="failed"; this.saveJob(j); }
        // Transport failures retry only unfinished backend work with its original payload.
        // A reserved dispatch remains visibly unknown and never sends again.
      }
    }
    if (this.ctx.storage.sql.exec<{data:string}>("SELECT data FROM email_jobs").toArray().some(r => !JSON.parse(r.data).done)) await this.ctx.storage.setAlarm(Date.now()+15_000);
  }
  private count(): number { return this.ctx.storage.sql.exec<{n:number}>("SELECT COUNT(*) AS n FROM messages").one().n; }
  private capacity(bytes: number) {
    const used = this.ctx.storage.sql.exec<{n:number}>("SELECT COALESCE(SUM(length(CAST(data AS BLOB))),0) AS n FROM messages").one().n
      + this.ctx.storage.sql.exec<{n:number}>("SELECT COALESCE(SUM(length(CAST(data AS BLOB))),0) AS n FROM operations").one().n
      + this.ctx.storage.sql.exec<{n:number}>("SELECT COALESCE(SUM(length(CAST(data AS BLOB))),0) AS n FROM email_watches").one().n
      + this.ctx.storage.sql.exec<{n:number}>("SELECT COALESCE(SUM(length(CAST(data AS BLOB))),0) AS n FROM email_jobs").one().n;
    if (this.count() >= MESSAGE_LIMIT || used + bytes > STORAGE_LIMIT) fail("mailbox_full");
  }
  private insert(m: StoredMessage) { this.ctx.storage.sql.exec("INSERT INTO messages (id,wire_id,data) VALUES (?,?,?)",m.id,m.message_id ?? null,JSON.stringify(m)); }
  private read(id: string): StoredMessage | undefined {
    const rows = this.ctx.storage.sql.exec<{data:string}>("SELECT data FROM messages WHERE id=?", id).toArray();
    return rows[0] ? JSON.parse(rows[0].data) : undefined;
  }
  async ingest(owner: string, m: StoredMessage) {
    if (owner !== config(this.env).owner || m.to.length !== 1 || m.to[0] !== config(this.env).address) fail("owner_mismatch");
    const existing = this.read(m.id);
    if (existing) { await this.queue(existing); return { status: "duplicate" }; }
    for (const ref of [...m.references].reverse()) {
      const row = this.ctx.storage.sql.exec<{data:string}>("SELECT data FROM messages WHERE wire_id=? ORDER BY seq DESC LIMIT 1",ref).toArray()[0];
      if (row) { const prior: StoredMessage = JSON.parse(row.data); m.related_agent_id = prior.agent_id ?? prior.related_agent_id; break; }
    }
    this.capacity(encoder.encode(JSON.stringify(m)).length);
    this.insert(m);
    await this.queue(m);
    return { status: "stored" };
  }
  async execute(value: unknown): Promise<unknown> {
    try { return await this.executeOperation(value); }
    catch (error) { return safeError(error); }
  }
  private async executeOperation(value: unknown, watchId?:string): Promise<unknown> {
    const input = object(value); const c = identity(this.env,input);
    const fields: Record<string, string[]> = {
      watch: ["watch_id","message_id","expected_recipient","goal","expires_at","max_replies"], unwatch: ["watch_id"], listwatches: [],
      status: [], list: ["cursor", "limit"], read: ["message_id"],
      send: ["operation_id", "to", "subject", "text", "reply_to_message_id"],
    };
    const allowed = typeof input.operation === "string" && Object.hasOwn(fields,input.operation) ? fields[input.operation] : undefined;
    if (!allowed || Object.keys(input).some(k => !["owner_id","agent_id","operation",...allowed].includes(k))) fail("invalid_input");
    switch (input.operation) {
      case "listwatches": return {watches:this.watches().filter(w => w.agent_id === input.agent_id).map(w => ({...w,jobs:this.ctx.storage.sql.exec<{data:string}>("SELECT data FROM email_jobs WHERE json_extract(data,'$.watch_id')=?",w.id).toArray().map(row => {
        const job:Job=JSON.parse(row.data);
        const operation=this.ctx.storage.sql.exec<{data:string}>("SELECT data FROM operations WHERE id=?",job.operation_id).toArray()[0];
        return {message_id:job.message_id,operation_id:job.operation_id,state:operation ? JSON.parse(operation.data).outcome.status : job.state ?? (job.done ? "held" : "queued")};
      })}))};
      case "unwatch": {
        const w = this.watches().find(w => w.id === bounded(input.watch_id,36) && w.agent_id === input.agent_id);
        if (!w) fail("watch_not_found"); w.revoked=true; this.saveWatch(w); return {watch:w};
      }
      case "watch": {
        const id = bounded(input.watch_id,36).toLowerCase();
        if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(id)) fail("invalid_watch_id");
        const prior = this.read(bounded(input.message_id,256));
        const recipient = address(input.expected_recipient);
        if (!prior || prior.direction !== "outgoing" || prior.agent_id !== input.agent_id || !prior.message_id || !prior.to.includes(recipient)) fail("invalid_watch_target");
        const op = this.ctx.storage.sql.exec<{data:string}>("SELECT data FROM operations WHERE id=?",prior.id).toArray()[0];
        if (!op || JSON.parse(op.data).outcome.status !== "accepted") fail("invalid_watch_target");
        const goal = bounded(input.goal,16*1024);
        if (!goal.trim()) fail("invalid_goal");
        const expires = input.expires_at; const max = input.max_replies;
        if (typeof expires !== "number" || !Number.isSafeInteger(expires) || expires <= Date.now() || expires > Date.now()+7*86400000) fail("invalid_expiry");
        if (typeof max !== "number" || !Number.isInteger(max) || max < 1 || max > 10) fail("invalid_reply_budget");
        const existing = this.watches().find(w => w.id === id);
        if (existing) {
          if (existing.agent_id !== input.agent_id || existing.message_id !== prior.id || existing.recipient !== recipient || existing.goal !== goal || existing.expires_at !== expires || existing.max_replies !== max) fail("watch_conflict");
          return {watch:existing};
        }
        if (this.watches().some(w => this.live(w) && w.wire_id === prior.message_id && w.recipient === recipient)) fail("watch_overlap");
        if (this.watches().length >= 1000) fail("watch_capacity");
        const w:Watch = {id,agent_id:String(input.agent_id),message_id:prior.id,wire_id:prior.message_id,recipient,goal,expires_at:expires,max_replies:max,replies:0,revoked:false};
        this.capacity(encoder.encode(JSON.stringify(w)).length);
        this.saveWatch(w);
        // Catch replies stored between the authorized send and watch registration.
        for (const row of this.ctx.storage.sql.exec<{data:string}>("SELECT data FROM messages WHERE seq>(SELECT seq FROM messages WHERE id=?) ORDER BY seq",prior.id).toArray()) await this.queue(JSON.parse(row.data));
        return {watch:w};
      }
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
        if (watchId) {
          const watch = this.watches().find(w => w.id === watchId);
          if (!watch || watch.revoked || watch.expires_at <= Date.now() || watch.replies > watch.max_replies) {
            op.outcome.status="rejected"; op.outcome.error="watch_inactive";
            this.ctx.storage.sql.exec("UPDATE operations SET data=? WHERE id=?",JSON.stringify(op),operationId);
            return op.outcome;
          }
        }
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
    const m:StoredMessage = {id:await digest(hashBytes),direction:"incoming",from:clipped(message.from,254),to:[c.address],subject:clipped(parsed.subject ?? "",998),text:clipped(parsed.text ?? "",TEXT_LIMIT),created_at:new Date().toISOString(),message_id:messageId(parsed.messageId),references:[...ids(parsed.references),...ids(parsed.inReplyTo)].slice(-20),auto_submitted:!message.from || parsed.headers.some(h => (h.key.toLowerCase() === "auto-submitted" && h.value.trim().toLowerCase() !== "no") || h.key.toLowerCase().startsWith("list-") || (h.key.toLowerCase() === "precedence" && /bulk|list|junk/i.test(h.value)) || (h.key.toLowerCase() === "content-type" && /multipart\/report|message\/delivery-status/i.test(h.value))),attachments:parsed.attachments.slice(0,100).map(a => ({filename:clipped((a.filename ?? "").replace(/[\x00-\x1f\x7f]/g,""),256),type:clipped(a.mimeType,128),size: typeof a.content === "string" ? encoder.encode(a.content).length : a.content.byteLength}))};
    await env.MAILBOX.get(env.MAILBOX.idFromName(c.owner)).ingest(c.owner,m);
  },
} satisfies ExportedHandler<Env>;
