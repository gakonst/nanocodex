import type { McpGrant } from "./oauthMcp.mts";
import { validateMcpWebhookUrl } from "./mcpEventTransport.mts";

/** All state belongs to one approved MCP principal. No access token is a subscription identity. */
export const MCP_EVENT_NAME = "agent.turn.completed";
const MIN_TTL = 1_000;
const DONE_RETENTION = 24 * 60 * 60_000;
const DEFAULT_TTL = 60 * 60_000;
const MAX_TTL = 24 * 60 * 60_000;
const VERIFY_TTL = 10 * 60_000;
const ROTATE_TTL = 5 * 60_000;
const POLL_MS = 5_000;
const MAX_SUBSCRIPTIONS = 32;
const MAX_TRACKED_TURNS = 4096;
const MAX_OUTBOX = 1024;
const MAX_ATTEMPTS = 5;
const RETRY_WINDOW = 15 * 60_000;
const encoder = new TextEncoder();

type FailureReason = "connection_refused" | "timeout" | "tls_error" | "http_4xx" | "http_5xx" | "challenge_failed";
export class McpEventFailure extends Error {
  constructor(public code: number, message: string, public data?: Record<string, unknown>) { super(message); }
}
/** DurableObjectStorage's transaction surface, also usable by a persistent local harness. */
export interface McpEventTransaction {
  get<T>(key: string): Promise<T | undefined>;
  put<T>(key: string, value: T): Promise<void>;
  delete(key: string): Promise<boolean>;
  list<T>(options: { prefix: string; limit?: number }): Promise<Map<string, T>>;
}
export interface McpEventStorage extends McpEventTransaction {
  transaction<T>(callback: (transaction: McpEventTransaction) => Promise<T>): Promise<T>;
  setAlarm(time: number): Promise<void>;
  deleteAlarm(): Promise<void>;
}
export type McpEventTurnStatus = {
  state: string;
  completedAt?: string;
  output?: string;
};
export type McpEventHooks = {
  /** Re-resolve both the Connect grant and OAuth family; never cache authorization. */
  active(grant: McpGrant): Promise<boolean>;
  /** Read only this grant's approved agent and exact registered turn. Undefined means not yet visible. */
  status(grant: McpGrant, turnId: string): Promise<McpEventTurnStatus | undefined>;
  /** Required protected transport: connection-time address validation/pinning, TLS and no redirects. */
  webhookFetch(request: Request, options?: { readBody?: boolean }): Promise<Response>;
  now?(): number;
};
type Subscription = {
  id: string; principal: string; grant: McpGrant; url: string; arguments: { turn_id?: string };
  secret: string; previousSecret?: string; previousUntil?: number;
  createdAt: number; expiresAt: number;
};
type TrackedTurn = {
  grant: McpGrant; principal: string; turnId: string; registeredAt: number;
  nextPoll: number; done?: boolean; doneAt?: number;
};
type Delivery = {
  subscriptionId: string; eventId: string; body: string;
  attempts: number; createdAt: number; nextAttempt: number;
};
type Verification = { expiresAt: number };
type VerificationRate = { expiresAt: number; count: number };

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function invalid(message: string): never { throw new McpEventFailure(-32602, message); }
function exhausted(limit: string, max: number): never {
  throw new McpEventFailure(-32013, "ResourceExhausted", { limit, max });
}
export function mcpEventsAllowed(grant: McpGrant): boolean {
  return Boolean(grant.agentId) && grant.scope?.split(" ").includes("agent:run") === true && grant.capabilities.includes("chatgpt");
}
export function mcpEventDefinitions(grant: McpGrant): Record<string, unknown>[] {
  if (!mcpEventsAllowed(grant)) return [];
  return [{
    name: MCP_EVENT_NAME,
    description: "An MCP-started turn on the approved Nanocodex agent reaches a terminal state. Optional turn_id limits delivery to that exact turn. Read full output with nanocodex_agent_status. No historical replay.",
    delivery: ["webhook"],
    inputSchema: { type: "object", properties: { turn_id: { type: "string", pattern: "^[A-Za-z0-9_-]{1,128}$" } }, additionalProperties: false },
    payloadSchema: {
      type: "object", properties: {
        agent_id: { type: "string" }, turn_id: { type: "string" },
        status: { type: "string", enum: ["completed", "failed", "cancelled"] },
        completed_at: { type: "string", format: "date-time" },
        output_preview: { type: "string" }, output_truncated: { type: "boolean" },
      }, required: ["agent_id", "turn_id", "status", "completed_at"], additionalProperties: false,
    },
  }];
}
function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (record(value)) return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
  return JSON.stringify(value);
}
async function digest(value: string): Promise<string> {
  return base64(new Uint8Array(await crypto.subtle.digest("SHA-256", encoder.encode(value))))
    .replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}
function base64(bytes: Uint8Array): string { return btoa(String.fromCharCode(...bytes)); }
function principal(grant: McpGrant): string {
  return canonical({ grant_id: grant.id, app_id: grant.appId, app_origin: grant.appOrigin, agent_id: grant.agentId });
}
function secretBytes(value: unknown): Uint8Array<ArrayBuffer> {
  if (typeof value !== "string" || !/^whsec_(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(value)) invalid("delivery.secret must be whsec_ followed by base64 encoding 24–64 bytes.");
  const encoded = value.slice(6);
  let bytes: Uint8Array<ArrayBuffer>;
  try { bytes = Uint8Array.from(atob(encoded), character => character.charCodeAt(0)); } catch { return invalid("Invalid webhook secret."); }
  if (bytes.length < 24 || bytes.length > 64 || base64(bytes) !== encoded) invalid("Invalid webhook secret length or encoding.");
  return bytes;
}
async function signature(secret: string, value: string): Promise<string> {
  const key = await crypto.subtle.importKey("raw", secretBytes(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return `v1,${base64(new Uint8Array(await crypto.subtle.sign("HMAC", key, encoder.encode(value))))}`;
}
/** Compare nonce possession using the platform's constant-time HMAC verifier. */
async function equalChallenge(expected: string, actual: unknown): Promise<boolean> {
  if (typeof actual !== "string" || actual.length > 256) return false;
  const key = await crypto.subtle.importKey("raw", encoder.encode(expected), { name: "HMAC", hash: "SHA-256" }, false, ["sign", "verify"]);
  const signed = await crypto.subtle.sign("HMAC", key, encoder.encode(expected));
  return crypto.subtle.verify("HMAC", key, signed, encoder.encode(actual));
}
function identity(params: Record<string, unknown>, subscribe: boolean): { url: string; arguments: { turn_id?: string } } {
  const allowed = subscribe ? ["name", "arguments", "delivery", "cursor", "ttlMs", "maxAgeMs", "_meta"] : ["name", "arguments", "delivery", "_meta"];
  if (Object.keys(params).some(key => !allowed.includes(key))) invalid("Unknown event parameter.");
  if (params.name !== MCP_EVENT_NAME) throw new McpEventFailure(-32011, "NotFound", { kind: "event" });
  const args = params.arguments === undefined ? {} : params.arguments;
  if (!record(args) || Object.keys(args).some(key => key !== "turn_id")
    || (args.turn_id !== undefined && (typeof args.turn_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(args.turn_id)))) invalid("Invalid event arguments.");
  const delivery = params.delivery;
  if (!record(delivery)) invalid("delivery is required.");
  if (Object.keys(delivery).some(key => !(subscribe ? ["mode", "url", "secret"] : ["mode", "url"]).includes(key))) invalid("Invalid delivery parameters.");
  if (delivery.mode !== "webhook" && (subscribe || delivery.mode !== undefined)) {
    throw new McpEventFailure(-32014, "Unsupported", { feature: "deliveryMode", value: delivery.mode ?? null });
  }
  let url: string;
  try { url = validateMcpWebhookUrl(delivery.url); } catch { return invalid("Callback must be a public HTTPS URL without credentials or fragment."); }
  if (subscribe) {
    secretBytes(delivery.secret);
    if (params.cursor !== undefined && params.cursor !== null && typeof params.cursor !== "string") invalid("cursor must be a string or null.");
    if (params.maxAgeMs !== undefined && (!Number.isSafeInteger(params.maxAgeMs) || Number(params.maxAgeMs) < 0)) invalid("Invalid maxAgeMs.");
    if (params.ttlMs !== undefined && params.ttlMs !== null && !Number.isSafeInteger(params.ttlMs)) invalid("ttlMs must be an integer or null.");
  }
  return { url, arguments: args.turn_id === undefined ? {} : { turn_id: args.turn_id as string } };
}
async function readBounded(response: Response, max: number): Promise<string> {
  const reader = response.body?.getReader();
  if (!reader) return "";
  const chunks: Uint8Array[] = []; let length = 0;
  try {
    for (;;) {
      const part = await reader.read(); if (part.done) break;
      length += part.value.length;
      if (length > max) { await reader.cancel(); throw new Error("Response too large"); }
      chunks.push(part.value);
    }
  } finally { reader.releaseLock(); }
  const result = new Uint8Array(length); let offset = 0;
  for (const chunk of chunks) { result.set(chunk, offset); offset += chunk.length; }
  return new TextDecoder().decode(result);
}
function reason(error: unknown): FailureReason {
  return error instanceof DOMException && (error.name === "TimeoutError" || error.name === "AbortError") ? "timeout" : "connection_refused";
}

/** Durable core. The DO wrapper supplies trusted internal hooks, never a global-fetch fallback.
 * Calls are serialized across network awaits so refresh/unsubscribe cannot race an in-flight alarm.
 * Each turn completion and its entire fan-out commit in one storage transaction.
 */
export class McpEventService {
  private tail: Promise<unknown> = Promise.resolve();
  constructor(private storage: McpEventStorage, private hooks: McpEventHooks) {}
  private now(): number { return this.hooks.now?.() ?? Date.now(); }
  private serial<T>(run: () => Promise<T>): Promise<T> {
    const result = this.tail.then(run, run); this.tail = result.catch(() => {}); return result;
  }
  private async authorized(grant: McpGrant): Promise<void> {
    if (!mcpEventsAllowed(grant) || grant.expiresAt * 1000 <= this.now() || !await this.hooks.active(grant) || grant.expiresAt * 1000 <= this.now()) {
      throw new McpEventFailure(-32012, "Forbidden");
    }
  }
  async list(grant: McpGrant, params: Record<string, unknown> = {}): Promise<{ events: Record<string, unknown>[] }> {
    if (Object.keys(params).some(key => key !== "_meta")) invalid("This event catalog has no additional pages.");
    if (!mcpEventsAllowed(grant)) return { events: [] };
    await this.authorized(grant);
    return { events: mcpEventDefinitions(grant) };
  }
  subscribe(grant: McpGrant, params: Record<string, unknown>): Promise<Record<string, unknown>> {
    return this.serial(async () => {
      await this.authorized(grant);
      const input = identity(params, true), owner = principal(grant);
      const id = `sub_${await digest(canonical([owner, input.url, MCP_EVENT_NAME, input.arguments]))}`;
      // Schedule before state writes: a crash can leave a harmless alarm, never unmonitored accepted state.
      await this.storage.setAlarm(this.now() + 1000);
      await this.cleanExpired();
      const previous = await this.storage.get<Subscription>(`sub:${id}`);
      const subscriptions = await this.storage.list<Subscription>({ prefix: "sub:" });
      if (!previous && subscriptions.size >= MAX_SUBSCRIPTIONS) exhausted("subscriptions", MAX_SUBSCRIPTIONS);
      const secret = (params.delivery as Record<string, unknown>).secret as string;
      const verificationKey = `verified:${await digest(canonical([owner, input.url]))}`;
      const verified = await this.storage.get<Verification>(verificationKey);
      if (!verified || verified.expiresAt <= this.now()) {
        await this.limitVerification(input.url);
        await this.verify(id, input.url, secret);
        await this.storage.put(verificationKey, { expiresAt: this.now() + VERIFY_TTL });
      }
      // The grant can be revoked while the callback is answering its challenge.
      await this.authorized(grant);
      const now = this.now();
      const expiresAt = Math.min(grant.expiresAt * 1000, now + Math.max(MIN_TTL, Math.min(typeof params.ttlMs === "number" ? params.ttlMs : DEFAULT_TTL, MAX_TTL)));
      const rotating = previous && previous.secret !== secret;
      const subscription: Subscription = {
        id, principal: owner, grant, ...input, secret, createdAt: previous?.createdAt ?? now, expiresAt,
        ...(rotating ? { previousSecret: previous.secret, previousUntil: now + ROTATE_TTL }
          : previous?.previousSecret && (previous.previousUntil ?? 0) > now ? { previousSecret: previous.previousSecret, previousUntil: previous.previousUntil } : {}),
      };
      await this.storage.put(`sub:${id}`, subscription);
      await this.schedule();
      return { id, refreshBefore: new Date(expiresAt).toISOString(), cursor: null, truncated: false };
    });
  }
  unsubscribe(grant: McpGrant, params: Record<string, unknown>): Promise<Record<string, never>> {
    return this.serial(async () => {
      await this.authorized(grant);
      const input = identity(params, false);
      const id = `sub_${await digest(canonical([principal(grant), input.url, MCP_EVENT_NAME, input.arguments]))}`;
      await this.removeSubscription(id);
      await this.schedule();
      // ChatGPT requires eager cleanup to be idempotent, including after expiry.
      return {};
    });
  }
  /** Persist before dispatching the idempotent start. A not-yet-visible turn remains monitored. */
  track(grant: McpGrant, turnId: string): Promise<void> {
    return this.serial(async () => {
      await this.authorized(grant);
      if (!/^[A-Za-z0-9_-]{1,128}$/.test(turnId)) invalid("Invalid turn_id.");
      const owner = principal(grant), key = `turn:${await digest(canonical([owner, turnId]))}`;
      const previous = await this.storage.get<TrackedTurn>(key);
      if (previous) return;
      await this.storage.setAlarm(this.now() + 1000);
      await this.cleanExpired();
      const tracked = await this.storage.list<TrackedTurn>({ prefix: "turn:" });
      if (tracked.size >= MAX_TRACKED_TURNS) {
        const oldestDone = [...tracked].filter(([, turn]) => turn.done).sort((a, b) => (a[1].doneAt ?? 0) - (b[1].doneAt ?? 0))[0];
        if (!oldestDone) exhausted("tracked_turns", MAX_TRACKED_TURNS);
        // Stable event IDs still deduplicate a very late start replay after bounded tombstone retention.
        await this.storage.delete(oldestDone[0]);
      }
      await this.storage.put<TrackedTurn>(key, { grant, principal: owner, turnId, registeredAt: this.now(), nextPoll: this.now() + 1000 });
    });
  }
  /** Explicitly rejected dispatches may be removed; uncertain outcomes must remain tracked. */
  forget(grant: McpGrant, turnId: string): Promise<void> {
    return this.serial(async () => {
      const key = `turn:${await digest(canonical([principal(grant), turnId]))}`;
      const turn = await this.storage.get<TrackedTurn>(key);
      if (turn && !turn.done) await this.storage.delete(key);
      await this.schedule();
    });
  }
  alarm(): Promise<void> {
    return this.serial(async () => {
      // Establish recovery before network or storage work. A failed invocation remains retryable.
      await this.storage.setAlarm(this.now() + POLL_MS);
      await this.cleanExpired();
      const subscriptions = await this.storage.list<Subscription>({ prefix: "sub:" });
      const turns = await this.storage.list<TrackedTurn>({ prefix: "turn:" });
      const grants = new Map<string, McpGrant>();
      for (const subscription of subscriptions.values()) grants.set(subscription.principal, subscription.grant);
      for (const turn of turns.values()) if (!turn.done) grants.set(turn.principal, turn.grant);
      // Fail closed on transient authorization lookup errors, but preserve durable state for recovery.
      const authorized = new Set<string>();
      for (const [owner, grant] of grants) {
        let active: boolean;
        try { active = mcpEventsAllowed(grant) && grant.expiresAt * 1000 > this.now() && await this.hooks.active(grant); }
        catch { continue; }
        if (active) authorized.add(owner);
        else {
          for (const subscription of subscriptions.values()) if (subscription.principal === owner) await this.removeSubscription(subscription.id);
          for (const [key, turn] of turns) if (turn.principal === owner) await this.storage.delete(key);
        }
      }
      const due = [...turns].filter(([, turn]) => !turn.done && turn.nextPoll <= this.now() && authorized.has(turn.principal)).sort((a, b) => a[1].nextPoll - b[1].nextPoll).slice(0, 8);
      await Promise.all(due.map(([key, turn]) => this.poll(key, turn)));
      const deliveries = await this.storage.list<Delivery>({ prefix: "out:" });
      await Promise.all([...deliveries].filter(([, delivery]) => delivery.nextAttempt <= this.now())
        .sort((a, b) => a[1].nextAttempt - b[1].nextAttempt).slice(0, 8).map(([key, delivery]) => this.deliver(key, delivery, authorized)));
      await this.schedule();
    });
  }
  private async signedRequest(id: string, url: string, secret: string, body: string, eventId: string, previous?: string): Promise<Request> {
    const timestamp = String(Math.floor(this.now() / 1000));
    const value = `${eventId}.${timestamp}.${body}`;
    const signatures = [await signature(secret, value)];
    if (previous && previous !== secret) signatures.push(await signature(previous, value));
    return new Request(url, { method: "POST", redirect: "manual", signal: AbortSignal.timeout(10_000), headers: {
      "content-type": "application/json", "webhook-id": eventId, "webhook-timestamp": timestamp,
      "webhook-signature": signatures.join(" "), "X-MCP-Subscription-Id": id,
    }, body });
  }
  private async limitVerification(url: string): Promise<void> {
    const key = `verify-rate:${await digest(new URL(url).hostname)}`;
    const previous = await this.storage.get<VerificationRate>(key);
    const current = previous && previous.expiresAt > this.now() ? previous : { expiresAt: this.now() + 60_000, count: 0 };
    if (current.count >= 10) exhausted("callback_verifications_per_host_per_minute", 10);
    if (!previous && (await this.storage.list({ prefix: "verify-rate:" })).size >= 128) exhausted("callback_verification_hosts", 128);
    // Failures count too; varying subscription arguments or URL paths cannot multiply probes.
    await this.storage.put(key, { ...current, count: current.count + 1 });
  }
  private async verify(id: string, url: string, secret: string): Promise<void> {
    const challenge = base64(crypto.getRandomValues(new Uint8Array(32))), startedAt = this.now();
    const body = JSON.stringify({ type: "verification", challenge });
    let failure: FailureReason = "challenge_failed";
    try {
      const response = await this.hooks.webhookFetch(await this.signedRequest(id, url, secret, body, `msg_verification_${crypto.randomUUID()}`), { readBody: true });
      if (!response.ok) { failure = response.status >= 500 ? "http_5xx" : "http_4xx"; await response.body?.cancel(); }
      else {
        const reply: unknown = JSON.parse(await readBounded(response, 4096));
        if (this.now() - startedAt <= 10_000 && record(reply) && await equalChallenge(challenge, reply.challenge)) return;
      }
    } catch (error) { if (!(error instanceof SyntaxError)) failure = reason(error); }
    throw new McpEventFailure(-32015, "CallbackEndpointError", { reason: failure });
  }
  private async poll(key: string, turn: TrackedTurn): Promise<void> {
    let status: McpEventTurnStatus | undefined;
    try { status = await this.hooks.status(turn.grant, turn.turnId); } catch { /* transient upstream error */ }
    const now = this.now();
    if (!status || !["completed", "failed", "cancelled"].includes(status.state)) {
      await this.storage.put(key, { ...turn, nextPoll: now + POLL_MS }); return;
    }
    const completedAt = status.completedAt;
    if (!completedAt || !Number.isFinite(Date.parse(completedAt))) {
      // Observation time cannot stand in for occurrence time: doing so could
      // disclose a historical completion to a newly created subscription.
      await this.storage.put(key, { ...turn, nextPoll: now + POLL_MS }); return;
    }
    const eventId = `evt_${await digest(canonical([turn.principal, turn.turnId, MCP_EVENT_NAME]))}`;
    const data = { agent_id: turn.grant.agentId, turn_id: turn.turnId, status: status.state, completed_at: completedAt,
      ...(typeof status.output === "string" ? { output_preview: status.output.slice(0, 4000), output_truncated: status.output.length > 4000 } : {}) };
    const body = JSON.stringify({ eventId, name: MCP_EVENT_NAME, timestamp: completedAt, data, cursor: null });
    await this.storage.transaction(async transaction => {
      if ((await transaction.get<TrackedTurn>(key))?.done) return;
      const subscriptions = await transaction.list<Subscription>({ prefix: "sub:" });
      const matching = [...subscriptions.values()].filter(subscription => subscription.principal === turn.principal
        && subscription.expiresAt > now && subscription.createdAt <= Date.parse(completedAt)
        && (!subscription.arguments.turn_id || subscription.arguments.turn_id === turn.turnId));
      const outbox = await transaction.list<Delivery>({ prefix: "out:" });
      if (outbox.size + matching.length > MAX_OUTBOX) {
        // Backpressure: retain the terminal turn until its fan-out fits, never mark a dropped event done.
        await transaction.put(key, { ...turn, nextPoll: now + POLL_MS }); return;
      }
      for (const subscription of matching) {
        await transaction.put<Delivery>(`out:${subscription.id}:${eventId}`, { subscriptionId: subscription.id, eventId, body, attempts: 0, createdAt: now, nextAttempt: now });
      }
      await transaction.put(key, { ...turn, done: true, doneAt: now });
    });
  }
  private async deliver(key: string, delivery: Delivery, authorized: Set<string>): Promise<void> {
    const subscription = await this.storage.get<Subscription>(`sub:${delivery.subscriptionId}`);
    if (!subscription || subscription.expiresAt <= this.now()) { await this.storage.delete(key); return; }
    if (!authorized.has(subscription.principal)) return;
    // Recheck after status polling and immediately before disclosing application data.
    try { await this.authorized(subscription.grant); } catch (error) {
      if (error instanceof McpEventFailure) await this.removeSubscription(subscription.id);
      return;
    }
    if (subscription.expiresAt <= this.now()) { await this.removeSubscription(subscription.id); return; }
    if (delivery.attempts >= MAX_ATTEMPTS || this.now() - delivery.createdAt >= RETRY_WINDOW || encoder.encode(delivery.body).length > 262144) {
      await this.storage.delete(key); return;
    }
    // Persist each attempt before network I/O. A process loss cannot reset its retry budget.
    const attempt = { ...delivery, attempts: delivery.attempts + 1, nextAttempt: this.now() + Math.min(5_000 * 2 ** delivery.attempts, 120_000) };
    await this.storage.put(key, attempt);
    let response: Response;
    try {
      response = await this.hooks.webhookFetch(await this.signedRequest(subscription.id, subscription.url, subscription.secret, delivery.body, delivery.eventId,
        (subscription.previousUntil ?? 0) > this.now() ? subscription.previousSecret : undefined));
    } catch { return; }
    await response.body?.cancel();
    if (response.ok || response.status === 410 || response.status === 413 || attempt.attempts >= MAX_ATTEMPTS) await this.storage.delete(key);
  }
  private async removeSubscription(id: string): Promise<void> {
    await this.storage.transaction(async transaction => {
      await transaction.delete(`sub:${id}`);
      for (const key of (await transaction.list({ prefix: `out:${id}:` })).keys()) await transaction.delete(key);
    });
  }
  private async cleanExpired(): Promise<void> {
    const now = this.now();
    for (const subscription of (await this.storage.list<Subscription>({ prefix: "sub:" })).values()) {
      if (subscription.expiresAt <= now || subscription.grant.expiresAt * 1000 <= now) await this.removeSubscription(subscription.id);
      else if (subscription.previousSecret && (subscription.previousUntil ?? 0) <= now) {
        delete subscription.previousSecret; delete subscription.previousUntil;
        await this.storage.put(`sub:${subscription.id}`, subscription);
      }
    }
    for (const [key, turn] of await this.storage.list<TrackedTurn>({ prefix: "turn:" })) if (turn.grant.expiresAt * 1000 <= now || (turn.done && (turn.doneAt ?? 0) + DONE_RETENTION <= now)) await this.storage.delete(key);
    for (const [key, value] of await this.storage.list<VerificationRate>({ prefix: "verify-rate:" })) if (value.expiresAt <= now) await this.storage.delete(key);
    const cache = await this.storage.list<Verification>({ prefix: "verified:" });
    for (const [key, value] of cache) if (value.expiresAt <= now) await this.storage.delete(key);
    // Verification state is bounded even under repeated subscribe/unsubscribe to distinct URLs.
    const surviving = [...cache].filter(([, value]) => value.expiresAt > now).sort((a, b) => a[1].expiresAt - b[1].expiresAt);
    for (const [key] of surviving.slice(0, Math.max(0, surviving.length - 127))) await this.storage.delete(key);
  }
  private async schedule(): Promise<void> {
    const times: number[] = [];
    for (const subscription of (await this.storage.list<Subscription>({ prefix: "sub:" })).values()) times.push(subscription.expiresAt, this.now() + 60_000);
    for (const turn of (await this.storage.list<TrackedTurn>({ prefix: "turn:" })).values()) times.push(turn.done ? Math.min(turn.grant.expiresAt * 1000, (turn.doneAt ?? 0) + DONE_RETENTION) : turn.nextPoll);
    for (const delivery of (await this.storage.list<Delivery>({ prefix: "out:" })).values()) times.push(delivery.nextAttempt);
    for (const verification of (await this.storage.list<Verification>({ prefix: "verified:" })).values()) times.push(verification.expiresAt);
    for (const rate of (await this.storage.list<VerificationRate>({ prefix: "verify-rate:" })).values()) times.push(rate.expiresAt);
    if (times.length) await this.storage.setAlarm(Math.max(this.now() + 1000, Math.min(...times)));
    else await this.storage.deleteAlarm();
  }
}
