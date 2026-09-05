export type RetainedResourceLease = {
  acquire(): () => void;
  retain(): void;
};

export function createRetainedResourceLease(
  retentionMs: number,
  expire: () => void,
): RetainedResourceLease {
  let activeLeases = 0;
  let expiry: ReturnType<typeof setTimeout> | undefined;

  const cancelExpiry = () => {
    clearTimeout(expiry);
    expiry = undefined;
  };

  return {
    acquire() {
      activeLeases++;
      cancelExpiry();
      let active = true;
      return () => {
        if (!active) return;
        active = false;
        activeLeases--;
      };
    },
    retain() {
      cancelExpiry();
      if (activeLeases > 0) return;
      const retainedExpiry = setTimeout(() => {
        if (expiry !== retainedExpiry) return;
        expiry = undefined;
        if (activeLeases > 0) return;
        expire();
      }, retentionMs);
      expiry = retainedExpiry;
    },
  };
}
