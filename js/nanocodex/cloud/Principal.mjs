const EXCHANGE_TOKEN = /^[A-Za-z0-9_-]{43}$/;
const MAX_EXCHANGE_TTL_SECONDS = 300;

/** Reads a one-time host-principal exchange from an application-owned route. */
export function host(options = {}) {
  const fetchFn = options.fetch ?? globalThis.fetch;
  if (typeof fetchFn !== "function") throw new TypeError("host principal requires fetch");
  return Object.freeze({
    key: requiredString(options.key ?? "host-principal", "principal key"),
    name: requiredString(options.name ?? "Host application principal", "principal name"),
    type: "host",
    setup({ appId, appOrigin }) {
      const endpoint = principalEndpoint(
        options.url ?? "/api/nanocodex/host-principal",
        appOrigin,
      );
      return Object.freeze({
        async create({ resources, signal } = {}) {
          const exactResources = principalResources(resources);
          const response = await fetchFn(endpoint, {
            method: "POST",
            credentials: "include",
            headers: {
              accept: "application/json",
              "content-type": "application/json",
              "x-nanocodex-app-id": appId,
            },
            body: JSON.stringify({ resources: exactResources }),
            signal,
          });
          const body = await response.json().catch(() => undefined);
          if (!response.ok) {
            const error = new Error(
              body?.error?.message ?? `The host principal exchange failed with ${response.status}.`,
            );
            error.status = response.status;
            throw error;
          }
          return principalExchange(body);
        },
      });
    },
  });
}

function principalEndpoint(value, appOrigin) {
  let endpoint;
  try {
    endpoint = new URL(String(value), appOrigin);
  } catch {
    throw new TypeError("host principal url requires an absolute application origin");
  }
  if (!appOrigin || endpoint.origin !== appOrigin) {
    throw new TypeError("host principal url must stay on the embedding application's origin");
  }
  if (endpoint.username || endpoint.password || endpoint.hash) {
    throw new TypeError("host principal url cannot contain credentials or a fragment");
  }
  return endpoint;
}

function principalExchange(value) {
  const now = Math.floor(Date.now() / 1_000);
  if (!value || typeof value !== "object" || Array.isArray(value)
    || !EXCHANGE_TOKEN.test(value.token)
    || !Number.isSafeInteger(value.expires_at)
    || value.expires_at <= now
    || value.expires_at > now + MAX_EXCHANGE_TTL_SECONDS) {
    throw new TypeError("host principal returned an invalid exchange");
  }
  return Object.freeze({ token: value.token, expiresAt: value.expires_at });
}

function principalResources(value) {
  if (!Array.isArray(value) || value.length === 0 || value.length > 64
    || value.some((resource) => typeof resource !== "string"
      || resource.length === 0 || resource.length > 512)) {
    throw new TypeError("host principal resources must be 1 through 64 bounded strings");
  }
  return Object.freeze([...value]);
}

function requiredString(value, label) {
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${label} must be a non-empty string`);
  }
  return value;
}
