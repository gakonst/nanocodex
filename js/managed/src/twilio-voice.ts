import { timingSafeEqual } from "node:crypto";

export interface TwilioVoiceEnv {
  TWILIO_ACCOUNT_SID?: string;
  TWILIO_API_KEY_SID?: string;
  TWILIO_API_KEY_SECRET?: string;
  TWILIO_AUTH_TOKEN?: string;
  TWILIO_VOICE_FROM_NUMBER?: string;
}

export type TwilioCallStatus = "queued" | "ringing" | "in-progress" | "completed" | "busy" | "failed" | "no-answer" | "canceled";
export type TwilioVoiceCallbackStatus = TwilioCallStatus | "initiated";
export interface TwilioVoiceCall { sid: string; status: TwilioCallStatus }
export interface CreateTwilioVoiceCallInput {
  to: string;
  /** Trusted, server-configured URLs; never derive these from untrusted Host headers. */
  streamUrl: string;
  statusCallbackUrl: string;
  callId: string;
  /** Provider-enforced connected-call limit, integer seconds (30..600), default 180. */
  maxDurationSeconds?: number;
}
const SID = /^CA[0-9a-f]{32}$/i;
const statuses = new Set<string>(["queued", "ringing", "in-progress", "completed", "busy", "failed", "no-answer", "canceled"]);
const invalid = () => new Error("Invalid Twilio voice input");

export function isE164PhoneNumber(value: unknown): value is string {
  return typeof value === "string" && /^\+[1-9][0-9]{1,14}$/.test(value);
}
function bounded(value: unknown, max: number): value is string {
  return typeof value === "string" && value.length > 0 && value.length <= max && !/[\u0000-\u001f\u007f]/.test(value);
}
function validateUrl(value: string, protocol: "https:" | "wss:"): void {
  if (!bounded(value, 2048) || value.trim() !== value) throw invalid();
  let url: URL;
  try { url = new URL(value); } catch { throw invalid(); }
  if (url.protocol !== protocol || url.username || url.password || url.hash || (protocol === "wss:" && url.search)) throw invalid();
}
function escapeXml(value: string): string {
  return value.replace(/[&<>"']/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&apos;" })[character]!);
}

/** Connect keeps the bidirectional stream attached to the call. */
export function buildTwilioVoiceTwiml(streamUrl: string, callId: string): string {
  validateUrl(streamUrl, "wss:");
  if (!bounded(callId, 256) || /[\ud800-\udfff\ufffe\uffff]/.test(callId)) throw invalid();
  return `<Response><Connect><Stream url="${escapeXml(streamUrl)}"><Parameter name="callId" value="${escapeXml(callId)}" /></Stream></Connect></Response>`;
}

function credentials(env: TwilioVoiceEnv): { accountSid: string; authorization: string } {
  const accountSid = env.TWILIO_ACCOUNT_SID;
  if (!accountSid || !/^AC[0-9a-f]{32}$/i.test(accountSid)) throw new Error("Twilio voice is not configured");
  let user = accountSid;
  let secret = env.TWILIO_AUTH_TOKEN;
  if (env.TWILIO_API_KEY_SID || env.TWILIO_API_KEY_SECRET) {
    if (!env.TWILIO_API_KEY_SID || !/^SK[0-9a-f]{32}$/i.test(env.TWILIO_API_KEY_SID)) throw new Error("Twilio voice is not configured");
    user = env.TWILIO_API_KEY_SID;
    secret = env.TWILIO_API_KEY_SECRET;
  }
  if (!bounded(secret, 256) || !/^[\x21-\x7e]+$/.test(secret)) throw new Error("Twilio voice is not configured");
  return { accountSid, authorization: `Basic ${btoa(`${user}:${secret}`)}` };
}

async function requestCall(env: TwilioVoiceEnv, sid?: string, body?: URLSearchParams): Promise<TwilioVoiceCall> {
  if (sid !== undefined && !SID.test(sid)) throw invalid();
  const { accountSid, authorization } = credentials(env);
  // One attempt only: a timed-out POST may already have created or ended a call.
  try {
    const response = await fetch(`https://api.twilio.com/2010-04-01/Accounts/${accountSid}/Calls${sid ? `/${sid}` : ""}.json`, {
      method: body ? "POST" : "GET", redirect: "error", signal: AbortSignal.timeout(15_000),
      headers: { Authorization: authorization, ...(body ? { "Content-Type": "application/x-www-form-urlencoded" } : {}) },
      body: body?.toString(),
    });
    if (!response.ok) { await response.body?.cancel(); throw new Error(); }
    const reader = response.body?.getReader();
    if (!reader) throw new Error();
    const chunks: Uint8Array[] = [];
    let length = 0;
    try {
      for (;;) {
        const next = await reader.read();
        if (next.done) break;
        length += next.value.byteLength;
        if (length > 64 * 1024) { await reader.cancel(); throw new Error(); }
        chunks.push(next.value);
      }
    } finally { reader.releaseLock(); }
    const bytes = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
    const result: unknown = JSON.parse(new TextDecoder().decode(bytes));
    if (!result || typeof result !== "object") throw new Error();
    const call = result as Record<string, unknown>;
    if (typeof call.sid !== "string" || !SID.test(call.sid) || (sid && call.sid !== sid) || typeof call.status !== "string" || !statuses.has(call.status)) throw new Error();
    return { sid: call.sid, status: call.status as TwilioCallStatus };
  } catch {
    // Never propagate response bodies, phone numbers, credentials, or fetch errors.
    throw new Error("Twilio voice request failed; outcome may be unknown");
  }
}

export async function createTwilioVoiceCall(env: TwilioVoiceEnv, input: CreateTwilioVoiceCallInput): Promise<TwilioVoiceCall> {
  if (!isE164PhoneNumber(input.to) || !isE164PhoneNumber(env.TWILIO_VOICE_FROM_NUMBER)) throw invalid();
  const maxDurationSeconds = input.maxDurationSeconds ?? 180;
  if (!Number.isInteger(maxDurationSeconds) || maxDurationSeconds < 30 || maxDurationSeconds > 600) throw invalid();
  validateUrl(input.statusCallbackUrl, "https:");
  const body = new URLSearchParams({ To: input.to, From: env.TWILIO_VOICE_FROM_NUMBER,
    Twiml: buildTwilioVoiceTwiml(input.streamUrl, input.callId), StatusCallback: input.statusCallbackUrl, StatusCallbackMethod: "POST",
    TimeLimit: String(maxDurationSeconds), Timeout: "30" });
  for (const event of ["initiated", "ringing", "answered", "completed"]) body.append("StatusCallbackEvent", event);
  return requestCall(env, undefined, body);
}
export function fetchTwilioVoiceCall(env: TwilioVoiceEnv, sid: string): Promise<TwilioVoiceCall> {
  return requestCall(env, sid);
}
export function hangupTwilioVoiceCall(env: TwilioVoiceEnv, sid: string): Promise<TwilioVoiceCall> {
  return requestCall(env, sid, new URLSearchParams({ Status: "completed" }));
}

/** Form webhooks or WSS handshakes (empty fields). Pass the exact configured public URL.
 * Preserve scheme, query and trailing slash; never guess variants from request headers.
 * Twilio documents a trailing slash caveat for WSS; configure it explicitly in the stream URL.
 */
export async function verifyTwilioWebhookSignature(env: Pick<TwilioVoiceEnv, "TWILIO_AUTH_TOKEN">, publicUrl: string, fields: URLSearchParams, signature: string | null): Promise<boolean> {
  try {
    if (!bounded(env.TWILIO_AUTH_TOKEN, 256) || !signature || !/^[A-Za-z0-9+/]{27}=$/.test(signature)) return false;
    validateUrl(publicUrl, publicUrl.startsWith("wss:") ? "wss:" : "https:");
    const grouped = new Map<string, Set<string>>();
    let total = publicUrl.length;
    let count = 0;
    for (const [name, value] of fields) {
      total += name.length + value.length;
      if (++count > 128 || total > 64 * 1024 || name.length > 256 || !name) return false;
      if (!grouped.has(name)) grouped.set(name, new Set());
      grouped.get(name)!.add(value);
    }
    // Match Twilio's SDK: sorted names, sorted unique values for repeated fields.
    let payload = publicUrl;
    for (const name of [...grouped.keys()].sort()) for (const value of [...grouped.get(name)!].sort()) payload += name + value;
    const encoder = new TextEncoder();
    const key = await crypto.subtle.importKey("raw", encoder.encode(env.TWILIO_AUTH_TOKEN), { name: "HMAC", hash: "SHA-1" }, false, ["sign"]);
    const expected = new Uint8Array(await crypto.subtle.sign("HMAC", key, encoder.encode(payload)));
    const supplied = Uint8Array.from(atob(signature), (value) => value.charCodeAt(0));
    return supplied.length === expected.length && timingSafeEqual(expected, supplied);
  } catch { return false; }
}
