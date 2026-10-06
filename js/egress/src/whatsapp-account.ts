import { DurableObject } from "cloudflare:workers";
import { CredentialVault, type CredentialVaultEnv, type EncryptedEnvelope } from "./credential-vault";
import type { WhatsAppAuthStore, WhatsAppEvent, WhatsAppMessage, WhatsAppStatus, WhatsAppTransport, WhatsAppTransportFactory } from "./whatsapp-transport";

type Meta = { authorized: boolean; id: string; state: WhatsAppStatus["state"]; attempt: WhatsAppStatus["attempt"]; retries: number; retry_at: number | null; oldest: number | null; received: number | null; history_complete: boolean };
type Receipt = { phone_hash: string; attempt: NonNullable<WhatsAppStatus["attempt"]> };
type Row = { id: string; chat_id: string; timestamp: number; expires_at: number | null; revision: number; tombstone: number; data: string };
const TTL = 5 * 60_000;
const OPERATION = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const SAFE_HEADERS = { "cache-control": "no-store", "referrer-policy": "no-referrer" };

/** One account owner per DO. Only the user broker may reach this binding. */
export class WhatsAppAccount extends DurableObject<CredentialVaultEnv> {
  private readonly store: DurableObjectStorage;
  private readonly vault: CredentialVault;
  private meta!: Meta;
  private queue: Promise<unknown> = Promise.resolve();
  private socket: WhatsAppTransport | null = null;
  private opening: Promise<WhatsAppTransport> | null = null;
  private generation = 0;
  private socketConnected = false;

  constructor(ctx: DurableObjectState, env: CredentialVaultEnv) {
    super(ctx, env);
    this.store = ctx.storage;
    this.vault = new CredentialVault(env, `whatsapp/${ctx.id.toString()}`);
    ctx.blockConcurrencyWhile(async () => {
      this.store.sql.exec(`CREATE TABLE IF NOT EXISTS wa_records (
        kind TEXT NOT NULL, id TEXT NOT NULL, chat_id TEXT NOT NULL DEFAULT '', timestamp REAL NOT NULL DEFAULT 0,
        expires_at REAL, revision REAL NOT NULL DEFAULT 0, tombstone INTEGER NOT NULL DEFAULT 0,
        data TEXT NOT NULL, PRIMARY KEY(kind,chat_id,id))`);
      this.store.sql.exec("CREATE INDEX IF NOT EXISTS wa_read ON wa_records(kind,chat_id,timestamp DESC,id DESC)");
      this.meta = await this.store.get<Meta>("meta") ?? {
        authorized: false, id: base64(crypto.getRandomValues(new Uint8Array(32))), state: "disconnected", attempt: null,
        retries: 0, retry_at: null, oldest: null, received: null, history_complete: false,
      };
      this.meta.authorized ??= this.meta.state === "connected" || this.meta.attempt?.state === "paired";
      // A persisted 'connected' receipt is not proof of a live socket after eviction.
      if (this.meta.state === "connected" || this.meta.state === "connecting") this.meta.state = "reconnecting";
      if (this.meta.attempt && ["requested", "ready"].includes(this.meta.attempt.state)) {
        this.meta.attempt.state = "unknown";
        this.meta.state = "disconnected";
        await this.store.delete("secret:pairing");
      }
      await this.saveAttempt();
      if (this.meta.authorized) await this.schedule();
      else if (this.meta.attempt && ["requested", "ready", "unknown"].includes(this.meta.attempt.state)) await this.store.setAlarm(Math.max(Date.now() + 1, this.meta.attempt.expires_at));
    });
  }

  protected transportFactory(): WhatsAppTransportFactory {
    // Status/catalog reads do not need the protocol stack or its WASM bridge.
    return { connect: async callbacks => {
      const { createWhatsAppTransportFactory } = await import("./whatsapp-runtime");
      return createWhatsAppTransportFactory().connect(callbacks);
    } };
  }
  private serial<T>(fn: () => Promise<T>): Promise<T> {
    const work = this.queue.then(fn);
    this.queue = work.catch(() => undefined);
    return work;
  }
  private async saveMeta(): Promise<void> { await this.store.put("meta", this.meta); }
  private async seal(key: string, value: unknown): Promise<void> { await this.store.put(key, await this.vault.seal(encode(value))); }
  private async secret<T>(key: string): Promise<T | null> {
    const envelope = await this.store.get<EncryptedEnvelope>(key);
    if (!envelope) return null;
    const opened = await this.vault.open<unknown>(envelope);
    if (opened.reseal) await this.store.put(key, await this.vault.seal(opened.value));
    return decode(opened.value) as T;
  }
  private status(): WhatsAppStatus {
    return { id: this.meta.id, connection_id: this.meta.id, label: "WhatsApp", connected: this.meta.authorized,
      socket_connected: this.socketConnected, state: this.meta.state, attempt: this.meta.attempt, retry_at: this.meta.retry_at,
      coverage: { source: "linked_device", complete: false, history_complete: this.meta.history_complete,
        oldest_timestamp: this.meta.oldest, last_received_at: this.meta.received,
        note: "Only messages delivered to this linked device are available. Older history may be incomplete; view-once and expired content are excluded." } };
  }
  async fetch(request: Request): Promise<Response> {
    try {
      const url = new URL(request.url);
      const selected = request.headers.get("x-nanocodex-connector-connection");
      if (selected && selected !== this.meta.id) return json({ error: "connection_not_found" }, 404);
      if (request.method === "DELETE" && url.pathname.startsWith("/connections/")) {
        if (url.pathname !== `/connections/${this.meta.id}`) return json({ error: "connection_not_found" }, 404);
        return await this.logout();
      }
      if (request.method === "POST" && url.pathname === "/start") return await this.start(await body(request));
      if (request.method === "POST" && ["/logout", "/revoke", "/disconnect"].includes(url.pathname)) return await this.logout();
      if (request.method === "POST" && url.pathname === "/history") return await this.history(await body(request));
      if (request.method !== "GET") return json({ error: "method_not_allowed" }, 405);
      await this.serial(() => this.expire());
      if (url.pathname === "/status") {
        if (this.meta.state === "reconnecting" && !this.meta.retry_at) await this.serial(() => this.schedule());
        return json(this.status());
      }
      if (url.pathname === "/pairing") {
        if (url.searchParams.get("operation_id") !== this.meta.attempt?.operation_id) return json({ error: "attempt_not_found" }, 404);
        const pending = await this.serial(() => this.secret<{ code: string; expires_at: number }>("secret:pairing"));
        if (!pending || pending.expires_at <= Date.now() || this.meta.attempt?.state !== "ready") return json({ error: "pairing_unavailable", attempt: this.meta.attempt }, 409);
        return json({ operation_id: this.meta.attempt.operation_id, code: pending.code, pairing_code: pending.code, expires_at: pending.expires_at });
      }
      if (["/chats", "/messages", "/search", "/contacts", "/context"].includes(url.pathname)) return this.read(url);
      return json({ error: "not_found" }, 404);
    } catch (error) {
      // Provider exceptions can contain Noise/Signal material or pairing codes.
      return json({ error: error instanceof InputError ? "invalid_request" : "whatsapp_unavailable" }, error instanceof InputError ? 400 : 503);
    }
  }

  private async start(input: Record<string, unknown>): Promise<Response> {
    const op = field(input.operation_id, 36);
    const phone = field(input.phone, 16).replace(/^\+/, "");
    if (!OPERATION.test(op) || !/^[1-9][0-9]{6,14}$/.test(phone)) throw new InputError();
    const phoneHash = base64(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(phone))));
    const reserved = await this.serial(async () => {
      await this.expire();
      const previous = await this.store.get<Receipt>(`attempt:${op}`);
      if (previous) return { existing: true, mismatch: previous.phone_hash !== phoneHash, attempt: previous.attempt };
      if (this.meta.authorized || (this.meta.attempt && ["requested", "ready", "unknown"].includes(this.meta.attempt.state) && this.meta.attempt.expires_at > Date.now())) return { conflict: true };
      const attempt: NonNullable<WhatsAppStatus["attempt"]> = { operation_id: op, state: "requested", expires_at: Date.now() + TTL };
      this.detach();
      await this.clearAuth();
      this.meta.id = base64(crypto.getRandomValues(new Uint8Array(32)));
      this.meta.attempt = attempt;
      this.meta.state = "connecting";
      this.meta.retries = 0;
      this.meta.retry_at = null;
      await this.store.transaction(async tx => {
        await tx.put(`attempt:${op}`, { phone_hash: phoneHash, attempt });
        await tx.put("meta", this.meta);
      });
      await this.store.setAlarm(attempt.expires_at);
      return { existing: false, epoch: this.generation };
    });
    if (reserved.mismatch) return json({ error: "operation_conflict" }, 409);
    if (reserved.conflict) return json({ error: "pairing_in_progress_or_connected", ...this.status() }, 409);
    if (reserved.existing) return json({ ...this.status(), attempt: reserved.attempt });
    try {
      const socket = await this.openTransport(reserved.epoch);
      const code = await socket.requestPairingCode(phone);
      await this.serial(async () => {
        if (reserved.epoch !== this.generation || this.meta.attempt?.operation_id !== op || this.meta.attempt.state !== "requested") return;
        if (this.meta.attempt.expires_at <= Date.now()) { await this.expire(); return; }
        if (!/^[A-Za-z0-9-]{4,32}$/.test(code)) throw new Error("invalid pairing response");
        await this.seal("secret:pairing", { code, expires_at: this.meta.attempt.expires_at });
        this.meta.attempt.state = "ready";
        this.meta.state = "pairing";
        await this.saveAttempt();
      });
    } catch {
      await this.serial(async () => {
        if (reserved.epoch === this.generation && this.meta.attempt?.operation_id === op && this.meta.attempt.state === "requested") { this.meta.attempt.state = "unknown"; this.meta.state = "disconnected"; await this.saveAttempt(); }
      });
    }
    return json(this.status(), 202);
  }
  private async saveAttempt(): Promise<void> {
    const attempt = this.meta.attempt;
    await this.store.transaction(async tx => {
      if (attempt) {
        const receipt = await tx.get<Receipt>(`attempt:${attempt.operation_id}`);
        if (receipt) await tx.put(`attempt:${attempt.operation_id}`, { ...receipt, attempt });
      }
      await tx.put("meta", this.meta);
    });
  }
  private auth(epoch: number): WhatsAppAuthStore {
    const valid = () => {
      if (epoch !== this.generation || this.meta.state === "revoked"
        || (!this.meta.authorized && (!this.meta.attempt || this.meta.attempt.expires_at <= Date.now()))) throw new Error("session ended");
    };
    return {
      credentials: () => this.serial(async () => { valid(); return this.secret<Record<string, unknown>>("secret:creds"); }),
      saveCredentials: update => this.serial(async () => {
        valid(); const current = await this.secret<Record<string, unknown>>("secret:creds") ?? {};
        await this.seal("secret:creds", { ...current, ...update });
        // Registered credentials are emitted only after the phone approves linking.
        // WhatsApp then requests a socket restart before the first open event.
        if (update.registered === true) {
          this.meta.authorized = true;
          if (this.meta.attempt) this.meta.attempt.state = "paired";
          await this.store.delete("secret:pairing"); await this.saveAttempt();
        }
      }),
      getKeys: (type, ids) => this.serial(async () => {
        valid(); const result: Record<string, unknown> = {};
        for (const id of ids) { const value = await this.secret(`secret:key:${JSON.stringify([type, id])}`); if (value !== null) result[id] = value; }
        return result;
      }),
      setKeys: data => this.serial(async () => {
        valid(); const updates: [string, EncryptedEnvelope | null][] = [];
        for (const [type, entries] of Object.entries(data)) for (const [id, value] of Object.entries(entries)) {
          updates.push([`secret:key:${JSON.stringify([type, id])}`, value === null ? null : await this.vault.seal(encode(value))]);
        }
        await this.store.transaction(async tx => { for (const [key, value] of updates) { if (value === null) await tx.delete(key); else await tx.put(key, value); } });
      }),
    };
  }
  private async openTransport(epoch = this.generation): Promise<WhatsAppTransport> {
    if (epoch !== this.generation || this.meta.state === "revoked") throw new Error("session ended");
    if (this.socket) return this.socket;
    if (this.opening) return this.opening;
    const opening = this.transportFactory().connect({ auth: this.auth(epoch),
      onConnection: update => this.serial(async () => {
        if (epoch !== this.generation || this.meta.state === "revoked") return;
        if (update.state === "open") {
          if (!this.meta.authorized && (!this.meta.attempt || this.meta.attempt.expires_at <= Date.now() || !["requested", "ready", "unknown"].includes(this.meta.attempt.state))) { await this.expire(); return; }
          this.meta.authorized = true; this.socketConnected = true;
          this.meta.state = "connected"; this.meta.retries = 0; this.meta.retry_at = null;
          if (this.meta.attempt) this.meta.attempt.state = "paired";
          await this.store.delete("secret:pairing"); await this.saveAttempt();
          await this.store.setAlarm(Date.now() + 60_000);
        } else if (update.state === "close") {
          this.detach();
          if (update.loggedOut || update.retryable === false) await this.revoke();
          else if (this.meta.authorized) await this.schedule();
          else {
            this.meta.state = "disconnected";
            if (this.meta.attempt && ["requested", "ready"].includes(this.meta.attempt.state)) this.meta.attempt.state = "unknown";
            await this.store.delete("secret:pairing"); await this.saveAttempt();
          }
        }
      }),
      onEvents: events => this.serial(async () => {
        await this.expire();
        if (epoch === this.generation && this.meta.authorized) await this.ingest(events);
      }),
    });
    this.opening = opening;
    try {
      const socket = await opening;
      if (epoch !== this.generation) { await socket.close(); throw new Error("session ended"); }
      this.socket = socket;
      return socket;
    } finally { if (this.opening === opening) this.opening = null; }
  }
  /** Invalidate callbacks before closing; a close callback may itself enter serial(). */
  private detach(): void {
    this.generation++; this.socketConnected = false;
    const socket = this.socket; this.socket = null; this.opening = null;
    if (socket) this.ctx.waitUntil(socket.close().catch(() => undefined));
  }
  private async clearAuth(): Promise<void> {
    for (;;) {
      const keys = await this.store.list({ prefix: "secret:", limit: 128 });
      if (!keys.size) break;
      await this.store.delete([...keys.keys()]);
    }
  }
  private async logout(): Promise<Response> {
    const socket = this.socket;
    await this.serial(async () => {
      this.generation++; this.socketConnected = false; this.socket = null; this.opening = null;
      await this.revoke();
    });
    let remote = "unavailable";
    if (socket) { try { await socket.logout(); remote = "confirmed"; } catch { remote = "unknown"; } finally { try { await socket.close(); } catch {} } }
    return json({ ...this.status(), remote_logout: remote });
  }
  private async revoke(): Promise<void> {
    this.meta.authorized = false; this.meta.state = "revoked"; this.meta.retry_at = null;
    if (this.meta.attempt) this.meta.attempt.state = "expired";
    await this.clearAuth(); await this.saveAttempt(); await this.store.deleteAlarm();
    this.store.sql.exec("DELETE FROM wa_records");
    this.meta.oldest = null; this.meta.received = null; this.meta.history_complete = false; await this.saveMeta();
  }
  private async schedule(): Promise<void> {
    if (!this.meta.authorized || this.meta.state === "revoked") return;
    this.meta.state = "reconnecting";
    this.meta.retries++;
    this.meta.retry_at = Date.now() + Math.min(300_000, 1000 * 2 ** Math.min(8, this.meta.retries - 1));
    await this.saveMeta(); await this.store.setAlarm(this.meta.retry_at);
  }
  async alarm(): Promise<void> {
    const epoch = await this.serial(async () => {
      await this.expire();
      if (!this.meta.authorized || this.meta.state === "revoked") return null;
      if (this.socketConnected) { await this.store.setAlarm(Date.now() + 60_000); return null; }
      // An earlier connect that never opened must not suppress recovery forever.
      this.detach();
      this.meta.retry_at = Date.now() + 60_000;
      await this.saveMeta(); await this.store.setAlarm(this.meta.retry_at);
      return this.generation;
    });
    if (epoch === null) return;
    try { await this.openTransport(epoch); }
    catch { await this.serial(async () => { if (epoch === this.generation) await this.schedule(); }); }
  }
  private async expire(): Promise<void> {
    const now = Date.now();
    if (this.meta.attempt && this.meta.attempt.expires_at <= now && ["requested", "ready", "unknown"].includes(this.meta.attempt.state)) {
      this.detach();
      this.meta.attempt.state = "expired"; this.meta.state = "disconnected";
      await this.clearAuth(); await this.saveAttempt(); await this.store.deleteAlarm();
    }
    this.store.sql.exec("UPDATE wa_records SET data='{}', tombstone=1 WHERE kind='message' AND expires_at IS NOT NULL AND expires_at<=? AND tombstone=0", now);
  }
  private async ingest(events: WhatsAppEvent[]): Promise<void> {
    if (events.length > 10_000) throw new Error("event batch exceeds limit");
    this.store.transactionSync(() => {
      for (const event of events) {
        if (event.type === "history") { this.meta.history_complete = event.complete; if (event.oldest_timestamp !== undefined) this.meta.oldest = Math.min(this.meta.oldest ?? Infinity, event.oldest_timestamp); continue; }
        if (event.type === "chat" || event.type === "contact") {
          const item = event.type === "chat" ? event.chat : event.contact;
          const timestamp = "timestamp" in item ? item.timestamp ?? 0 : 0;
          this.store.sql.exec("INSERT INTO wa_records(kind,id,data,timestamp) VALUES(?,?,?,?) ON CONFLICT(kind,chat_id,id) DO UPDATE SET data=excluded.data,timestamp=MAX(timestamp,excluded.timestamp)", event.type, item.id, JSON.stringify(item), timestamp);
          continue;
        }
        const message: WhatsAppMessage = event.type === "revoke" ? { id: event.id, chat_id: event.chat_id, timestamp: event.timestamp, revoked: true } : event.message;
        if (!message.id || !message.chat_id || !Number.isFinite(message.timestamp)) continue;
        const existing = this.store.sql.exec<Row>("SELECT * FROM wa_records WHERE kind='message' AND chat_id=? AND id=?", message.chat_id, message.id).toArray()[0];
        const revision = message.revision ?? 0;
        if (existing?.tombstone || (existing && existing.revision > revision && !message.revoked)) continue;
        const expiresAt = existing?.expires_at !== null && existing?.expires_at !== undefined
          ? Math.min(existing.expires_at, message.expires_at ?? Infinity) : message.expires_at;
        const wasViewOnce = existing ? (JSON.parse(existing.data) as WhatsAppMessage).view_once === true : false;
        const viewOnce = wasViewOnce || message.view_once === true;
        const tombstone = message.revoked || (expiresAt !== undefined && expiresAt <= Date.now());
        // Explicit projection avoids retaining raw protobuf/media keys or view-once payloads.
        const safe: WhatsAppMessage = { id: message.id, chat_id: message.chat_id, timestamp: existing?.timestamp || message.timestamp,
          ...(message.sender ? { sender: message.sender } : {}), from_me: message.from_me === true,
          kind: message.kind ?? "text", view_once: viewOnce,
          ...(expiresAt !== undefined ? { expires_at: expiresAt } : {}),
          ...(!viewOnce && !tombstone && message.text ? { text: message.text.slice(0, 64_000) } : {}),
          revision, ...(tombstone ? { revoked: true } : {}) };
        this.store.sql.exec(`INSERT INTO wa_records(kind,id,chat_id,timestamp,expires_at,revision,tombstone,data) VALUES('message',?,?,?,?,?,?,?)
          ON CONFLICT(kind,chat_id,id) DO UPDATE SET expires_at=excluded.expires_at,revision=excluded.revision,tombstone=excluded.tombstone,data=excluded.data`,
          safe.id, safe.chat_id, safe.timestamp, safe.expires_at ?? null, revision, tombstone ? 1 : 0, tombstone ? "{}" : JSON.stringify(safe));
        this.meta.oldest = Math.min(this.meta.oldest ?? Infinity, safe.timestamp);
      }
    });
    this.meta.received = Date.now(); await this.saveMeta();
  }
  private read(url: URL): Response {
    const kind = url.pathname === "/chats" ? "chat" : url.pathname === "/contacts" ? "contact" : "message";
    const limit = number(url.searchParams.get("limit") ?? 50, 1, 100);
    const chat = url.searchParams.get("chat_id") ?? url.searchParams.get("chat");
    if (["/messages", "/context"].includes(url.pathname) && !chat) throw new InputError();
    const clauses = ["kind=?", "tombstone=0", "(expires_at IS NULL OR expires_at>?)"];
    const params: (string | number)[] = [kind, Date.now()];
    if (chat) { clauses.push("chat_id=?"); params.push(chat); }
    const q = url.searchParams.get("q");
    if (url.pathname === "/search" || (url.pathname === "/contacts" && q !== null)) {
      if (!q?.trim() || q.length > 256) throw new InputError();
      clauses.push(url.pathname === "/contacts"
        ? "(json_extract(data,'$.name') LIKE ? ESCAPE '\\' OR id LIKE ? ESCAPE '\\')"
        : "json_extract(data,'$.text') LIKE ? ESCAPE '\\'");
      const pattern = `%${q.replace(/[\\%_]/g, "\\$&")}%`;
      params.push(pattern); if (url.pathname === "/contacts") params.push(pattern);
    }
    if (url.pathname === "/context") {
      const id = field(url.searchParams.get("id"), 256);
      if (url.searchParams.has("cursor") || url.searchParams.has("before")) throw new InputError();
      const where = clauses.join(" AND ");
      const anchor = this.store.sql.exec<Row>(`SELECT * FROM wa_records WHERE ${where} AND id=?`, ...params, id).toArray()[0];
      if (!anchor) return json({ error: "message_not_found" }, 404);
      const older = this.store.sql.exec<Row>(`SELECT * FROM wa_records WHERE ${where} AND (timestamp<? OR (timestamp=? AND id<?)) ORDER BY timestamp DESC,id DESC LIMIT ?`, ...params, anchor.timestamp, anchor.timestamp, anchor.id, Math.floor((limit - 1) / 2)).toArray();
      const newer = this.store.sql.exec<Row>(`SELECT * FROM wa_records WHERE ${where} AND (timestamp>? OR (timestamp=? AND id>?)) ORDER BY timestamp ASC,id ASC LIMIT ?`, ...params, anchor.timestamp, anchor.timestamp, anchor.id, limit - 1 - older.length).toArray();
      const items = [...older.reverse(), anchor, ...newer].map(row => JSON.parse(row.data));
      return json({ items, messages: items, anchor_id: id, next_cursor: null, coverage: this.status().coverage });
    }
    const scope = JSON.stringify([this.meta.id, url.pathname, chat, q, url.searchParams.get("before")]);
    const token = url.searchParams.get("cursor");
    if (token) {
      let parsed: unknown; try { parsed = JSON.parse(new TextDecoder().decode(Uint8Array.from(atob(token.replaceAll("-", "+").replaceAll("_", "/")), c => c.charCodeAt(0)))); } catch { throw new InputError(); }
      if (!Array.isArray(parsed) || parsed.length !== 4 || parsed[3] !== scope || !Number.isFinite(parsed[0]) || typeof parsed[1] !== "string" || typeof parsed[2] !== "string") throw new InputError();
      clauses.push("(timestamp<? OR (timestamp=? AND (id<? OR (id=? AND chat_id<?))))"); params.push(parsed[0], parsed[0], parsed[1], parsed[1], parsed[2]);
    }
    const before = url.searchParams.get("before");
    if (before) { clauses.push("timestamp<?"); params.push(number(before, 0, Number.MAX_SAFE_INTEGER)); }
    const rows = this.store.sql.exec<Row>(`SELECT * FROM wa_records WHERE ${clauses.join(" AND ")} ORDER BY timestamp DESC,id DESC,chat_id DESC LIMIT ?`, ...params, limit + 1).toArray();
    const page = rows.slice(0, limit), last = page.at(-1);
    const items = page.map(row => JSON.parse(row.data));
    const next = rows.length > limit && last ? base64(new TextEncoder().encode(JSON.stringify([last.timestamp, last.id, last.chat_id, scope]))) : null;
    return json({ items, ...(kind === "message" ? { messages: items } : kind === "chat" ? { chats: items } : { contacts: items }), next_cursor: next, coverage: this.status().coverage });
  }
  private async history(input: Record<string, unknown>): Promise<Response> {
    const chat_id = field(input.chat_id, 256), before = number(input.before, 1, Number.MAX_SAFE_INTEGER), limit = number(input.limit ?? 50, 1, 100);
    if (!this.socket || this.meta.state !== "connected") return json({ error: "not_connected" }, 409);
    if (!this.socket.requestHistory) return json({ error: "history_unavailable", coverage: this.status().coverage }, 409);
    const row = this.store.sql.exec<Row>("SELECT * FROM wa_records WHERE kind='message' AND chat_id=? AND timestamp<=? AND tombstone=0 ORDER BY timestamp DESC,id DESC LIMIT 1", chat_id, before).toArray()[0];
    if (!row) return json({ error: "history_anchor_not_found", coverage: this.status().coverage }, 409);
    const anchor = JSON.parse(row.data) as WhatsAppMessage;
    await this.socket.requestHistory({ chat_id, before: row.timestamp, limit, id: row.id, from_me: anchor.from_me === true });
    return json({ accepted: true, coverage: this.status().coverage }, 202);
  }
}
class InputError extends Error {}
function field(value: unknown, max: number): string { if (typeof value !== "string" || !value || value.length > max) throw new InputError(); return value; }
function number(value: unknown, min: number, max: number): number { const result = typeof value === "number" || typeof value === "string" ? Number(value) : NaN; if (!Number.isFinite(result) || result < min || result > max || !Number.isInteger(result)) throw new InputError(); return result; }
async function body(request: Request): Promise<Record<string, unknown>> { const text = await request.text(); if (text.length > 4096) throw new InputError(); let value: unknown; try { value = JSON.parse(text); } catch { throw new InputError(); } if (!value || typeof value !== "object" || Array.isArray(value)) throw new InputError(); return value as Record<string, unknown>; }
function json(value: unknown, status = 200): Response { return Response.json(value, { status, headers: SAFE_HEADERS }); }
function base64(bytes: Uint8Array): string { let text = ""; for (const byte of bytes) text += String.fromCharCode(byte); return btoa(text).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, ""); }
function encode(value: unknown): unknown {
  if (value instanceof Uint8Array) return { __wa_bytes: base64(value) };
  if (Array.isArray(value)) return value.map(encode);
  if (value && typeof value === "object") return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, encode(item)]));
  return value;
}
function decode(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(decode);
  if (value && typeof value === "object") {
    const object = value as Record<string, unknown>;
    if (typeof object.__wa_bytes === "string" && Object.keys(object).length === 1) return Uint8Array.from(atob(object.__wa_bytes.replaceAll("-", "+").replaceAll("_", "/")), c => c.charCodeAt(0));
    return Object.fromEntries(Object.entries(object).map(([key, item]) => [key, decode(item)]));
  }
  return value;
}
