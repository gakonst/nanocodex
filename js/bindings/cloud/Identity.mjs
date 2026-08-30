const SESSION_TOKEN = /^[A-Za-z0-9_-]{43}$/;

export function from(parameters) {
  if (!parameters || typeof parameters !== "object") {
    throw new TypeError("Identity.from requires parameters");
  }
  if (typeof parameters.setup !== "function") {
    throw new TypeError("Identity.from requires setup");
  }
  return Object.freeze({
    key: requiredString(parameters.key, "identity key"),
    name: requiredString(parameters.name, "identity name"),
    type: requiredString(parameters.type, "identity type"),
    setup: parameters.setup,
  });
}

/**
 * Reads a short-lived Nanocodex identity session from an application-owned,
 * same-origin route. The route authenticates with Auth0, Better Auth, Privy,
 * or any other host session and mints the opaque session server-to-server.
 */
export function host(options = {}) {
  const fetchFn = options.fetch ?? globalThis.fetch;
  if (typeof fetchFn !== "function") throw new TypeError("host identity requires fetch");
  return from({
    key: options.key ?? "host-session",
    name: options.name ?? "Host application session",
    type: "host",
    setup({ appId, appOrigin }) {
      const endpoint = identityEndpoint(options.url ?? "/api/nanocodex/session", appOrigin);
      return Object.freeze({
        async getSession({ signal } = {}) {
          const response = await fetchFn(endpoint, {
            method: "POST",
            credentials: "include",
            headers: {
              accept: "application/json",
              "x-nanocodex-app-id": appId,
            },
            signal,
          });
          const body = await response.json().catch(() => undefined);
          if (!response.ok) {
            throw new Error(
              body?.error?.message ?? `The host identity session failed with ${response.status}.`,
            );
          }
          return identitySession(body);
        },
      });
    },
  });
}

/** Advanced adapter for frameworks that already expose a safe opaque session. */
export function custom(parameters) {
  if (!parameters || typeof parameters.getSession !== "function") {
    throw new TypeError("custom identity requires getSession");
  }
  return from({
    key: parameters.key ?? "custom-session",
    name: parameters.name ?? "Custom host session",
    type: "custom",
    setup(context) {
      return Object.freeze({
        async getSession(options = {}) {
          return identitySession(await parameters.getSession({ ...context, ...options }));
        },
      });
    },
  });
}

function identityEndpoint(value, appOrigin) {
  let endpoint;
  try {
    endpoint = new URL(String(value), appOrigin);
  } catch {
    throw new TypeError("host identity url requires an absolute application origin");
  }
  if (!appOrigin || endpoint.origin !== appOrigin) {
    throw new TypeError("host identity url must stay on the embedding application's origin");
  }
  if (endpoint.username || endpoint.password || endpoint.hash) {
    throw new TypeError("host identity url cannot contain credentials or a fragment");
  }
  return endpoint;
}

function identitySession(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || !SESSION_TOKEN.test(value.token)
    || !Number.isSafeInteger(value.expires_at)
    || value.expires_at <= Math.floor(Date.now() / 1_000)) {
    throw new TypeError("host identity returned an invalid Nanocodex session");
  }
  return Object.freeze({ token: value.token, expiresAt: value.expires_at });
}

function requiredString(value, label) {
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${label} must be a non-empty string`);
  }
  return value;
}
