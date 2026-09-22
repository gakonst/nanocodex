import { phoneAdminConfigured } from "./phone-admin";
import { Container } from "@cloudflare/containers";
import { verifyTwilioWebhookSignature, type TwilioVoiceEnv } from "./twilio-voice";

export interface PhoneContainerEnv extends TwilioVoiceEnv {
  NANOCODEX_PHONE_BRIDGE_TOKEN?: string;
  NANOCODEX_PHONE_OWNER_ID?: string;
  NANOCODEX_PHONE_ADMIN_ID?: string;
  NANOCODEX_PHONE_MANAGED_API_KEY?: string;
  NANOCODEX_PHONE_PUBLIC_ORIGIN?: string;
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const MAX_ROW = 1024 * 1024;
const MAX_STORE = 32 * MAX_ROW;
const statuses = new Set(["preparing", "unknown", "queued", "initiated", "ringing", "in-progress", "completed", "busy", "failed", "no-answer", "canceled"]);
const recordKeys = new Set(["call_id", "status", "transcript", "transcript_truncated", "transcript_bytes", "max_duration_seconds", "error", "sid", "stop_requested", "hangup_attempted", "dial_requested", "callback_sequence", "delegate_agent_id", "delegate_session_id", "delegate_cleaned", "to", "steering"]);
type CallRow = { id: string; agent: string; operation: string; fingerprint: string; record: string };
function object(value: unknown): value is Record<string, unknown> { return !!value && typeof value === "object" && !Array.isArray(value); }
export function validPhoneCheckpoint(value: unknown): value is CallRow {
  if (!object(value) || Object.keys(value).length !== 5 || !["id", "agent", "operation"].every(key => typeof value[key] === "string" && UUID.test(value[key]))
    || typeof value.fingerprint !== "string" || !/^[0-9a-f]{64}$/.test(value.fingerprint) || typeof value.record !== "string" || value.record.length > MAX_ROW) return false;
  let r: unknown;
  try { r = JSON.parse(value.record); } catch { return false; }
  if (!object(r)) return false;
  if (Object.keys(r).some(key => !recordKeys.has(key)) || r.call_id !== value.id || typeof r.status !== "string" || !statuses.has(r.status)
    || !Number.isInteger(r.max_duration_seconds) || Number(r.max_duration_seconds) < 30 || Number(r.max_duration_seconds) > 600
    || !Array.isArray(r.transcript) || r.transcript.length > 200) return false;
  if ("to" in r && (typeof r.to !== "string" || !/^\+[1-9][0-9]{1,14}$/.test(r.to))) return false;
  if ("steering" in r) {
    if (!Array.isArray(r.steering) || r.steering.length > 16) return false;
    const ids = new Set();
    for (const item of r.steering) {
      if (!object(item) || Object.keys(item).length !== 4 || typeof item.operation_id !== "string" || !UUID.test(item.operation_id)
        || ids.has(item.operation_id) || typeof item.fingerprint !== "string" || !/^[0-9a-f]{64}$/.test(item.fingerprint)
        || typeof item.instructions !== "string" || !item.instructions.trim() || new TextEncoder().encode(item.instructions).length > 8000
        || !["pending", "submitted", "not_applied", "unknown"].includes(String(item.status))) return false;
      ids.add(item.operation_id);
    }
  }
  for (const entry of r.transcript) {
    if (!object(entry) || Object.keys(entry).some(key => !["speaker", "text"].includes(key))
      || !["user", "assistant"].includes(String(entry.speaker)) || typeof entry.text !== "string" || entry.text.length > 4000) return false;
  }
  for (const key of ["stop_requested", "hangup_attempted", "dial_requested", "transcript_truncated", "delegate_cleaned"]) if (key in r && typeof r[key] !== "boolean") return false;
  for (const key of ["transcript_bytes", "callback_sequence"]) if (key in r && (!Number.isSafeInteger(r[key]) || Number(r[key]) < 0)) return false;
  if ("error" in r && (typeof r.error !== "string" || !/^[a-z_]{1,100}$/.test(r.error))) return false;
  if ("delegate_session_id" in r && (typeof r.delegate_session_id !== "string" || !UUID.test(r.delegate_session_id))) return false;
  if (("delegate_agent_id" in r) !== ("delegate_session_id" in r)) return false;
  if ("delegate_agent_id" in r && (typeof r.delegate_agent_id !== "string" || !UUID.test(r.delegate_agent_id) || r.delegate_agent_id === value.agent)) return false;
  if ("sid" in r && (typeof r.sid !== "string" || !/^CA[0-9a-f]{32}$/i.test(r.sid))) return false;
  return new TextEncoder().encode(JSON.stringify(value)).length <= MAX_ROW;
}

async function authorized(request: Request, token: string | undefined): Promise<boolean> {
  if (!token || token.length < 32 || /\s/.test(token)) return false;
  const supplied = request.headers.get("authorization") ?? "";
  const digest = (value: string) => crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  const [left, right] = await Promise.all([digest(supplied), digest(`Bearer ${token}`)]);
  return crypto.subtle.timingSafeEqual(left, right);
}
const json = (value: unknown, status = 200) => Response.json(value, { status, headers: { "Cache-Control": "no-store" } });

/** Single-owner container. The Worker authenticates public control routes before forwarding.
 * /internal/state is served before container startup: native hydration calls back into this DO.
 * Never wrap startup in blockConcurrencyWhile, which would deadlock that callback.
 */
export class PhoneContainer extends Container<PhoneContainerEnv> {
  defaultPort = 8788;
  enableInternet = true; // Native WebRTC needs UDP egress.
  sleepAfter = "15m";
  private resolvedAuthToken?: string;
  private authLookupStatus?: number;
  private authTokenMetadata = { account_matches: false, token_returned: false, token_usable: false };

  constructor(ctx: DurableObjectState, env: PhoneContainerEnv) {
    super(ctx as ConstructorParameters<typeof Container<PhoneContainerEnv>>[0], env);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS phone_calls (
      id TEXT PRIMARY KEY, agent TEXT NOT NULL, operation TEXT NOT NULL,
      fingerprint TEXT NOT NULL, record TEXT NOT NULL, UNIQUE(agent, operation))`);
  }

  private async twilio(path: string): Promise<Response | undefined> {
    const { TWILIO_ACCOUNT_SID: account, TWILIO_API_KEY_SID: key, TWILIO_API_KEY_SECRET: secret, TWILIO_AUTH_TOKEN: token } = this.env;
    if (!account || !/^AC[0-9a-f]{32}$/i.test(account)) return;
    if ((key || secret) && (!key || !/^SK[0-9a-f]{32}$/i.test(key) || !secret)) return;
    if (!key && !token) return;
    try {
      return await fetch(`https://api.twilio.com/2010-04-01/Accounts/${account}${path}`, {
        headers: { Authorization: `Basic ${btoa(`${key ?? account}:${key ? secret : token}`)}` },
        redirect: "manual", signal: AbortSignal.timeout(10_000),
      });
    } catch { return; }
  }

  private async authToken(): Promise<string | undefined> {
    if (this.env.TWILIO_AUTH_TOKEN) return this.env.TWILIO_AUTH_TOKEN;
    if (this.resolvedAuthToken) return this.resolvedAuthToken;
    const response = await this.twilio(".json");
    this.authLookupStatus = response?.status;
    if (!response?.ok) return; // Restricted API keys commonly return 403. Never substitute the API secret.
    try {
      const value: unknown = await response.json();
      this.authTokenMetadata = {
        account_matches: object(value) && value.sid === this.env.TWILIO_ACCOUNT_SID,
        token_returned: object(value) && typeof value.auth_token === "string" && value.auth_token.length > 0,
        token_usable: object(value) && typeof value.auth_token === "string" && /^[A-Za-z0-9_-]{32,128}$/.test(value.auth_token) && !/^[xX]+$/.test(value.auth_token),
      };
      if (object(value) && this.authTokenMetadata.account_matches && this.authTokenMetadata.token_usable && typeof value.auth_token === "string") {
        this.resolvedAuthToken = value.auth_token; // Memory only; never Durable Object storage.
        return value.auth_token;
      }
    } catch { /* Provider errors never leave the credential boundary. */ }
  }

  private async setupMetadata(): Promise<Response> {
    const token = await this.authToken();
    const response = await this.twilio("/IncomingPhoneNumbers.json?PageSize=1000");
    const phone_numbers: string[] = [];
    if (response?.ok) {
      try {
        const value: unknown = await response.json();
        if (object(value) && Array.isArray(value.incoming_phone_numbers)) {
          for (const number of value.incoming_phone_numbers.slice(0, 1000)) {
            if (object(number) && object(number.capabilities) && number.capabilities.voice === true
              && typeof number.phone_number === "string" && /^\+[1-9]\d{1,14}$/.test(number.phone_number)) phone_numbers.push(number.phone_number);
          }
        }
      } catch { /* Return only safe metadata. */ }
    }
    const callerIds = await this.twilio("/OutgoingCallerIds.json?PageSize=1000");
    const verified_caller_ids: string[] = [];
    if (callerIds?.ok) {
      try {
        const value: unknown = await callerIds.json();
        if (object(value) && Array.isArray(value.outgoing_caller_ids)) {
          for (const item of value.outgoing_caller_ids.slice(0, 1000)) {
            if (object(item) && typeof item.phone_number === "string" && /^\+[1-9]\d{1,14}$/.test(item.phone_number)) verified_caller_ids.push(item.phone_number);
          }
        }
      } catch { /* Only validated caller IDs leave the provider boundary. */ }
    }
    return json({ phone_numbers, verified_caller_ids, caller_id_lookup_status: callerIds?.status ?? null, number_lookup_available: response?.ok === true, webhook_auth_available: !!token,
      number_lookup_status: response?.status ?? null, auth_lookup_status: this.authLookupStatus ?? null, auth_token_metadata: this.authTokenMetadata,
      account_sid_valid: /^AC[0-9a-f]{32}$/i.test(this.env.TWILIO_ACCOUNT_SID ?? ""),
      api_key_configured: /^SK[0-9a-f]{32}$/i.test(this.env.TWILIO_API_KEY_SID ?? "") && !!this.env.TWILIO_API_KEY_SECRET });
  }

  private async checkpoint(request: Request): Promise<Response> {
    const sql = this.ctx.storage.sql;
    if (request.method === "GET") {
      const url = new URL(request.url);
      const cursor = url.searchParams.get("cursor");
      if ([...url.searchParams.keys()].some(key => key !== "cursor") || url.searchParams.getAll("cursor").length > 1 || (cursor !== null && !UUID.test(cursor))) return json({ error: "invalid_cursor" }, 400);
      const rows = sql.exec<{ id: string; agent: string; operation: string; fingerprint: string; record: string }>(
        "SELECT id, agent, operation, fingerprint, record FROM phone_calls WHERE id > ? ORDER BY id LIMIT 11", cursor ?? "",
      ).toArray();
      // Native limits hydration responses to 8 MiB, including JSON escaping.
      const calls: typeof rows = []; let pageBytes = 128;
      for (const row of rows.slice(0, 10)) {
        const bytes = new TextEncoder().encode(JSON.stringify(row)).length + 1;
        if (pageBytes + bytes > 7 * MAX_ROW) break;
        calls.push(row); pageBytes += bytes;
      }
      return json({ calls, ...(rows.length > calls.length ? { next_cursor: calls.at(-1)!.id } : {}) });
    }
    if (request.method !== "POST") return json({ error: "method_not_allowed" }, 405);
    let size = 0;
    const chunks: Uint8Array[] = [];
    const reader = request.body?.getReader();
    if (!reader) return json({ error: "invalid_checkpoint" }, 400);
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > MAX_ROW) { await reader.cancel(); return json({ error: "checkpoint_too_large" }, 413); }
      chunks.push(value);
    }
    const bytes = new Uint8Array(size); let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    let value: unknown;
    try { value = JSON.parse(new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(bytes)); } catch { return json({ error: "invalid_checkpoint" }, 400); }
    if (!validPhoneCheckpoint(value)) return json({ error: "invalid_checkpoint" }, 400);
    const row = value;
    // No await from identity checks through write: uniqueness and quota admission are atomic.
    const existing = sql.exec<{ id: string; agent: string; operation: string; fingerprint: string; record: string }>(
      "SELECT id, agent, operation, fingerprint, record FROM phone_calls WHERE id = ? OR (agent = ? AND operation = ?)", row.id, row.agent, row.operation,
    ).toArray();
    if (existing.some(prior => prior.id !== row.id || prior.agent !== row.agent || prior.operation !== row.operation || prior.fingerprint !== row.fingerprint)) return json({ error: "operation_conflict" }, 409);
    const record = row.record;
    const usage = sql.exec<{ count: number; bytes: number }>("SELECT COUNT(*) AS count, COALESCE(SUM(length(CAST(record AS BLOB))), 0) AS bytes FROM phone_calls").one();
    if ((!existing.length && usage.count >= 1000) || usage.bytes - (existing[0] ? new TextEncoder().encode(existing[0].record).length : 0) + new TextEncoder().encode(record).length > MAX_STORE) return json({ error: "checkpoint_capacity" }, 507);
    sql.exec("INSERT INTO phone_calls (id, agent, operation, fingerprint, record) VALUES (?, ?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET record = excluded.record", row.id, row.agent, row.operation, row.fingerprint, record);
    return json({ ok: true });
  }

  async fetch(request: Request): Promise<Response> {
    if (!phoneAdminConfigured(this.env)) return json({ error: "phone_not_configured" }, 503);
    const url = new URL(request.url);
    if (url.pathname.startsWith("/internal/")) {
      if (!await authorized(request, this.env.NANOCODEX_PHONE_BRIDGE_TOKEN)) return json({ error: "unauthorized" }, 401);
      if (url.search && !(url.pathname === "/internal/state" && request.method === "GET")) return json({ error: "invalid_request" }, 400);
      if (url.pathname === "/internal/state") return this.checkpoint(request);
      if (url.pathname === "/internal/setup" && request.method === "GET") return this.setupMetadata();
      return json({ error: "not_found" }, 404);
    }
    const callback = /^\/status\/[0-9a-f-]{36}$/.test(url.pathname) && request.method === "POST" && !url.search;
    const media = /^\/media\/[0-9a-f-]{36}\/$/.test(url.pathname) && request.method === "GET" && !url.search;
    if (!callback && !media && !await authorized(request, this.env.NANOCODEX_PHONE_BRIDGE_TOKEN)) return json({ error: "unauthorized" }, 401);
    const signature = request.headers.get("x-twilio-signature");
    if ((callback || media) && (!signature || !/^[A-Za-z0-9+/]{27}=$/.test(signature))) return json({ error: "invalid_signature" }, 403);
    try {
      const token = await this.authToken();
      if (!token || !this.env.NANOCODEX_PHONE_MANAGED_API_KEY || !this.env.NANOCODEX_PHONE_OWNER_ID
        || !this.env.NANOCODEX_PHONE_BRIDGE_TOKEN || this.env.NANOCODEX_PHONE_BRIDGE_TOKEN.length < 32
        || /\s/.test(this.env.NANOCODEX_PHONE_BRIDGE_TOKEN) || !/^\+[1-9]\d{1,14}$/.test(this.env.TWILIO_VOICE_FROM_NUMBER ?? "")) return json({ error: "phone_not_configured" }, 503);
      const origin = new URL(this.env.NANOCODEX_PHONE_PUBLIC_ORIGIN ?? "https://nanocodex.gakonst.workers.dev");
      if (origin.protocol !== "https:" || origin.username || origin.password || origin.pathname !== "/" || origin.search || origin.hash) return json({ error: "phone_not_configured" }, 503);
      if (callback || media) {
        const publicUrl = `${origin.origin}/v1/phone/bridge${url.pathname}`;
        let fields = new URLSearchParams();
        if (callback) {
          if (!(request.headers.get("content-type") ?? "").startsWith("application/x-www-form-urlencoded")) return json({ error: "invalid_callback" }, 400);
          const reader = request.clone().body?.getReader();
          if (!reader) return json({ error: "invalid_callback" }, 400);
          const chunks: Uint8Array[] = []; let size = 0;
          while (true) {
            const { done, value } = await reader.read(); if (done) break;
            size += value.byteLength;
            if (size > 32 * 1024) { void reader.cancel(); return json({ error: "request_too_large" }, 413); }
            chunks.push(value);
          }
          const bytes = new Uint8Array(size); let offset = 0;
          for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
          fields = new URLSearchParams(new TextDecoder().decode(bytes));
        }
        const authEnv = { TWILIO_AUTH_TOKEN: token };
        if (!await verifyTwilioWebhookSignature(authEnv, publicUrl, fields, signature)
          && !(media && await verifyTwilioWebhookSignature(authEnv, publicUrl.replace(/^https:/, "wss:"), fields, signature))) return json({ error: "invalid_signature" }, 403);
      }
      const envVars: Record<string, string> = {
        NANOCODEX_PHONE_HOST: "0.0.0.0", NANOCODEX_PHONE_PORT: "8788",
        NANOCODEX_PHONE_STATE_URL: `${origin.origin}/v1/phone/bridge/internal/state`,
        NANOCODEX_PHONE_PUBLIC_ORIGIN: origin.origin, NANOCODEX_PHONE_PUBLIC_PREFIX: "/v1/phone/bridge",
        NANOCODEX_PHONE_MANAGED_ORIGIN: "https://nanocodex.gakonst.workers.dev",
        NANOCODEX_PHONE_MANAGED_API_KEY: this.env.NANOCODEX_PHONE_MANAGED_API_KEY,
        NANOCODEX_PHONE_OWNER_ID: this.env.NANOCODEX_PHONE_OWNER_ID,
        NANOCODEX_PHONE_BRIDGE_TOKEN: this.env.NANOCODEX_PHONE_BRIDGE_TOKEN,
        TWILIO_ACCOUNT_SID: this.env.TWILIO_ACCOUNT_SID!, TWILIO_AUTH_TOKEN: token,
        TWILIO_VOICE_FROM_NUMBER: this.env.TWILIO_VOICE_FROM_NUMBER!,
      };
      if (this.env.TWILIO_API_KEY_SID) envVars.TWILIO_API_KEY_SID = this.env.TWILIO_API_KEY_SID;
      if (this.env.TWILIO_API_KEY_SECRET) envVars.TWILIO_API_KEY_SECRET = this.env.TWILIO_API_KEY_SECRET;
      await this.startAndWaitForPorts({ ports: [8788], startOptions: { envVars, enableInternet: true } });
      return await this.containerFetch(request, 8788); // Forward the original response, including WebSocket upgrades.
    } catch { return json({ error: "phone_unavailable" }, 503); }
  }
}
