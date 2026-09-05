const MAX_REVISION = 18_446_744_073_709_551_615n;

export function subscriptionRevision(value) {
  const revision = String(value);
  if (!/^(0|[1-9][0-9]*)$/.test(revision) || BigInt(revision) > MAX_REVISION) {
    throw new TypeError("subscription revision must be an unsigned 64-bit decimal string");
  }
  return revision;
}

export function createMemoryChatGptSubscriptionStore(id, initial) {
  if (typeof id !== "string" || !id.trim()) {
    throw new TypeError("subscription ID must be a non-empty string");
  }
  let stored = Object.freeze({
    revision: subscriptionRevision(initial?.revision ?? 0n),
    ...(initial?.payload === undefined ? {} : { payload: initial.payload }),
  });
  const select = (selected) => {
    if (selected !== id) throw new Error(`unknown ChatGPT subscription: ${selected}`);
  };
  return Object.freeze({
    id,
    load(selected) {
      select(selected);
      return stored;
    },
    compareAndSwap(selected, request) {
      select(selected);
      const expectedRevision = subscriptionRevision(request.expectedRevision);
      if (expectedRevision !== stored.revision) {
        return { status: "conflict", actualRevision: stored.revision };
      }
      const revision = subscriptionRevision(BigInt(stored.revision) + 1n);
      stored = Object.freeze({ revision, payload: request.payload });
      return { status: "committed", revision };
    },
    snapshot() {
      return stored;
    },
  });
}
