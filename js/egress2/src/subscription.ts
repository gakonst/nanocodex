import { subscriptionRevision } from "nanocodex";
import type { ChatGptSubscriptionHandle, ChatGptSubscriptionOptions, ChatGptSubscriptionStore, SubscriptionStoredValue, SubscriptionCommitRequest } from "nanocodex";
import type { ChatGptCredentialImport } from "./chatgpt";
import { CredentialCipher } from "./encryption";

export type SubscriptionOpen = (
  options: Omit<ChatGptSubscriptionOptions, "module">,
) => Promise<ChatGptSubscriptionHandle>;

type StoredRow = { revision: string; payload?: string };

/** One store per owner. Secrets are confined to this DO; CAS protects rotating refresh tokens. */
export class DurableSubscriptionStore implements ChatGptSubscriptionStore {
  constructor(private readonly storage: DurableObjectStorage, private readonly cipher: CredentialCipher,
    private readonly key = "subscription") {}

  async load(_id: string): Promise<SubscriptionStoredValue> {
    const row = await this.storage.get<StoredRow>(this.key);
    return { revision: subscriptionRevision(row?.revision ?? "0"), ...(row?.payload === undefined ? {} : { payload: await this.cipher.open(row.payload, "chatgpt") }) };
  }

  async compareAndSwap(_id: string, request: SubscriptionCommitRequest) {
    const payload = await this.cipher.seal(request.payload, "chatgpt");
    return this.storage.transaction(async tx => {
      const current = await tx.get<StoredRow>(this.key);
      const actualRevision = subscriptionRevision(current?.revision ?? "0");
      if (actualRevision !== request.expectedRevision) {
        return { status: "conflict" as const, actualRevision };
      }
      const revision = subscriptionRevision(BigInt(actualRevision) + 1n);
      await tx.put(this.key, { revision, payload } satisfies StoredRow);
      return { status: "committed" as const, revision };
    });
  }

  /** Copy the validated staged secret into the active row in one durable transaction. */
  async promote(staged: DurableSubscriptionStore): Promise<void> {
    await this.storage.transaction(async tx => {
      const candidate = await tx.get<StoredRow>(staged.key);
      if (!candidate?.payload) throw new Error("Staged subscription unavailable");
      const current = await tx.get<StoredRow>(this.key);
      // Bump past every old manager's CAS revision so a racing refresh cannot
      // overwrite the newly imported credential after the swap.
      const revision = subscriptionRevision(BigInt(current?.revision ?? "0") + 1n);
      await tx.put(this.key, { revision, payload: candidate.payload } satisfies StoredRow);
      await tx.delete(staged.key);
    });
  }

  async discard(): Promise<void> { await this.storage.delete(this.key); }

  /** Explicit import replacement (open(seed) alone intentionally keeps the old credential). */
  async clear(): Promise<void> {
    await this.storage.transaction(async tx => {
      const current = await tx.get<StoredRow>(this.key);
      await tx.put(this.key, { revision: (BigInt(current?.revision ?? "0") + 1n).toString() } satisfies StoredRow);
    });
  }
}

/** Kept inside UserCredentials, never exposed through service RPC. */
export class OwnerSubscription {
  private readonly store: DurableSubscriptionStore;
  private readonly staging: DurableSubscriptionStore;
  private manager?: Promise<ChatGptSubscriptionHandle>;
  private replacing?: Promise<void>;

  constructor(storage: DurableObjectStorage, private readonly id: string, private readonly open: SubscriptionOpen, cipher: CredentialCipher) {
    this.store = new DurableSubscriptionStore(storage, cipher);
    this.staging = new DurableSubscriptionStore(storage, cipher, "subscription-import");
  }

  async credential(): Promise<{ secret: string; accountId: string; fedramp: boolean; expiresAt: number; revision: string }> {
    if (this.replacing) await this.replacing;
    const manager = await this.getManager();
    const credential = await manager.credential(); // Rust proactively refreshes and CAS-persists rotation.
    const status = await manager.status();
    if (status.state !== "authenticated" || status.expiresAt === null || status.expiresAt <= Date.now()) {
      throw new Error("Subscription credential unavailable");
    }
    return { secret: credential.accessToken, accountId: credential.accountId,
      fedramp: credential.fedramp, expiresAt: status.expiresAt, revision: credential.revision };
  }

  async recover(rejectedRevision: string): Promise<{ secret: string; accountId: string; fedramp: boolean; expiresAt: number; revision: string }> {
    if (this.replacing) await this.replacing;
    const manager = await this.getManager();
    const credential = await manager.recover(subscriptionRevision(rejectedRevision));
    const status = await manager.status();
    if (status.state !== "authenticated" || status.expiresAt === null || status.expiresAt <= Date.now()) {
      throw new Error("Subscription credential unavailable");
    }
    return { secret: credential.accessToken, accountId: credential.accountId,
      fedramp: credential.fedramp, expiresAt: status.expiresAt, revision: credential.revision };
  }

  async replace(imported: ChatGptCredentialImport): Promise<void> {
    // Serialize import with reads and another import on this DO instance.
    const previous = this.replacing;
    const work = (async () => {
      if (previous) await previous;
      // The active row and old manager remain untouched until the candidate
      // has opened successfully. A failed import or crash cannot erase them.
      await this.staging.clear();
      const seed = { accessToken: imported.access_token, refreshToken: imported.refresh_token,
        accountId: imported.account_id, fedramp: imported.fedramp };
      let candidate: ChatGptSubscriptionHandle | undefined;
      let promoted = false;
      try {
        candidate = await this.open({ id: this.id, store: this.staging, seed });
        await this.store.promote(this.staging);
        promoted = true;
      } finally {
        if (candidate) { try { candidate.dispose(); } catch { /* no effect on the durable swap */ } }
        if (!promoted) { try { await this.staging.discard(); } catch { /* next import clears stale staging */ } }
      }
      const old = this.manager;
      this.manager = undefined;
      if (old) { try { (await old).dispose(); } catch { /* the new durable record remains active */ } }
    })();
    this.replacing = work;
    try { await work; } finally { if (this.replacing === work) this.replacing = undefined; }
  }

  private getManager(): Promise<ChatGptSubscriptionHandle> {
    return this.manager ??= this.open({ id: this.id, store: this.store }).catch(error => {
      this.manager = undefined;
      throw error;
    });
  }
}
