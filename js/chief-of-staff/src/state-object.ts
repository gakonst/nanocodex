import { DurableObject } from "cloudflare:workers";
import type { Lock, QueueEntry } from "chat";
import {
  ConversationEngine,
  ConversationError,
  type ConversationStore,
  type ConversationTurnRequest,
} from "./conversation.ts";
import {
  claimDelivery,
  completeDelivery,
  type DeliveryRecord,
  releaseDelivery,
} from "./delivery.ts";
import {
  NanocodexManagedGateway,
  type ChiefOfStaffIdentity,
} from "./managed.ts";
import {
  sameChannelIdentity,
  validSlackInstallationMetadata,
  type ChannelIdentity,
  type SlackInstallationMetadata,
} from "./protocol.ts";
import type { Env } from "./worker.ts";

type ExpiringValue = Readonly<{ expiresAt: number | null; value: unknown }>;

function chiefIdentity(channel: ChannelIdentity): ChiefOfStaffIdentity {
  switch (channel.platform) {
    case "slack": return {
      provider: "slack",
      subject: channel.userId,
      tenant: channel.teamId,
    };
    case "viber": return {
      provider: "viber",
      subject: channel.userId,
      tenant: channel.botUri,
    };
    case "whatsapp": return {
      provider: "whatsapp",
      subject: channel.userId,
      tenant: channel.businessPhoneNumberId,
    };
  }
}

export class ChiefOfStaffState extends DurableObject<Env> {
  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/chat-sdk" && request.method === "POST") return this.chatState(request);
    if (url.pathname === "/conversation/turn" && request.method === "POST") return this.conversationTurn(request);
    if (url.pathname === "/conversation/delivery" && request.method === "POST") {
      return this.conversationDelivery(request);
    }
    if (url.pathname === "/slack/installations/claim" && request.method === "POST") {
      return this.claimSlackInstallation(request);
    }
    if (url.pathname === "/slack/installations") return this.slackInstallations(request);
    return json({ error: "not_found" }, 404);
  }

  private async conversationDelivery(request: Request): Promise<Response> {
    let body: Record<string, unknown>;
    try { body = await request.json<Record<string, unknown>>(); }
    catch { return json({ error: "invalid_request" }, 400); }
    const deliveryId = typeof body.deliveryId === "string" ? body.deliveryId : "";
    if (!/^viber:(?:reply|welcome):[0-9]+$/.test(deliveryId)) {
      return json({ error: "invalid_request" }, 400);
    }
    const key = `delivery:${deliveryId}`;
    switch (body.operation) {
      case "claim": return this.ctx.storage.transaction(async (transaction) => {
        const retained = await transaction.get<DeliveryRecord>(key);
        const now = Date.now();
        const claim = claimDelivery(retained, now, now + 45_000, crypto.randomUUID());
        if (claim.status === "claimed") await transaction.put(key, claim.record);
        return json({ status: claim.status, token: claim.token }, 200);
      });
      case "complete": {
        if (typeof body.token !== "string") return json({ error: "invalid_request" }, 400);
        const token = body.token;
        const completed = await this.ctx.storage.transaction(async (transaction) => {
          const record = completeDelivery(await transaction.get<DeliveryRecord>(key), token);
          if (!record) return false;
          await transaction.put(key, record);
          return true;
        });
        return completed ? json({ status: "completed" }, 200) : json({ error: "claim_conflict" }, 409);
      }
      case "release": {
        if (typeof body.token !== "string") return json({ error: "invalid_request" }, 400);
        const token = body.token;
        await this.ctx.storage.transaction(async (transaction) => {
          const retained = await transaction.get<DeliveryRecord>(key);
          if (releaseDelivery(retained, token)) await transaction.delete(key);
        });
        return json({ status: "released" }, 200);
      }
      default: return json({ error: "invalid_request" }, 400);
    }
  }

  private async slackInstallations(request: Request): Promise<Response> {
    if (request.method === "GET") {
      const retained = await this.ctx.storage.list<SlackInstallationMetadata>({ prefix: "slack:metadata:" });
      const installations = [...retained.values()]
        .filter(validSlackInstallationMetadata)
        .sort((left, right) => right.installedAt - left.installedAt);
      return json({ installations }, 200);
    }
    let body: unknown;
    try { body = await request.json(); }
    catch { return json({ error: "invalid_request" }, 400); }
    if (!validSlackInstallationMetadata(body)) return json({ error: "invalid_request" }, 400);
    const key = `slack:metadata:${body.teamId}`;
    if (request.method === "PUT") {
      return this.ctx.storage.transaction(async (transaction) => {
        const ownerKey = `slack:owner:${body.teamId}`;
        const owner = await transaction.get<string>(ownerKey);
        const current = await transaction.get<unknown>(key);
        const expected = owner ?? (validSlackInstallationMetadata(current) ? current.accountId : undefined);
        if (expected !== body.accountId) return json({ error: "account_forbidden" }, 409);
        await transaction.put(key, body);
        return new Response(null, { status: 204 });
      });
    }
    if (request.method === "DELETE") {
      return this.ctx.storage.transaction(async (transaction) => {
        const ownerKey = `slack:owner:${body.teamId}`;
        const owner = await transaction.get<string>(ownerKey);
        const current = await transaction.get<unknown>(key);
        const expected = owner ?? (validSlackInstallationMetadata(current) ? current.accountId : undefined);
        if (expected !== undefined && expected !== body.accountId) {
          return json({ error: "account_forbidden" }, 409);
        }
        await Promise.all([transaction.delete(key), transaction.delete(ownerKey)]);
        return new Response(null, { status: 204 });
      });
    }
    return json({ error: "method_not_allowed" }, 405);
  }

  private async claimSlackInstallation(request: Request): Promise<Response> {
    let body: unknown;
    try { body = await request.json(); }
    catch { return json({ error: "invalid_request" }, 400); }
    if (!isRecord(body)
      || Object.keys(body).length !== 2
      || typeof body.accountId !== "string"
      || !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(body.accountId)
      || typeof body.teamId !== "string"
      || !/^T[A-Z0-9]+$/.test(body.teamId)) {
      return json({ error: "invalid_request" }, 400);
    }
    return this.ctx.storage.transaction(async (transaction) => {
      const ownerKey = `slack:owner:${body.teamId}`;
      const metadata = await transaction.get<unknown>(`slack:metadata:${body.teamId}`);
      const owner = await transaction.get<string>(ownerKey)
        ?? (validSlackInstallationMetadata(metadata) ? metadata.accountId : undefined);
      if (owner !== undefined && owner !== body.accountId) {
        return json({ error: "workspace_already_installed" }, 409);
      }
      if (owner === undefined) await transaction.put(ownerKey, body.accountId);
      return new Response(null, { status: 204 });
    });
  }

  private async conversationTurn(request: Request): Promise<Response> {
    if (!this.env.NANOCODEX_BACKEND) {
      return json({ error: "managed_service_unavailable" }, 503);
    }
    let body: ConversationTurnRequest;
    try { body = await request.json<ConversationTurnRequest>(); }
    catch { return json({ error: "invalid_request" }, 400); }
    try {
      const engine = new ConversationEngine(
        new DurableConversationStore(this.ctx.storage),
        new NanocodexManagedGateway(this.env.NANOCODEX_BACKEND, chiefIdentity(body.channel)),
      );
      return json(await engine.turn(body), 200);
    } catch (error) {
      if (error instanceof ConversationError) return json({ error: error.code }, error.status);
      console.warn({
        type: "chief_of_staff.turn_failed",
        error_kind: error instanceof Error ? error.name : typeof error,
      });
      return json({ error: "turn_unavailable" }, 503);
    }
  }

  private async chatState(request: Request): Promise<Response> {
    let body: Record<string, unknown>;
    try { body = await request.json<Record<string, unknown>>(); }
    catch { return json({ error: "invalid_request" }, 400); }
    const operation = body.operation;
    if (typeof operation !== "string") return json({ error: "invalid_request" }, 400);
    try {
      return json({ value: await this.runStateOperation(operation, body) }, 200);
    } catch {
      return json({ error: "invalid_state_operation" }, 400);
    }
  }

  private async runStateOperation(operation: string, body: Record<string, unknown>): Promise<unknown> {
    const key = typeof body.key === "string" ? `value:${body.key}` : undefined;
    const threadId = typeof body.threadId === "string" ? body.threadId : undefined;
    switch (operation) {
      case "get": return key ? this.getValue(key) : null;
      case "set": {
        if (!key) throw new Error("key required");
        await this.putValue(key, body.value, optionalNumber(body.ttlMs));
        return null;
      }
      case "setIfNotExists": {
        if (!key) throw new Error("key required");
        return this.setIfMissing(key, body.value, optionalNumber(body.ttlMs));
      }
      case "delete": {
        if (!key) throw new Error("key required");
        await this.ctx.storage.delete(key);
        return null;
      }
      case "appendToList": {
        if (!key) throw new Error("key required");
        const options = isRecord(body.options) ? body.options : {};
        await this.appendList(
          key,
          body.value,
          optionalNumber(options.maxLength),
          optionalNumber(options.ttlMs),
        );
        return null;
      }
      case "getList": return key ? (await this.getValue(key) ?? []) : [];
      case "subscribe": {
        if (!threadId) throw new Error("thread required");
        await this.ctx.storage.put(`subscription:${threadId}`, true);
        return null;
      }
      case "unsubscribe": {
        if (!threadId) throw new Error("thread required");
        await this.ctx.storage.delete(`subscription:${threadId}`);
        return null;
      }
      case "isSubscribed": return threadId
        ? (await this.ctx.storage.get(`subscription:${threadId}`)) === true
        : false;
      case "acquireLock": return threadId
        ? this.acquireLock(threadId, requiredNumber(body.ttlMs))
        : null;
      case "extendLock": return this.extendLock(body.lock, requiredNumber(body.ttlMs));
      case "releaseLock": await this.releaseLock(body.lock); return null;
      case "forceReleaseLock": {
        if (!threadId) throw new Error("thread required");
        await this.ctx.storage.delete(`lock:${threadId}`);
        return null;
      }
      case "enqueue": {
        if (!threadId) throw new Error("thread required");
        return this.enqueue(threadId, body.entry, requiredNumber(body.maxSize));
      }
      case "dequeue": return threadId ? this.dequeue(threadId) : null;
      case "queueDepth": return threadId
        ? (await this.ctx.storage.get<QueueEntry[]>(`queue:${threadId}`) ?? []).length
        : 0;
      default: throw new Error("unsupported operation");
    }
  }

  private async getValue(key: string): Promise<unknown | null> {
    const retained = await this.ctx.storage.get<ExpiringValue>(key);
    if (!retained) return null;
    if (retained.expiresAt !== null && retained.expiresAt <= Date.now()) {
      await this.ctx.storage.delete(key);
      return null;
    }
    return retained.value;
  }

  private async putValue(key: string, value: unknown, ttlMs?: number): Promise<void> {
    await this.ctx.storage.put(key, {
      expiresAt: ttlMs === undefined ? null : Date.now() + ttlMs,
      value,
    } satisfies ExpiringValue);
  }

  private async setIfMissing(key: string, value: unknown, ttlMs?: number): Promise<boolean> {
    return this.ctx.storage.transaction(async (transaction) => {
      const retained = await transaction.get<ExpiringValue>(key);
      if (retained && (retained.expiresAt === null || retained.expiresAt > Date.now())) return false;
      await transaction.put(key, {
        expiresAt: ttlMs === undefined ? null : Date.now() + ttlMs,
        value,
      } satisfies ExpiringValue);
      return true;
    });
  }

  private async appendList(
    key: string,
    value: unknown,
    maxLength?: number,
    ttlMs?: number,
  ): Promise<void> {
    await this.ctx.storage.transaction(async (transaction) => {
      const retained = await transaction.get<ExpiringValue>(key);
      const active = retained && (retained.expiresAt === null || retained.expiresAt > Date.now());
      const list = active && Array.isArray(retained.value) ? [...retained.value, value] : [value];
      const trimmed = maxLength === undefined ? list : list.slice(-maxLength);
      await transaction.put(key, {
        expiresAt: ttlMs === undefined ? null : Date.now() + ttlMs,
        value: trimmed,
      } satisfies ExpiringValue);
    });
  }

  private async acquireLock(threadId: string, ttlMs: number): Promise<Lock | null> {
    return this.ctx.storage.transaction(async (transaction) => {
      const key = `lock:${threadId}`;
      const retained = await transaction.get<Lock>(key);
      if (retained && retained.expiresAt > Date.now()) return null;
      const lock = { expiresAt: Date.now() + ttlMs, threadId, token: crypto.randomUUID() };
      await transaction.put(key, lock);
      return lock;
    });
  }

  private async extendLock(value: unknown, ttlMs: number): Promise<boolean> {
    const lock = decodeLock(value);
    return this.ctx.storage.transaction(async (transaction) => {
      const key = `lock:${lock.threadId}`;
      const retained = await transaction.get<Lock>(key);
      if (!retained || retained.token !== lock.token || retained.expiresAt <= Date.now()) return false;
      await transaction.put(key, { ...retained, expiresAt: Date.now() + ttlMs });
      return true;
    });
  }

  private async releaseLock(value: unknown): Promise<void> {
    const lock = decodeLock(value);
    await this.ctx.storage.transaction(async (transaction) => {
      const key = `lock:${lock.threadId}`;
      const retained = await transaction.get<Lock>(key);
      if (retained?.token === lock.token) await transaction.delete(key);
    });
  }

  private async enqueue(threadId: string, entry: unknown, maxSize: number): Promise<number> {
    if (!isRecord(entry)) throw new Error("entry required");
    return this.ctx.storage.transaction(async (transaction) => {
      const key = `queue:${threadId}`;
      const queue = await transaction.get<QueueEntry[]>(key) ?? [];
      queue.push(entry as unknown as QueueEntry);
      const retained = queue.slice(-maxSize);
      await transaction.put(key, retained);
      return retained.length;
    });
  }

  private async dequeue(threadId: string): Promise<QueueEntry | null> {
    return this.ctx.storage.transaction(async (transaction) => {
      const key = `queue:${threadId}`;
      const queue = await transaction.get<QueueEntry[]>(key) ?? [];
      while (queue.length > 0) {
        const entry = queue.shift()!;
        if (entry.expiresAt > Date.now()) {
          await transaction.put(key, queue);
          return entry;
        }
      }
      await transaction.delete(key);
      return null;
    });
  }
}

class DurableConversationStore implements ConversationStore {
  private readonly storage: DurableObjectStorage;

  constructor(storage: DurableObjectStorage) {
    this.storage = storage;
  }

  async bindIdentity(identity: ChannelIdentity): Promise<boolean> {
    return this.storage.transaction(async (transaction) => {
      const retained = await transaction.get<ChannelIdentity>("conversation:identity");
      if (retained && !sameChannelIdentity(retained, identity)) return false;
      if (!retained) await transaction.put("conversation:identity", identity);
      return true;
    });
  }

  get<T>(key: string): Promise<T | undefined> {
    return this.storage.get<T>(`conversation:${key}`);
  }

  async put<T>(key: string, value: T): Promise<void> {
    await this.storage.put(`conversation:${key}`, value);
  }
}

function decodeLock(value: unknown): Lock {
  if (!isRecord(value)
    || typeof value.threadId !== "string"
    || typeof value.token !== "string"
    || typeof value.expiresAt !== "number") throw new Error("lock required");
  return value as unknown as Lock;
}

function requiredNumber(value: unknown): number {
  if (!Number.isSafeInteger(value) || Number(value) <= 0) throw new Error("positive integer required");
  return Number(value);
}

function optionalNumber(value: unknown): number | undefined {
  return value === undefined ? undefined : requiredNumber(value);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function json(body: unknown, status: number): Response {
  return Response.json(body, {
    status,
    headers: { "cache-control": "no-store", "x-content-type-options": "nosniff" },
  });
}
