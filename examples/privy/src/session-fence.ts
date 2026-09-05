export type SessionCleanup = Readonly<{
  revoke(): Promise<void>;
  shutdownAgent?(): Promise<void>;
  logoutConnect(): Promise<void>;
  clearUi(): void;
}>;

export async function clearSessionBeforeProviderChange(options: SessionCleanup): Promise<void> {
  // A failed revocation must preserve both sessions so the user can retry. Provider
  // logout is deliberately owned by the caller and happens only after this resolves.
  await options.revoke();
  try {
    await options.shutdownAgent?.();
    await options.logoutConnect();
  } finally {
    options.clearUi();
  }
}

export function createSessionFence() {
  let running: Readonly<{ abort: AbortController; promise: Promise<unknown> }> | undefined;
  let cleaning: Promise<void> | undefined;

  return Object.freeze({
    run<T>(operation: (signal: AbortSignal) => Promise<T>): Promise<T> {
      if (cleaning) return Promise.reject(new Error("Secure logout is in progress."));
      if (running) return Promise.reject(new Error("A connection operation is already in progress."));
      const abort = new AbortController();
      const promise = Promise.resolve().then(() => operation(abort.signal));
      const current = { abort, promise };
      running = current;
      void promise.then(clear, clear);
      return promise;

      function clear() {
        if (running === current) running = undefined;
      }
    },

    async cancel(): Promise<void> {
      const pending = running;
      if (!pending) return;
      pending.abort.abort();
      try {
        await pending.promise;
      } catch {
        // The caller is replacing this operation and intentionally owns its outcome.
      }
    },

    beforeProviderChange(options: SessionCleanup): Promise<void> {
      if (cleaning) return cleaning;
      const pending = running;
      const promise = (async () => {
        if (pending) {
          pending.abort.abort();
          try {
            await pending.promise;
          } catch {
            // Cancellation and failed connection attempts cannot skip session cleanup.
          }
        }
        await clearSessionBeforeProviderChange(options);
      })();
      cleaning = promise;
      void promise.then(clear, clear);
      return promise;

      function clear() {
        if (cleaning === promise) cleaning = undefined;
      }
    },
  });
}

export async function revokeHostPrincipal(fetcher: typeof fetch = fetch): Promise<void> {
  const response = await fetcher("/api/nanocodex/host-principal", {
    method: "DELETE",
    credentials: "same-origin",
    headers: { accept: "application/json" },
  });
  if (!response.ok) throw new Error(`Nanocodex session revocation failed (${response.status}).`);
}
