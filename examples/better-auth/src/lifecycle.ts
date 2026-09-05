export type HostLogoutCleanup = Readonly<{
  revoke(): Promise<void>;
  logoutConnect(): Promise<void>;
  clearUi(): void;
}>;

export function createConnectLifecycle() {
  let active: Readonly<{
    abort: AbortController;
    promise: Promise<unknown>;
  }> | undefined;
  let cleanup: Promise<void> | undefined;

  return Object.freeze({
    run<T>(operation: (signal: AbortSignal) => Promise<T>): Promise<T> {
      if (cleanup) return Promise.reject(new Error("Host logout cleanup is in progress."));
      if (active) return Promise.reject(new Error("A Connect operation is already in progress."));

      const abort = new AbortController();
      const promise = Promise.resolve().then(() => operation(abort.signal));
      const current = { abort, promise };
      active = current;
      void promise.then(clear, clear);
      return promise;

      function clear() {
        if (active === current) active = undefined;
      }
    },

    beforeProviderLogout(options: HostLogoutCleanup): Promise<void> {
      if (cleanup) return cleanup;
      const pending = active;
      const operation = (async () => {
        if (pending) {
          pending.abort.abort();
          try {
            await pending.promise;
          } catch {
            // A cancelled or failed Connect attempt cannot skip the session fence.
          }
        }
        await options.revoke();
        try {
          await options.logoutConnect();
        } finally {
          options.clearUi();
        }
      })();
      cleanup = operation;
      void operation.then(clear, clear);
      return operation;

      function clear() {
        if (cleanup === operation) cleanup = undefined;
      }
    },
  });
}

export async function revokeHostPrincipal(
  fetcher: typeof fetch = fetch,
): Promise<void> {
  const response = await fetcher("/api/nanocodex/host-principal", {
    method: "DELETE",
    credentials: "same-origin",
    headers: { accept: "application/json" },
  });
  if (!response.ok) {
    throw new Error(`Nanocodex host-session revocation failed (${response.status}).`);
  }
}
