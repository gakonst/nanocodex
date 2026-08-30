const APP_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const SUBJECT = /^[^\u0000-\u001f\u007f]{1,512}$/;

export function create(parameters) {
  if (!parameters || typeof parameters !== "object") {
    throw new TypeError("Session.create requires parameters");
  }
  if (!APP_ID.test(parameters.appId)) throw new TypeError("Session.create requires appId");
  if (typeof parameters.secret !== "string" || !/^\S{32,512}$/.test(parameters.secret)) {
    throw new TypeError("Session.create requires a 32 to 512 character project secret");
  }
  const appOrigin = publicOrigin(parameters.appOrigin);
  const baseUrl = new URL(parameters.baseUrl ?? "https://api.nanocodex.xyz");
  const fetchFn = parameters.fetch ?? globalThis.fetch;
  if (typeof fetchFn !== "function") throw new TypeError("Session.create requires fetch");

  const client = {
    async create(options) {
      if (!options || typeof options !== "object" || !SUBJECT.test(options.subject)) {
        throw new TypeError("identity session subject must be a bounded opaque string");
      }
      if (options.organization !== undefined && !SUBJECT.test(options.organization)) {
        throw new TypeError("identity session organization must be a bounded opaque string");
      }
      if (options.expiresIn !== undefined
        && (!Number.isSafeInteger(options.expiresIn)
          || options.expiresIn < 30
          || options.expiresIn > 300)) {
        throw new TypeError("identity session expiresIn must be between 30 and 300 seconds");
      }
      const response = await fetchFn(new URL("/v1/embed/sessions", baseUrl), {
        method: "POST",
        headers: {
          accept: "application/json",
          authorization: `Bearer ${parameters.secret}`,
          "content-type": "application/json",
          "x-nanocodex-app-id": parameters.appId,
        },
        body: JSON.stringify({
          app_origin: appOrigin,
          subject: options.subject,
          ...(options.organization === undefined ? {} : { organization: options.organization }),
          ...(options.expiresIn === undefined ? {} : { expires_in: options.expiresIn }),
        }),
        signal: options.signal,
      });
      const body = await response.json().catch(() => undefined);
      if (!response.ok) {
        throw new Error(
          body?.error?.message ?? `Nanocodex identity session creation failed with ${response.status}.`,
        );
      }
      if (!body || typeof body !== "object"
        || !/^[A-Za-z0-9_-]{43}$/.test(body.token)
        || !Number.isSafeInteger(body.expires_at)) {
        throw new Error("Nanocodex returned an invalid identity session.");
      }
      return Object.freeze({ token: body.token, expires_at: body.expires_at });
    },
    handler(options) {
      if (!options || typeof options.authenticate !== "function") {
        throw new TypeError("identity session handler requires authenticate");
      }
      return async function nanocodexIdentitySession(request) {
        if (!request || request.method !== "POST") {
          return Response.json({ error: { message: "Method not allowed." } }, {
            status: 405,
            headers: { allow: "POST", "cache-control": "no-store" },
          });
        }
        const origin = request.headers?.get?.("origin");
        const fetchSite = request.headers?.get?.("sec-fetch-site");
        if (origin !== appOrigin || (fetchSite && fetchSite !== "same-origin")) {
          return Response.json({ error: { message: "Origin not allowed." } }, {
            status: 403,
            headers: { "cache-control": "no-store" },
          });
        }
        const identity = await options.authenticate(request);
        if (!identity) {
          return Response.json({ error: { message: "Authentication required." } }, {
            status: 401,
            headers: { "cache-control": "no-store" },
          });
        }
        const session = await client.create(identity);
        return Response.json(session, { headers: { "cache-control": "no-store" } });
      };
    },
  };
  return Object.freeze(client);
}

function publicOrigin(value) {
  if (typeof value !== "string") throw new TypeError("Session.create requires appOrigin");
  const url = new URL(value);
  const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname.toLowerCase());
  if (url.origin !== value || (url.protocol !== "https:" && !(url.protocol === "http:" && loopback))
    || url.username || url.password) {
    throw new TypeError("Session.create appOrigin must be an exact HTTPS or loopback origin");
  }
  return url.origin;
}
