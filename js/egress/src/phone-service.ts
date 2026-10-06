import { CredentialVault, type CredentialVaultEnv, type EncryptedEnvelope, type SecretsStoreSecret } from "./credential-vault";

export interface PhoneServiceEnv extends CredentialVaultEnv {
  PHONE_SERVICE_ACCOUNTS?: DurableObjectNamespace;
  TWILIO_PHONE_PROVIDER?: Fetcher;
  PHONE_PROVISIONING_ENABLED?: string;
  PHONE_MAX_NUMBERS_PER_OWNER?: string;
  PHONE_MAX_MONTHLY_PRICE?: string;
  PHONE_MAX_INBOUND_SMS_PRICE?: string;
  PHONE_MESSAGE_TTL_SECONDS?: string;
  PHONE_MAX_MESSAGE_RECEIPTS?: string;
  TWILIO_ACCOUNT_SID?: string;
  TWILIO_AUTH_TOKEN?: string | SecretsStoreSecret;
  PHONE_WEBHOOK_URL?: string;
}
type Config = { sid: string; token: string; webhook: string; broker?: Fetcher };
type NumberRecord = { id: string; phone_number: string; country: "US"; status: "active" | "release_pending" | "released"; created_at: string; provider_sid: string };
type Quote = { id: string; currency: string; monthly_price: string; inbound_sms_price: string; recurring: true; expires_at: string };
type Operation = { operation_id: string; kind: "purchase" | "release"; status: "pending_approval" | "complete" | "denied" | "expired" | "failed" | "outcome_unknown"; phone_number: string; number_id?: string; quote?: Quote; created_at: string; error?: string };
type Slot = { owner: string; operation_id: string; number_id: string; active: boolean; retired?: boolean };
type Message = { id: string; from: string; to: string; body: string; received_at: string; expires_at: string };
type StoredMessage = { envelope: EncryptedEnvelope; expires: number; key: string };
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const E164 = /^\+[1-9][0-9]{7,14}$/;
const QUOTE_TTL = 10 * 60 * 1000;
const MAX_MESSAGES = 200;
const MAX_OPERATIONS = 1000; // Fences never expire: at capacity, fail closed rather than forget an old purchase.
function json(value: unknown, status = 200): Response { return Response.json(value, { status, headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" } }); }
class Failure extends Error { constructor(readonly code: string, readonly status = 400) { super(code); } }
function requireValue(condition: unknown, code = "invalid_request", status = 400): asserts condition { if (!condition) throw new Failure(code, status); }
async function boundedText(request: Request | Response, max = 16384): Promise<string> {
  const reader = request.body?.getReader(); if (!reader) return "";
  const chunks: Uint8Array[] = []; let size = 0;
  try { for (;;) { const part = await reader.read(); if (part.done) break; size += part.value.length; if (size > max) { await reader.cancel(); throw new Failure("body_too_large", 413); } chunks.push(part.value); } }
  finally { reader.releaseLock(); }
  const result = new Uint8Array(size); let offset = 0; for (const chunk of chunks) { result.set(chunk, offset); offset += chunk.length; }
  return new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(result);
}
async function body(request: Request, fields: string[]): Promise<Record<string, unknown>> {
  requireValue(request.headers.get("content-type")?.split(";")[0]?.trim() === "application/json");
  let result: unknown; try { result = JSON.parse(await boundedText(request)); } catch { throw new Failure("invalid_request"); }
  requireValue(result && typeof result === "object" && !Array.isArray(result));
  requireValue(Object.keys(result).every(key => fields.includes(key)));
  return result as Record<string, unknown>;
}
async function config(env: PhoneServiceEnv): Promise<Config> {
  requireValue(env.PHONE_SERVICE_ACCOUNTS && (env.TWILIO_PHONE_PROVIDER || (/^AC[0-9a-f]{32}$/i.test(env.TWILIO_ACCOUNT_SID ?? "") && env.TWILIO_AUTH_TOKEN)) && env.PHONE_WEBHOOK_URL && env.CREDENTIAL_ENCRYPTION_KEY, "phone_not_configured", 503);
  let url: URL; try { url = new URL(env.PHONE_WEBHOOK_URL); } catch { throw new Failure("phone_not_configured", 503); }
  requireValue(url.protocol === "https:" && !url.username && !url.password && !url.search && !url.hash && url.pathname === "/v1/services/phone/webhook", "phone_not_configured", 503);
  if (env.TWILIO_PHONE_PROVIDER) {
    const response = await env.TWILIO_PHONE_PROVIDER.fetch("https://phone-provider.internal/status");
    requireValue(response.ok, "phone_not_configured", 503);
    const status = await response.json() as {configured?:boolean;account_sid?:string;auth_token_available?:boolean};
    requireValue(status.configured && status.auth_token_available && /^AC[0-9a-f]{32}$/i.test(status.account_sid ?? ""), "phone_not_configured", 503);
    return { sid: status.account_sid!, token: "", webhook: url.href, broker: env.TWILIO_PHONE_PROVIDER };
  }
  const token = typeof env.TWILIO_AUTH_TOKEN === "string" ? env.TWILIO_AUTH_TOKEN : await env.TWILIO_AUTH_TOKEN!.get();
  requireValue(/^[A-Za-z0-9_-]{16,256}$/.test(token), "phone_not_configured", 503);
  return { sid: env.TWILIO_ACCOUNT_SID!, token, webhook: url.href };
}
function account(env: PhoneServiceEnv, owner: string): DurableObjectStub { return env.PHONE_SERVICE_ACCOUNTS!.get(env.PHONE_SERVICE_ACCOUNTS!.idFromName(`phone-account:${owner}`)); }
function directory(env: PhoneServiceEnv, phone: string): DurableObjectStub { return env.PHONE_SERVICE_ACCOUNTS!.get(env.PHONE_SERVICE_ACCOUNTS!.idFromName(`phone-number:${phone}`)); }
async function registry(env: PhoneServiceEnv, phone: string, action: string, data: unknown): Promise<Response> { return directory(env, phone).fetch(new Request(`https://phone.internal/_registry/${action}`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(data) })); }
async function provider(c: Config, path: string, method = "GET", form?: URLSearchParams): Promise<Response> {
  if (c.broker) return c.broker.fetch(new Request("https://phone-provider.internal/request", {
    method: "POST", headers: { "content-type": "application/json" }, signal: AbortSignal.timeout(8000),
    body: JSON.stringify({ path: path.replace(c.sid, "{account}"), method, ...(form ? {body: form.toString()} : {}) }),
  }));
  const origin = path.startsWith("/v1/") ? "https://pricing.twilio.com" : "https://api.twilio.com";
  return fetch(origin + path, { method, headers: { authorization: `Basic ${btoa(`${c.sid}:${c.token}`)}`, ...(form ? { "content-type": "application/x-www-form-urlencoded" } : {}) }, ...(form ? { body: form } : {}), redirect: "manual", signal: AbortSignal.timeout(8000) });
}
async function providerJson(c: Config, path: string): Promise<Record<string, any>> {
  const response = await provider(c, path); requireValue(response.ok, "provider_unavailable", 503);
  try { return JSON.parse(await boundedText(response, 256 * 1024)); } catch { throw new Failure("provider_unavailable", 503); }
}
const base = (c: Config) => `/2010-04-01/Accounts/${c.sid}`;
async function available(c: Config, area: string | null, limit: number, exact?: string): Promise<{phone_number: string; country: "US"; type: "local"}[]> {
  const query = new URLSearchParams({ SmsEnabled: "true", ExcludeAllAddressRequired: "true", PageSize: String(limit) });
  if (area) query.set("AreaCode", area); if (exact) query.set("Contains", exact);
  const result = await providerJson(c, `${base(c)}/AvailablePhoneNumbers/US/Local.json?${query}`);
  requireValue(Array.isArray(result.available_phone_numbers), "provider_unavailable", 503);
  return result.available_phone_numbers.filter((n: any) => typeof n.phone_number === "string" && E164.test(n.phone_number) && n.iso_country === "US" && n.capabilities?.SMS === true && (!exact || n.phone_number === exact)).slice(0, limit).map((n: any) => ({ phone_number: n.phone_number, country: "US", type: "local" }));
}
async function prices(c: Config): Promise<Pick<Quote, "currency" | "monthly_price" | "inbound_sms_price">> {
  const [numbers, sms] = await Promise.all([providerJson(c, "/v1/PhoneNumbers/Countries/US"), providerJson(c, "/v1/Messaging/Countries/US")]);
  const monthly = numbers.phone_number_prices?.find((p: any) => p.number_type === "local")?.current_price;
  const inbound = sms.inbound_sms_prices?.find((p: any) => p.number_type === "local" || p.number_type === "longcode")?.current_price;
  requireValue(typeof monthly === "string" && /^\d+(\.\d{1,8})?$/.test(monthly) && typeof inbound === "string" && /^\d+(\.\d{1,8})?$/.test(inbound) && typeof numbers.price_unit === "string" && /^[a-z]{3}$/i.test(numbers.price_unit) && numbers.price_unit.toLowerCase() === String(sms.price_unit).toLowerCase(), "pricing_unavailable", 503);
  return { currency: numbers.price_unit.toLowerCase(), monthly_price: monthly, inbound_sms_price: inbound };
}
async function signedWebhook(request: Request, c: Config): Promise<URLSearchParams> {
  requireValue(request.method === "POST" && request.headers.get("content-type")?.split(";")[0]?.trim() === "application/x-www-form-urlencoded", "invalid_webhook", 400);
  requireValue(!new URL(request.url).search, "invalid_webhook", 400);
  const rawBody = await boundedText(request);
  const form = new URLSearchParams(rawBody);
  const names = [...form.keys()].sort(); requireValue(names.length <= 100 && new Set(names).size === names.length, "invalid_webhook", 400);
  const signature = request.headers.get("x-twilio-signature") ?? "";
  requireValue(/^[A-Za-z0-9+/]{27}=$/.test(signature), "invalid_signature", 403);
  let valid = false;
  if (c.broker) {
    const response = await c.broker.fetch(new Request("https://phone-provider.internal/verify", {method:"POST", headers:{"content-type":"application/json"}, body:JSON.stringify({url:c.webhook,signature,body:rawBody})}));
    valid = response.ok && (await response.json() as {valid?:boolean}).valid === true;
  } else {
    const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(c.token), { name: "HMAC", hash: "SHA-1" }, false, ["verify"]);
    valid = await crypto.subtle.verify("HMAC", key, Uint8Array.from(atob(signature), char => char.charCodeAt(0)), new TextEncoder().encode(c.webhook + names.map(name => name + form.get(name)).join("")));
  }
  requireValue(valid, "invalid_signature", 403);
  requireValue(form.get("AccountSid") === c.sid && E164.test(form.get("To") ?? "") && /^(SM|MM)[0-9a-f]{32}$/i.test(form.get("MessageSid") ?? "") && (form.get("Body")?.length ?? 0) <= 4096 && (form.get("From")?.length ?? 0) <= 64 && form.get("NumMedia") === "0", "invalid_webhook", 400);
  return form;
}

/** Private binding only. The account gateway supplies owner; public headers never select a tenant. */
export async function handlePhoneService(request: Request, env: PhoneServiceEnv, owner?: string): Promise<Response> {
  try {
    const c = await config(env);
    if (new URL(request.url).pathname === "/v1/phone/webhook") {
      const copy = request.clone(); const form = await signedWebhook(copy, c);
      const resolved = await registry(env, form.get("To")!, "resolve", {});
      requireValue(resolved.ok, "unknown_destination", 404);
      const slot = await resolved.json() as Slot;
      const headers = new Headers(request.headers); headers.set("x-phone-owner", slot.owner); headers.set("x-phone-number-id", slot.number_id);
      return account(env, slot.owner).fetch(new Request("https://phone.internal/_inbound", { method: "POST", headers, body: request.body }));
    }
    requireValue(owner && owner.length <= 128 && !/[\u0000-\u0020]/.test(owner), "unauthorized", 401);
    const url = new URL(request.url); requireValue(url.pathname.startsWith("/v1/phone/"), "not_found", 404);
    const headers = new Headers(request.headers); headers.set("x-phone-owner", owner);
    return account(env, owner).fetch(new Request(request, { headers }));
  } catch (error) { return json({ error: error instanceof Failure ? error.code : "phone_service_unavailable" }, error instanceof Failure ? error.status : 503); }
}

export class PhoneServiceAccount {
  constructor(private readonly state: DurableObjectState, private readonly env: PhoneServiceEnv) {}
  async fetch(request: Request): Promise<Response> {
    // Serializes provider operations and directory transitions even when fetch yields. Durable writes precede side effects.
    return this.state.blockConcurrencyWhile(async () => {
      try {
        const url = new URL(request.url);
        if (url.pathname.startsWith("/_registry/")) return await this.registry(request, url.pathname.slice(11));
        const c = await config(this.env);
        const owner = request.headers.get("x-phone-owner"); requireValue(owner, "unauthorized", 401);
        const savedOwner = await this.state.storage.get<string>("owner"); requireValue(!savedOwner || savedOwner === owner, "forbidden", 403);
        if (!savedOwner) await this.state.storage.put("owner", owner);
        await this.prune();
        if (url.pathname === "/_inbound") return await this.inbound(request, c, owner);
        await this.rate("api", 120);
        const path = url.pathname.slice("/v1/phone".length);
        if (path === "/numbers/available" && request.method === "GET") {
          requireValue([...url.searchParams.keys()].every(k => ["country", "area_code", "limit"].includes(k)) && (url.searchParams.get("country") ?? "US") === "US");
          const area = url.searchParams.get("area_code"); requireValue(!area || /^[2-9][0-9]{2}$/.test(area));
          await this.rate("provider", 20);
          return json({ numbers: await available(c, area, this.limit(url, 10, 20)) });
        }
        if (path === "/numbers" && request.method === "GET") { requireValue(!url.search); return json({ numbers: [...(await this.state.storage.list<NumberRecord>({ prefix: "number:", limit: 101 })).values()].map(publicNumber) }); }
        if (path === "/numbers" && request.method === "POST") { requireValue(!url.search); return await this.purchaseIntent(request, c); }
        const numberMatch = /^\/numbers\/([0-9a-f-]+)(\/messages)?$/.exec(path);
        if (numberMatch && UUID.test(numberMatch[1]!)) {
          const number = await this.state.storage.get<NumberRecord>(`number:${numberMatch[1]}`); requireValue(number, "not_found", 404);
          if (numberMatch[2] && request.method === "GET") return await this.messages(number, url, owner);
          requireValue(!url.search);
          if (!numberMatch[2] && request.method === "GET") return json({ number: publicNumber(number) });
          if (!numberMatch[2] && request.method === "DELETE") return await this.releaseIntent(request, number);
        }
        const operationMatch = /^\/requests\/([0-9a-f-]+)(?:\/(approve|deny))?$/.exec(path);
        if (operationMatch && UUID.test(operationMatch[1]!)) {
          requireValue(!url.search); const operation = await this.state.storage.get<Operation>(`op:${operationMatch[1]}`); requireValue(operation, "not_found", 404);
          if (!operationMatch[2] && request.method === "GET") return json({ request: await this.reconcile(c, owner, operation) });
          if (operationMatch[2] && request.method === "POST") {
            requireValue(request.headers.get("x-nanocodex-phone-human-approval") === "true", "human_approval_required", 403);
            return await this.decision(request, c, owner, operation, operationMatch[2]);
          }
        }
        throw new Failure("not_found", 404);
      } catch (error) { return json({ error: error instanceof Failure ? error.code : "phone_service_unavailable" }, error instanceof Failure ? error.status : 503); }
    });
  }
  private async registry(request: Request, action: string): Promise<Response> {
    const input = await body(request, ["owner", "operation_id", "number_id"]);
    const slot = await this.state.storage.get<Slot>("slot");
    if (action === "resolve") return slot?.active ? json(slot) : json({ error: "not_found" }, 404);
    requireValue(typeof input.owner === "string" && typeof input.operation_id === "string" && typeof input.number_id === "string");
    if (action === "reserve") {
      requireValue(!slot || (!slot.retired && slot.owner === input.owner && slot.operation_id === input.operation_id && slot.number_id === input.number_id), "number_reserved", 409);
      if (!slot) await this.state.storage.put("slot", { ...input, active: false });
    } else {
      requireValue(slot && slot.owner === input.owner && slot.number_id === input.number_id, "number_reserved", 409);
      if (action === "activate") { requireValue(slot.operation_id === input.operation_id, "number_reserved", 409); await this.state.storage.put("slot", { ...slot, active: true }); }
      else if (action === "remove") await this.state.storage.put("slot", { ...slot, active: false, retired: true });
      else if (action === "cancel") { requireValue(!slot.active && slot.operation_id === input.operation_id, "number_reserved", 409); await this.state.storage.delete("slot"); }
      else throw new Failure("not_found", 404);
    }
    return json({ ok: true });
  }
  private async rate(key: string, max: number): Promise<void> {
    const minute = Math.floor(Date.now() / 60000); const old = await this.state.storage.get<{minute:number;count:number}>(`rate:${key}`);
    const count = old?.minute === minute ? old.count + 1 : 1; requireValue(count <= max, "rate_limited", 429);
    await this.state.storage.put(`rate:${key}`, { minute, count });
  }
  private limit(url: URL, fallback: number, maximum: number): number { const value = url.searchParams.get("limit"); requireValue(value === null || /^\d{1,2}$/.test(value)); const n = value === null ? fallback : Number(value); requireValue(n >= 1 && n <= maximum); return n; }
  private async operationCapacity(): Promise<void> { const count = await this.state.storage.get<number>("operation_count") ?? 0; requireValue(count < MAX_OPERATIONS, "operation_capacity_reached", 409); }
  private async saveNew(op: Operation): Promise<Response> { const count = await this.state.storage.get<number>("operation_count") ?? 0; await this.state.storage.put({ [`op:${op.operation_id}`]: op, operation_count: count + 1 }); return json({ request: op }, 202); }
  private provisioning(prices?: Pick<Quote,"currency"|"monthly_price"|"inbound_sms_price">): number {
    const cap = Number(this.env.PHONE_MAX_NUMBERS_PER_OWNER ?? "3");
    const monthly = Number(this.env.PHONE_MAX_MONTHLY_PRICE), inbound = Number(this.env.PHONE_MAX_INBOUND_SMS_PRICE);
    requireValue(this.env.PHONE_PROVISIONING_ENABLED === "true" && Number.isInteger(cap) && cap >= 1 && cap <= 10 && Number.isFinite(monthly) && monthly > 0 && Number.isFinite(inbound) && inbound > 0, "provisioning_disabled", 503);
    if (prices) requireValue(prices.currency === "usd" && Number(prices.monthly_price) <= monthly && Number(prices.inbound_sms_price) <= inbound, "operator_price_limit", 409);
    return cap;
  }
  private async purchaseIntent(request: Request, c: Config): Promise<Response> {
    const input = await body(request, ["operation_id", "phone_number", "country"]);
    requireValue(typeof input.operation_id === "string" && UUID.test(input.operation_id) && typeof input.phone_number === "string" && /^\+1[2-9][0-9]{9}$/.test(input.phone_number) && input.country === "US");
    const previous = await this.state.storage.get<Operation>(`op:${input.operation_id}`);
    if (previous) { requireValue(previous.kind === "purchase" && previous.phone_number === input.phone_number, "operation_conflict", 409); return json({ request: previous }); }
    this.provisioning(); await this.operationCapacity(); await this.rate("provider", 20);
    requireValue((await this.state.storage.list({ prefix: "number:", limit: 101 })).size < 100, "number_capacity_reached", 409);
    requireValue((await available(c, null, 20, input.phone_number)).some(n => n.phone_number === input.phone_number), "number_unavailable", 409);
    const price = await prices(c); this.provisioning(price);
    const quote: Quote = { id: crypto.randomUUID(), ...price, recurring: true, expires_at: new Date(Date.now() + QUOTE_TTL).toISOString() };
    return this.saveNew({ operation_id: input.operation_id, kind: "purchase", status: "pending_approval", phone_number: input.phone_number, quote, created_at: new Date().toISOString() });
  }
  private async releaseIntent(request: Request, number: NumberRecord): Promise<Response> {
    const input = await body(request, ["operation_id"]); requireValue(typeof input.operation_id === "string" && UUID.test(input.operation_id));
    const previous = await this.state.storage.get<Operation>(`op:${input.operation_id}`);
    if (previous) { requireValue(previous.kind === "release" && previous.number_id === number.id, "operation_conflict", 409); return json({ request: previous }); }
    requireValue(number.status === "active", "number_not_active", 409); await this.operationCapacity();
    return this.saveNew({ operation_id: input.operation_id, kind: "release", status: "pending_approval", phone_number: number.phone_number, number_id: number.id, created_at: new Date().toISOString() });
  }
  private async decision(request: Request, c: Config, owner: string, op: Operation, decision: string): Promise<Response> {
    const input = await body(request, decision === "deny" ? [] : op.kind === "purchase" ? ["quote_id", "accept_recurring"] : ["confirm_release"]);
    if (decision === "approve") requireValue(op.kind === "purchase" ? input.quote_id === op.quote?.id && input.accept_recurring === true : input.confirm_release === true, "approval_mismatch", 409);
    if (op.status !== "pending_approval") return json({ request: op });
    if (decision === "deny") { op.status = "denied"; await this.state.storage.put(`op:${op.operation_id}`, op); return json({ request: op }); }
    if (op.kind === "purchase") {
      if (Date.parse(op.quote!.expires_at) <= Date.now()) { op.status = "expired"; await this.state.storage.put(`op:${op.operation_id}`, op); return json({ request: op }, 409); }
      const cap = this.provisioning();
      const active = [...(await this.state.storage.list<NumberRecord>({prefix:"number:",limit:101})).values()].filter(n => n.status !== "released").length;
      const ambiguous = [...(await this.state.storage.list<Operation>({prefix:"op:",limit:MAX_OPERATIONS})).values()].filter(o => o.kind === "purchase" && o.status === "outcome_unknown").length;
      requireValue(active + ambiguous < cap, "number_limit_reached", 409);
      await this.rate("provider", 20); const current = await prices(c); this.provisioning(current);
      if (current.currency !== op.quote!.currency || current.monthly_price !== op.quote!.monthly_price || current.inbound_sms_price !== op.quote!.inbound_sms_price) { op.status = "expired"; op.error = "price_changed"; await this.state.storage.put(`op:${op.operation_id}`, op); return json({ request: op }, 409); }
      requireValue((await available(c, null, 20, op.phone_number)).some(n => n.phone_number === op.phone_number), "number_unavailable", 409);
      const id = crypto.randomUUID();
      const reserved = await registry(this.env, op.phone_number, "reserve", { owner, operation_id: op.operation_id, number_id: id }); requireValue(reserved.ok, "number_reserved", 409);
      op.number_id = id; op.status = "outcome_unknown"; await this.state.storage.put(`op:${op.operation_id}`, op);
      try {
        const response = await provider(c, `${base(c)}/IncomingPhoneNumbers.json`, "POST", new URLSearchParams({ PhoneNumber: op.phone_number, SmsUrl: c.webhook, SmsMethod: "POST", FriendlyName: `Nanocodex ${id}` }));
        // Only an unambiguous provider rejection permits clearing the reservation; never retry this operation.
        if (response.status >= 400 && response.status < 500 && response.status !== 408 && response.status !== 429) {
          op.status = "failed"; op.error = "provider_rejected"; await this.state.storage.put(`op:${op.operation_id}`, op);
          await registry(this.env, op.phone_number, "cancel", { owner, operation_id: op.operation_id, number_id: id });
          return json({ request: op }, 409);
        }
        requireValue(response.status === 201, "provider_outcome_unknown", 502);
        const value = JSON.parse(await boundedText(response));
        requireValue(/^PN[0-9a-f]{32}$/i.test(value.sid ?? "") && value.phone_number === op.phone_number && value.account_sid === c.sid && value.sms_url === c.webhook && value.sms_method === "POST", "provider_outcome_unknown", 502);
        const number: NumberRecord = { id, phone_number: op.phone_number, country: "US", status: "active", created_at: new Date().toISOString(), provider_sid: value.sid };
        await this.state.storage.put(`number:${id}`, number);
        const activated = await registry(this.env, op.phone_number, "activate", { owner, operation_id: op.operation_id, number_id: id }); requireValue(activated.ok, "provider_outcome_unknown", 502);
        op.status = "complete"; await this.state.storage.put(`op:${op.operation_id}`, op);
      } catch { return json({ request: { ...op, status: "outcome_unknown" } }, 202); }
    } else {
      const number = await this.state.storage.get<NumberRecord>(`number:${op.number_id}`); requireValue(number?.status === "active", "number_not_active", 409);
      op.status = "outcome_unknown"; number.status = "release_pending";
      await this.state.storage.put({ [`op:${op.operation_id}`]: op, [`number:${number.id}`]: number });
      try {
        const response = await provider(c, `${base(c)}/IncomingPhoneNumbers/${number.provider_sid}.json`, "DELETE");
        requireValue(response.status === 204, "provider_outcome_unknown", 502);
        number.status = "released"; await this.state.storage.put(`number:${number.id}`, number);
        await registry(this.env, number.phone_number, "remove", { owner, operation_id: op.operation_id, number_id: number.id });
        await this.clearMessages(number.id);
        op.status = "complete"; await this.state.storage.put(`op:${op.operation_id}`, op);
      } catch { return json({ request: op }, 202); }
    }
    return json({ request: op });
  }
  private async reconcile(c: Config, owner: string, op: Operation): Promise<Operation> {
    if (op.status !== "outcome_unknown" || !op.number_id) return op;
    try {
      await this.rate("provider", 20);
      if (op.kind === "purchase") {
        const query = new URLSearchParams({ PhoneNumber: op.phone_number, FriendlyName: `Nanocodex ${op.number_id}`, PageSize: "20" });
        const data = await providerJson(c, `${base(c)}/IncomingPhoneNumbers.json?${query}`);
        if (!Array.isArray(data.incoming_phone_numbers)) return op;
        const matches = data.incoming_phone_numbers.filter((value: any) => /^PN[0-9a-f]{32}$/i.test(value.sid ?? "") && value.phone_number === op.phone_number && value.account_sid === c.sid && value.friendly_name === `Nanocodex ${op.number_id}` && value.sms_url === c.webhook && value.sms_method === "POST");
        if (matches.length !== 1) return op;
        const number: NumberRecord = { id: op.number_id, phone_number: op.phone_number, country: "US", status: "active", created_at: op.created_at, provider_sid: matches[0].sid };
        await this.state.storage.put(`number:${number.id}`, number);
        const activated = await registry(this.env, op.phone_number, "activate", { owner, operation_id: op.operation_id, number_id: number.id });
        if (!activated.ok) return op;
      } else {
        const number = await this.state.storage.get<NumberRecord>(`number:${op.number_id}`);
        if (!number || number.status === "active") return op;
        const response = await provider(c, `${base(c)}/IncomingPhoneNumbers/${number.provider_sid}.json`);
        // 20404 is Twilio's documented missing-resource receipt, distinct from gateway/authentication failures.
        if (response.status !== 404 || (JSON.parse(await boundedText(response)) as {code?:number}).code !== 20404) return op;
        number.status = "released"; await this.state.storage.put(`number:${number.id}`, number);
        const removed = await registry(this.env, op.phone_number, "remove", { owner, operation_id: op.operation_id, number_id: number.id });
        if (!removed.ok) return op;
        await this.clearMessages(number.id);
      }
      const complete: Operation = { ...op, status: "complete" };
      await this.state.storage.put(`op:${op.operation_id}`, complete);
      return complete;
    } catch { return op; }
  }
  private async inbound(request: Request, c: Config, owner: string): Promise<Response> {
    const form = await signedWebhook(request, c); const id = request.headers.get("x-phone-number-id")!;
    const number = await this.state.storage.get<NumberRecord>(`number:${id}`);
    requireValue(number?.status === "active" && number.phone_number === form.get("To"), "unknown_destination", 404);
    const sid = form.get("MessageSid")!;
    if (await this.state.storage.get(`seen:${sid}`)) return twiml();
    await this.rate("inbound", 60);
    const now = Date.now(); const ttl = Number(this.env.PHONE_MESSAGE_TTL_SECONDS ?? "86400");
    requireValue(Number.isInteger(ttl) && ttl >= 1 && ttl <= 86400, "phone_not_configured", 503);
    const expires = now + ttl * 1000; const message: Message = { id: sid, from: form.get("From") ?? "", to: number.phone_number, body: form.get("Body") ?? "", received_at: new Date(now).toISOString(), expires_at: new Date(expires).toISOString() };
    const key = `message:${id}:${String(now).padStart(13, "0")}:${sid}`;
    const envelope = await new CredentialVault(this.env, `phone:${owner}`).seal(message);
    // Permanent SID-only tombstones prevent replay even after plaintext expiration; fail closed at a bounded lifetime capacity.
    const seenCount = await this.state.storage.get<number>("seen_count") ?? 0;
    const receiptCap = Number(this.env.PHONE_MAX_MESSAGE_RECEIPTS ?? "10000");
    requireValue(Number.isInteger(receiptCap) && receiptCap >= 1 && receiptCap <= 10000, "phone_not_configured", 503);
    requireValue(seenCount < receiptCap, "inbox_capacity_reached", 429);
    await this.state.storage.put({ [key]: { envelope, expires, key }, [`seen:${sid}`]: true, seen_count: seenCount + 1 });
    const messages = await this.state.storage.list<StoredMessage>({ prefix: "message:" });
    if (messages.size > MAX_MESSAGES) {
      const oldest = [...messages.entries()].sort((a,b) => a[1].expires - b[1].expires).slice(0, messages.size - MAX_MESSAGES);
      await this.state.storage.delete(oldest.map(([key]) => key));
    }
    if (!(await this.state.storage.getAlarm())) await this.state.storage.setAlarm(now + 60000);
    return twiml();
  }
  private async messages(number: NumberRecord, url: URL, owner: string): Promise<Response> {
    requireValue([...url.searchParams.keys()].every(k => ["limit", "cursor"].includes(k)));
    const limit = this.limit(url, 20, 50); const cursor = url.searchParams.get("cursor");
    let startAfter: string | undefined;
    if (cursor) { try { startAfter = atob(cursor); } catch { throw new Failure("invalid_cursor"); } requireValue(startAfter.startsWith(`message:${number.id}:`) && startAfter.length < 150, "invalid_cursor"); }
    const entries = await this.state.storage.list<StoredMessage>({ prefix: `message:${number.id}:`, limit: limit + 1, ...(startAfter ? { startAfter } : {}) });
    const page = [...entries.entries()].slice(0, limit); const vault = new CredentialVault(this.env, `phone:${owner}`); const messages: Message[] = [];
    for (const [, value] of page) { if (value.expires > Date.now()) messages.push((await vault.open<Message>(value.envelope)).value); }
    return json({ messages, next_cursor: entries.size > limit ? btoa(page[page.length - 1]![0]) : null });
  }
  private async clearMessages(id: string): Promise<void> {
    const keys = [...(await this.state.storage.list({ prefix: `message:${id}:`, limit: MAX_MESSAGES + 1 })).keys()];
    for (let i = 0; i < keys.length; i += 128) await this.state.storage.delete(keys.slice(i, i + 128));
  }
  private async prune(): Promise<void> {
    const now = Date.now(); const expired: string[] = [];
    for (const [key, value] of await this.state.storage.list<StoredMessage>({ prefix: "message:", limit: MAX_MESSAGES + 1 })) if (value.expires <= now) expired.push(key);
    for (let i = 0; i < expired.length; i += 128) await this.state.storage.delete(expired.slice(i, i + 128));
  }
  async alarm(): Promise<void> { await this.prune(); if ((await this.state.storage.list({ prefix: "message:", limit: 1 })).size) await this.state.storage.setAlarm(Date.now() + 60000); }
}
function publicNumber(number: NumberRecord) { const { provider_sid: _private, ...result } = number; return result; }
function twiml(): Response { return new Response("<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response></Response>", { headers: { "content-type": "text/xml", "cache-control": "no-store" } }); }
