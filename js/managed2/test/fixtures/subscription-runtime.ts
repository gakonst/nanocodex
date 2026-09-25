import type { ChatGptSubscriptionHandle, ChatGptSubscriptionOptions } from "nanocodex";

/** Only the Rust subscription lifecycle is replaced in this cross-worker test.
 * Egress routing, credential DO storage, and model sockets remain real workerd code. */
export async function openChatGptSubscription(
  options: Omit<ChatGptSubscriptionOptions, "module">,
): Promise<ChatGptSubscriptionHandle> {
  const stored = await options.store.load(options.id);
  const seed = options.seed ?? (stored.payload ? JSON.parse(stored.payload) as NonNullable<typeof options.seed> : undefined);
  if (options.seed) await options.store.compareAndSwap(options.id, {
    expectedRevision: stored.revision, payload: JSON.stringify(options.seed),
  });
  if (!seed) throw new Error("no subscription imported");
  const expiry = Number(JSON.parse(atob(seed.accessToken.split(".")[1]!)).exp) * 1000;
  return {
    id: options.id,
    startLogin: async () => ({ state: "signed_out" }),
    status: async () => ({ state: "authenticated", accountId: seed.accountId, expiresAt: expiry }),
    credential: async () => ({ kind: "chatgpt", accessToken: seed.accessToken, accountId: seed.accountId,
      fedramp: seed.fedramp ?? false, revision: stored.revision }),
    recover: async () => { throw new Error("fixture cannot refresh"); },
    logout: async () => {},
    dispose: () => {},
  };
}
