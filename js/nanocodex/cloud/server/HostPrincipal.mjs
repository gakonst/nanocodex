const APP_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const OPAQUE_CLAIM = /^[^\u0000-\u001f\u007f]{1,512}$/;
const EXCHANGE_TOKEN = /^[A-Za-z0-9_-]{43}$/;
const EXCHANGE_RESOURCE_PREFIX = "urn:nanocodex:host-principal:exchange:";

export function create(parameters) {
  if (!parameters || typeof parameters !== "object") {
    throw new TypeError("HostPrincipal.create requires parameters");
  }
  if (!APP_ID.test(parameters.appId)) {
    throw new TypeError("HostPrincipal.create requires a valid appId");
  }
  if (typeof parameters.secret !== "string" || !/^\S{32,512}$/.test(parameters.secret)) {
    throw new TypeError("HostPrincipal.create requires a 32 to 512 character project secret");
  }
  const appOrigin = publicOrigin(parameters.appOrigin, "HostPrincipal.create appOrigin");
  const baseUrl = serviceBaseUrl(parameters.baseUrl ?? "https://api.nanocodex.xyz");
  const fetchFn = parameters.fetch ?? globalThis.fetch;
  if (typeof fetchFn !== "function") throw new TypeError("HostPrincipal.create requires fetch");

  const client = {
    async create(options) {
      const claims = principalClaims(options);
      const resources = principalResources(options?.resources);
      const expiresIn = exchangeExpiry(options?.expiresIn);
      const response = await fetchFn(new URL("/v1/host-principal/exchanges", baseUrl), {
        method: "POST",
        headers: serverHeaders(parameters.appId, parameters.secret),
        body: JSON.stringify({
          app_origin: appOrigin,
          issuer: claims.issuer,
          tenant: claims.tenant,
          subject: claims.subject,
          session_id: claims.sessionId,
          resources,
          ...(expiresIn === undefined ? {} : { expires_in: expiresIn }),
        }),
        signal: options?.signal,
      });
      const body = await response.json().catch(() => undefined);
      if (!response.ok) throw serviceError(response, body, "creation");
      return exchangeFromWire(body);
    },
    handler(options) {
      if (!options || typeof options.authenticate !== "function") {
        throw new TypeError("host principal handler requires authenticate");
      }
      return async function hostPrincipalExchange(request) {
        if (!request || request.method !== "POST") {
          return errorResponse(405, "Method not allowed.", { allow: "POST" });
        }
        const origin = request.headers?.get?.("origin");
        const fetchSite = request.headers?.get?.("sec-fetch-site");
        if (origin !== appOrigin || (fetchSite && fetchSite !== "same-origin")) {
          return errorResponse(403, "Origin not allowed.");
        }
        let requestBody;
        try {
          requestBody = await request.json();
        } catch {
          return errorResponse(400, "Invalid host principal request.");
        }
        if (!requestBody || typeof requestBody !== "object" || Array.isArray(requestBody)
          || Object.keys(requestBody).some((key) => key !== "resources")) {
          return errorResponse(400, "Invalid host principal request.");
        }
        let resources;
        try {
          resources = principalResources(requestBody.resources);
        } catch (error) {
          return errorResponse(400, error.message);
        }
        const identity = await options.authenticate(request);
        if (!identity) return errorResponse(401, "Authentication required.");
        const exchange = await client.create({ ...identity, resources });
        return Response.json({ token: exchange.token, expires_at: exchange.expiresAt }, {
          headers: { "cache-control": "no-store" },
        });
      };
    },
    async revoke(options) {
      const claims = principalClaims(options);
      const response = await fetchFn(new URL("/v1/host-principal/sessions", baseUrl), {
        method: "DELETE",
        headers: serverHeaders(parameters.appId, parameters.secret),
        body: JSON.stringify({
          app_origin: appOrigin,
          issuer: claims.issuer,
          tenant: claims.tenant,
          subject: claims.subject,
          session_id: claims.sessionId,
        }),
        signal: options?.signal,
      });
      if (!response.ok) {
        const body = await response.json().catch(() => undefined);
        throw serviceError(response, body, "revocation");
      }
      await response.body?.cancel();
    },
  };
  return Object.freeze(client);
}

function principalClaims(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)
    || !boundedOpaqueClaim(value.issuer) || !boundedOpaqueClaim(value.tenant)
    || !boundedOpaqueClaim(value.subject) || !boundedOpaqueClaim(value.sessionId)) {
    throw new TypeError("host principal requires bounded opaque issuer, tenant, subject, and sessionId");
  }
  return {
    issuer: value.issuer,
    tenant: value.tenant,
    subject: value.subject,
    sessionId: value.sessionId,
  };
}

function boundedOpaqueClaim(value) {
  return typeof value === "string" && OPAQUE_CLAIM.test(value);
}

function principalResources(value) {
  if (!Array.isArray(value) || value.length === 0 || value.length > 64
    || value.some((resource) => typeof resource !== "string" || resource.length === 0
      || resource.length > 512 || resource.startsWith(EXCHANGE_RESOURCE_PREFIX)
      || forbiddenAuthority(resource))) {
    throw new TypeError("host principal resources must be 1 through 64 bounded non-exchange strings");
  }
  return Object.freeze([...value]);
}

function forbiddenAuthority(resource) {
  const value = resource.toLowerCase();
  return value.startsWith("urn:nanocodex:mpp:")
    || value.startsWith("urn:nanocodex:access-key:")
    || value.startsWith("urn:nanocodex:access_key:")
    || value === "urn:nanocodex:authorize-access-key";
}

function exchangeExpiry(value) {
  if (value === undefined) return undefined;
  if (!Number.isSafeInteger(value) || value < 30 || value > 300) {
    throw new TypeError("host principal expiresIn must be between 30 and 300 seconds");
  }
  return value;
}

function exchangeFromWire(value) {
  const now = Math.floor(Date.now() / 1_000);
  if (!value || typeof value !== "object" || Array.isArray(value)
    || !EXCHANGE_TOKEN.test(value.token)
    || !Number.isSafeInteger(value.expires_at)
    || value.expires_at <= now || value.expires_at > now + 300) {
    throw new Error("Nanocodex returned an invalid host principal exchange.");
  }
  return Object.freeze({ token: value.token, expiresAt: value.expires_at });
}

function serverHeaders(appId, secret) {
  return {
    accept: "application/json",
    authorization: `Bearer ${secret}`,
    "content-type": "application/json",
    "x-nanocodex-app-id": appId,
  };
}

function serviceError(response, body, operation) {
  const error = new Error(
    body?.error?.message ?? `Nanocodex host principal ${operation} failed with ${response.status}.`,
  );
  error.status = response.status;
  return error;
}

function errorResponse(status, message, headers = {}) {
  return Response.json({ error: { message } }, {
    status,
    headers: { ...headers, "cache-control": "no-store" },
  });
}

function serviceBaseUrl(value) {
  const url = new URL(value);
  const origin = publicOrigin(url.origin, "HostPrincipal.create baseUrl");
  if (url.username || url.password || url.hash) {
    throw new TypeError("HostPrincipal.create baseUrl cannot contain credentials or a fragment");
  }
  return new URL(origin);
}

function publicOrigin(value, label) {
  if (typeof value !== "string") throw new TypeError(`${label} must be an exact origin`);
  const url = new URL(value);
  const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname.toLowerCase());
  if (url.origin !== value || (url.protocol !== "https:" && !(url.protocol === "http:" && loopback))
    || url.username || url.password) {
    throw new TypeError(`${label} must be an exact HTTPS or loopback origin`);
  }
  return url.origin;
}
