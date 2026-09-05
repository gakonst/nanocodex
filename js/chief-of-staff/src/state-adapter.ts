import type { Lock, QueueEntry, StateAdapter } from "chat";

type Fetcher = Readonly<{ fetch(request: Request): Promise<Response> }>;

export class DurableChatStateAdapter implements StateAdapter {
  protected readonly state: Fetcher;

  constructor(state: Fetcher) {
    this.state = state;
  }

  async connect(): Promise<void> {}
  async disconnect(): Promise<void> {}

  async acquireLock(threadId: string, ttlMs: number): Promise<Lock | null> {
    return this.call("acquireLock", { threadId, ttlMs });
  }

  async appendToList(
    key: string,
    value: unknown,
    options?: { maxLength?: number; ttlMs?: number },
  ): Promise<void> {
    await this.call("appendToList", { key, value, options });
  }

  async delete(key: string): Promise<void> {
    await this.call("delete", { key });
  }

  async dequeue(threadId: string): Promise<QueueEntry | null> {
    return this.call("dequeue", { threadId });
  }

  async enqueue(threadId: string, entry: QueueEntry, maxSize: number): Promise<number> {
    return this.call("enqueue", { threadId, entry, maxSize });
  }

  async extendLock(lock: Lock, ttlMs: number): Promise<boolean> {
    return this.call("extendLock", { lock, ttlMs });
  }

  async forceReleaseLock(threadId: string): Promise<void> {
    await this.call("forceReleaseLock", { threadId });
  }

  async get<T = unknown>(key: string): Promise<T | null> {
    return this.call("get", { key });
  }

  async getList<T = unknown>(key: string): Promise<T[]> {
    return this.call("getList", { key });
  }

  async isSubscribed(threadId: string): Promise<boolean> {
    return this.call("isSubscribed", { threadId });
  }

  async queueDepth(threadId: string): Promise<number> {
    return this.call("queueDepth", { threadId });
  }

  async releaseLock(lock: Lock): Promise<void> {
    await this.call("releaseLock", { lock });
  }

  async set<T = unknown>(key: string, value: T, ttlMs?: number): Promise<void> {
    await this.call("set", { key, value, ttlMs });
  }

  async setIfNotExists(key: string, value: unknown, ttlMs?: number): Promise<boolean> {
    return this.call("setIfNotExists", { key, value, ttlMs });
  }

  async subscribe(threadId: string): Promise<void> {
    await this.call("subscribe", { threadId });
  }

  async unsubscribe(threadId: string): Promise<void> {
    await this.call("unsubscribe", { threadId });
  }

  private async call<T>(operation: string, body: Record<string, unknown>): Promise<T> {
    const response = await this.state.fetch(new Request("https://state.internal/chat-sdk", {
      body: JSON.stringify({ operation, ...body }),
      headers: { "content-type": "application/json" },
      method: "POST",
    }));
    if (!response.ok) throw new Error(`Chat SDK state operation failed: ${operation}`);
    const result = await response.json<{ value: T }>();
    return result.value;
  }
}

export class SlackInstallationOwnershipError extends Error {
  constructor() { super("slack_workspace_already_installed"); }
}

export class SlackOAuthStateAdapter extends DurableChatStateAdapter {
  private readonly accountId: string;

  constructor(state: Fetcher, accountId: string) {
    super(state);
    this.accountId = accountId;
  }

  override async set<T = unknown>(key: string, value: T, ttlMs?: number): Promise<void> {
    const teamId = /^slack:installation:(T[A-Z0-9]+)$/.exec(key)?.[1];
    if (teamId) {
      const claimed = await this.state.fetch(new Request(
        "https://state.internal/slack/installations/claim",
        {
          body: JSON.stringify({ accountId: this.accountId, teamId }),
          headers: { "content-type": "application/json" },
          method: "POST",
        },
      ));
      if (claimed.status === 409) throw new SlackInstallationOwnershipError();
      if (!claimed.ok) throw new Error("Slack installation ownership claim failed");
      await claimed.body?.cancel();
    }
    await super.set(key, value, ttlMs);
  }
}
