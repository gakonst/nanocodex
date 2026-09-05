const CLEANUP_HOST_LOCK = "nanocodex-cleanup-host-v1";

export interface CleanupHostLock {
  release(): Promise<void>;
}

type LockRequester = Pick<LockManager, "request">;

export async function acquireCleanupHost(
  locks: LockRequester = navigator.locks,
): Promise<CleanupHostLock | undefined> {
  let releaseHold: (() => void) | undefined;
  const hold = new Promise<void>((resolve) => {
    releaseHold = resolve;
  });
  let resolveAcquired: ((acquired: boolean) => void) | undefined;
  let rejectAcquired: ((cause: unknown) => void) | undefined;
  const acquired = new Promise<boolean>((resolve, reject) => {
    resolveAcquired = resolve;
    rejectAcquired = reject;
  });
  const request = locks.request(
    CLEANUP_HOST_LOCK,
    { ifAvailable: true },
    async (lock) => {
      resolveAcquired?.(lock !== null);
      if (lock !== null) await hold;
    },
  );
  void request.catch((cause) => rejectAcquired?.(cause));
  if (!await acquired) {
    await request;
    return undefined;
  }
  let released = false;
  return {
    async release() {
      if (!released) {
        released = true;
        releaseHold?.();
      }
      await request;
    },
  };
}
