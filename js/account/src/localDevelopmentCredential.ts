const LOCAL_DEVELOPMENT_HOSTS = new Set([
  "localhost",
  "127.0.0.1",
  "nanocodex.localhost",
]);
const LOCAL_NANOCODEX_INSTANCE = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.nanocodex\.localhost$/;

function isLocalDevelopmentHost(hostname: string): boolean {
  return LOCAL_DEVELOPMENT_HOSTS.has(hostname)
    || LOCAL_NANOCODEX_INSTANCE.test(hostname);
}

type CredentialClaim = Readonly<{
  userId: string;
  promise: Promise<boolean>;
  settled: boolean;
}>;

/**
 * Serializes the local-development-only host credential import for one browser
 * identity. Production never calls the claim route.
 */
export function createLocalDevelopmentCredentialResource(
  request: typeof fetch = globalThis.fetch.bind(globalThis),
  hostname: string = globalThis.location?.hostname ?? "",
) {
  const enabled = isLocalDevelopmentHost(hostname);
  let current: CredentialClaim | undefined;

  const claim = (userId: string): Promise<boolean> => {
    let guarded!: Promise<boolean>;
    let active!: CredentialClaim;
    guarded = request("/v1/credentials/local-claim", {
      method: "POST",
      credentials: "same-origin",
    }).then(async (response) => {
      await response.body?.cancel();
      if (!response.ok) {
        throw new Error(`Local development credential claim failed (HTTP ${response.status})`);
      }
      active = { ...active, settled: true };
      if (current?.promise === guarded) current = active;
      return true;
    }).catch((error) => {
      if (current?.promise === guarded) current = undefined;
      throw error;
    });
    active = { userId, promise: guarded, settled: false };
    current = active;
    return guarded;
  };

  return Object.freeze({
    enabled,
    ensure(userId: string): Promise<boolean> {
      if (!enabled) return Promise.resolve(false);
      if (current?.userId === userId) return current.promise;
      return claim(userId);
    },
    refresh(userId: string): Promise<boolean> {
      if (!enabled) return Promise.resolve(false);
      if (current?.userId === userId && !current.settled) return current.promise;
      return claim(userId);
    },
  });
}

export const localDevelopmentCredential = createLocalDevelopmentCredentialResource();
