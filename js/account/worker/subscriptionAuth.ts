import {
  subscriptionRevision,
  type ChatGptCredential,
  type ChatGptLoginStatus,
  type ChatGptSubscriptionHandle,
  type ChatGptSubscriptionStore,
  type SubscriptionCommitRequest,
  type SubscriptionStoredValue,
} from "nanocodex";

import { CredentialVault, type CredentialVaultEnv, type EncryptedEnvelope } from "./credentialVault.ts";

export const CHATGPT_LOGIN_TTL_MS = 15 * 60_000;
export const CHATGPT_SESSION_TTL_MS = 7 * 24 * 60 * 60_000;
const USAGE_WINDOW_MS = 60_000;
const SOCKET_LEASE_MS = 2 * 60 * 60_000;
const MAX_ACTIVE_SOCKETS = 8;
const OPERATION_LIMITS = {
  socket: 12,
  search: 30,
  image: 4,
} as const;

export type ChatGptOperation = "health" | keyof typeof OPERATION_LIMITS;
export type { ChatGptCredential };

type StoredUsage = {
  windows: Partial<Record<keyof typeof OPERATION_LIMITS, { startedAt: number; count: number }>>;
  socketLeases: Record<string, number>;
};

type StoredSubscriptionRow = {
  revision: string;
  envelope: EncryptedEnvelope;
};

type SubscriptionRuntime = {
  open(options: {
    id: string;
    store: ChatGptSubscriptionStore;
    issuer?: string;
  }): Promise<ChatGptSubscriptionHandle>;
};

export class ChatGptSession {
  readonly #state: DurableObjectState;
  readonly #store: DurableSubscriptionStore;
  readonly #issuer?: string;
  readonly #runtime?: SubscriptionRuntime;
  #subscription?: Promise<ChatGptSubscriptionHandle>;

  constructor(
    state: DurableObjectState,
    env: CredentialVaultEnv & { CHATGPT_ISSUER?: string },
    runtime?: SubscriptionRuntime,
  ) {
    this.#state = state;
    const scope = `chatgpt/${state.id?.toString() ?? "test"}`;
    this.#store = new DurableSubscriptionStore(state.storage, new CredentialVault(env, scope));
    this.#issuer = env.CHATGPT_ISSUER?.trim() || undefined;
    this.#runtime = runtime;
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    try {
      if (request.method === "POST" && url.pathname === "/start") {
        const status = await (await this.#manager()).startLogin();
        await this.#state.storage.setAlarm(Date.now() + CHATGPT_LOGIN_TTL_MS);
        return Response.json(status, { headers: noStoreHeaders() });
      }
      if (request.method === "GET" && url.pathname === "/status") {
        const status = await (await this.#manager()).status();
        if (status.state === "authenticated") {
          await this.#state.storage.setAlarm(Date.now() + CHATGPT_SESSION_TTL_MS);
        }
        return Response.json(status, { headers: noStoreHeaders() });
      }
      if (request.method === "POST" && url.pathname === "/credential") {
        const body = await request.json<{ operation?: unknown; leaseId?: unknown }>();
        const operation = parseOperation(body.operation);
        if (!operation) {
          return Response.json({ error: "invalid operation" }, { status: 400, headers: noStoreHeaders() });
        }
        const credential = await (await this.#manager()).credential();
        if (!await this.#consume(operation, body.leaseId)) {
          return Response.json(
            { error: "session_rate_limit_exceeded" },
            { status: 429, headers: { ...noStoreHeaders(), "retry-after": "60" } },
          );
        }
        return Response.json(credential, { headers: noStoreHeaders() });
      }
      if (request.method === "DELETE" && url.pathname === "/lease") {
        const body = await request.json<{ leaseId?: unknown }>();
        await this.#releaseLease(body.leaseId);
        return new Response(null, { status: 204, headers: noStoreHeaders() });
      }
      if (request.method === "POST" && url.pathname === "/recover") {
        const body = await request.json<{ revision?: unknown }>();
        const revision = parseRevision(body.revision);
        if (!revision) {
          return Response.json({ error: "invalid revision" }, { status: 400, headers: noStoreHeaders() });
        }
        const credential = await (await this.#manager()).recover(revision);
        return Response.json(credential, { headers: noStoreHeaders() });
      }
      if (request.method === "DELETE") {
        await (await this.#manager()).logout();
        await this.#state.storage.delete("usage");
        return new Response(null, { status: 204, headers: noStoreHeaders() });
      }
      return Response.json({ error: "not_found" }, { status: 404, headers: noStoreHeaders() });
    } catch (error) {
      const message = safeError(error);
      const status = message.includes("not authenticated") ? 404 : 503;
      return Response.json({ error: message }, { status, headers: noStoreHeaders() });
    }
  }

  async alarm(): Promise<void> {
    await this.#state.storage.deleteAll();
  }

  #manager(): Promise<ChatGptSubscriptionHandle> {
    return this.#subscription ??= this.#openManager();
  }

  async #openManager(): Promise<ChatGptSubscriptionHandle> {
    const options = {
      id: `chatgpt:${this.#state.id?.toString() ?? "test"}`,
      store: this.#store,
      ...(this.#issuer === undefined ? {} : { issuer: this.#issuer }),
    };
    if (this.#runtime) return this.#runtime.open(options);
    const { openChatGptSubscription } = await import("./subscriptionRuntime.ts");
    return openChatGptSubscription(options);
  }

  async #consume(operation: ChatGptOperation, leaseId: unknown): Promise<boolean> {
    if (operation === "health") return true;
    const now = Date.now();
    const usage = await this.#state.storage.get<StoredUsage>("usage") ?? {
      windows: {},
      socketLeases: {},
    };
    usage.socketLeases = Object.fromEntries(
      Object.entries(usage.socketLeases).filter(([, expiresAt]) => expiresAt > now),
    );
    const current = usage.windows[operation];
    const window = !current || current.startedAt + USAGE_WINDOW_MS <= now
      ? { startedAt: now, count: 0 }
      : current;
    if (window.count >= OPERATION_LIMITS[operation]) return false;
    if (operation === "socket") {
      if (!isLeaseId(leaseId) || Object.keys(usage.socketLeases).length >= MAX_ACTIVE_SOCKETS) {
        return false;
      }
      usage.socketLeases[leaseId] = now + SOCKET_LEASE_MS;
    }
    window.count += 1;
    usage.windows[operation] = window;
    await this.#state.storage.put("usage", usage);
    return true;
  }

  async #releaseLease(leaseId: unknown): Promise<void> {
    if (!isLeaseId(leaseId)) return;
    const usage = await this.#state.storage.get<StoredUsage>("usage");
    if (!usage || !(leaseId in usage.socketLeases)) return;
    delete usage.socketLeases[leaseId];
    await this.#state.storage.put("usage", usage);
  }
}

class DurableSubscriptionStore implements ChatGptSubscriptionStore {
  readonly #storage: DurableObjectStorage;
  readonly #vault: CredentialVault;

  constructor(storage: DurableObjectStorage, vault: CredentialVault) {
    this.#storage = storage;
    this.#vault = vault;
  }

  async load(_id: string): Promise<SubscriptionStoredValue> {
    const row = await this.#storage.get<StoredSubscriptionRow>("subscription");
    if (!row) return { revision: subscriptionRevision(0n) };
    const opened = await this.#vault.open<{ payload: string }>(row.envelope);
    if (opened.reseal) {
      await this.#storage.put("subscription", {
        revision: row.revision,
        envelope: await this.#vault.seal(opened.value),
      } satisfies StoredSubscriptionRow);
    }
    return {
      revision: subscriptionRevision(row.revision),
      payload: opened.value.payload,
    };
  }

  async compareAndSwap(
    _id: string,
    request: SubscriptionCommitRequest,
  ) {
    const envelope: EncryptedEnvelope = await this.#vault.seal({ payload: request.payload });
    return this.#storage.transaction(async (transaction) => {
      const current = await transaction.get<StoredSubscriptionRow>("subscription");
      const actualRevision = subscriptionRevision(current?.revision ?? "0");
      if (actualRevision !== request.expectedRevision) {
        return { status: "conflict" as const, actualRevision };
      }
      const revision = subscriptionRevision(BigInt(actualRevision) + 1n);
      await transaction.put("subscription", {
        revision,
        envelope,
      } satisfies StoredSubscriptionRow);
      return { status: "committed" as const, revision };
    });
  }
}

function parseOperation(value: unknown): ChatGptOperation | undefined {
  return value === "health" || value === "socket" || value === "search" || value === "image"
    ? value
    : undefined;
}

function parseRevision(value: unknown) {
  if (typeof value !== "string" || !/^(0|[1-9][0-9]*)$/.test(value)) return undefined;
  return subscriptionRevision(value);
}

function isLeaseId(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9_-]{43}$/.test(value);
}

function noStoreHeaders() {
  return { "cache-control": "no-store" };
}

function safeError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
