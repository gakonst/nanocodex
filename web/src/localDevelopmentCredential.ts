const LOCAL_DEVELOPMENT_HOSTS = new Set([
  "localhost",
  "127.0.0.1",
  "nanocodex.localhost",
]);

function isLocalDevelopmentHost(hostname: string): boolean {
  return LOCAL_DEVELOPMENT_HOSTS.has(hostname)
    || hostname.endsWith(".nanocodex.localhost");
}

type CredentialClaim = Readonly<{
  userId: string;
  promise: Promise<boolean>;
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

  return Object.freeze({
    ensure(userId: string): Promise<boolean> {
      if (!enabled) return Promise.resolve(false);
      if (current?.userId === userId) return current.promise;

      let guarded!: Promise<boolean>;
      guarded = request("/v1/credentials/local-claim", {
        method: "POST",
        credentials: "same-origin",
      }).then(async (response) => {
        await response.body?.cancel();
        if (!response.ok) {
          throw new Error(`Local development credential claim failed (HTTP ${response.status})`);
        }
        return true;
      }).catch((error) => {
        if (current?.promise === guarded) current = undefined;
        throw error;
      });
      current = { userId, promise: guarded };
      return guarded;
    },
  });
}

export const localDevelopmentCredential = createLocalDevelopmentCredentialResource();
