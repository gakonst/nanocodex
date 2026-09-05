const THREAD_ID = /^[a-f0-9]{8}-[a-f0-9]{4}-[1-5][a-f0-9]{3}-[89ab][a-f0-9]{3}-[a-f0-9]{12}$/;
const VAULT_ID = /^[A-Za-z0-9_-]{22,64}$/;
const VAULT_ID_HEADER = "x-nanocodex-vault-id";
const VAULT_PLACEHOLDER = /\{\{NANOCODEX_VAULT_(?:USERNAME|PASSWORD|API_KEY|BASIC|CARD_NUMBER|EXPIRY_MONTH|EXPIRY_YEAR|CVV|BILLING_ZIP)\}\}/;
const PRIVATE_HEADER = /(?:^|[-_])(?:auth(?:orization)?|cookie|credential|password|proxy|secret|token|api[-_]?key)(?:$|[-_])/i;
const FORBIDDEN_HEADERS = new Set([
  "connection", "host", "origin", "proxy-connection", "referer", "te", "trailer",
  "transfer-encoding", "upgrade", "x-nanocodex-subject",
]);
let installed;

/** Installs one thread-owned external fetch route for the current browser Worker isolate. */
export function installBrowserEgressFetch(options) {
  const origin = new URL(options?.origin).origin;
  if (installed) {
    if (installed.origin !== origin || installed.threadId !== options.threadId) {
      throw new Error("browser egress is already bound to a different thread");
    }
    return installed.fetch;
  }
  const routedFetch = createBrowserRuntimeFetch(options);
  globalThis.fetch = routedFetch;
  installed = Object.freeze({ fetch: routedFetch, origin, threadId: options.threadId });
  return routedFetch;
}

/** Creates the browser harness fetch boundary without mutating the caller's global fetch. */
export function createBrowserRuntimeFetch(options) {
  const origin = new URL(options?.origin).origin;
  const nativeFetch = (options.fetch ?? globalThis.fetch).bind(globalThis);
  const externalFetch = standardFetchFromEgress(createBrowserEgressFetch({
    fetch: nativeFetch,
    headers: options.headers,
    origin,
    threadId: options.threadId,
  }));
  return async (input, init) => {
    const request = input instanceof Request
      ? new Request(input, init)
      : new Request(new URL(String(input), origin), init);
    return new URL(request.url).origin === origin
      ? nativeFetch(request)
      : externalFetch(request);
  };
}

/** Creates the only workload-network capability installed in browser-side runtimes. */
export function createBrowserEgressFetch(options) {
  if (typeof options?.fetch !== "function") {
    throw new TypeError("browser egress requires a fetch implementation");
  }
  if (typeof options.origin !== "string" || !options.origin) {
    throw new TypeError("browser egress requires the application origin");
  }
  if (!THREAD_ID.test(options.threadId)) {
    throw new TypeError("browser egress requires a valid thread id");
  }
  const endpoint = new URL("/v1/egress", options.origin);
  return async (target, request = {}) => {
    const url = new URL(target);
    if (!["http:", "https:"].includes(url.protocol) || url.username || url.password) {
      throw new Error("browser egress supports only credential-free http:// and https:// URLs");
    }
    const headers = new Headers(request.headers);
    const vaultId = headers.get(VAULT_ID_HEADER);
    if (vaultId !== null && !VAULT_ID.test(vaultId)) {
      throw new Error("browser egress requires a valid Vault item id");
    }
    for (const [name, value] of headers) {
      const lower = name.toLowerCase();
      const privateVaultPlaceholder = vaultId !== null
        && PRIVATE_HEADER.test(name)
        && safeVaultHeaderValue(lower, value);
      if ((!privateVaultPlaceholder && PRIVATE_HEADER.test(name)) || FORBIDDEN_HEADERS.has(lower)
        || lower.startsWith("cf-") || lower.startsWith("forwarded")
        || lower.startsWith("sec-") || lower.startsWith("x-forwarded-")) {
        throw new Error(`browser egress does not accept credential or routing header '${name}'`);
      }
    }
    if (vaultId !== null && ![...headers.values()].some((value) => VAULT_PLACEHOLDER.test(value))
      && (typeof request.body !== "string" || !VAULT_PLACEHOLDER.test(request.body))) {
      throw new Error("browser egress Vault requests require a supported placeholder");
    }
    try {
      const gatewayHeaders = new Headers(options.headers);
      gatewayHeaders.set("content-type", "application/json");
      const response = await options.fetch(endpoint, {
        method: "POST",
        headers: gatewayHeaders,
        body: JSON.stringify({
          thread_id: options.threadId,
          url: url.href,
          method: request.method ?? "GET",
          headers: Object.fromEntries(headers.entries()),
          ...(request.body === undefined ? {} : { body: request.body }),
        }),
        credentials: "same-origin",
        redirect: "manual",
        signal: request.signal,
      });
      return {
        status: response.status,
        statusText: response.statusText,
        headers: Object.fromEntries(response.headers.entries()),
        body: new Uint8Array(await response.arrayBuffer()),
        url: url.href,
      };
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      throw new Error(`browser egress failed (${detail})`);
    }
  };
}

function safeVaultHeaderValue(name, value) {
  if (name === "cookie" || name === "proxy-authorization") return false;
  if (name === "authorization") {
    return value === "Basic {{NANOCODEX_VAULT_BASIC}}"
      || value === "Bearer {{NANOCODEX_VAULT_API_KEY}}"
      || value === "Bearer {{NANOCODEX_VAULT_PASSWORD}}";
  }
  return /^\{\{NANOCODEX_VAULT_(?:PASSWORD|API_KEY|BASIC|CARD_NUMBER|EXPIRY_MONTH|EXPIRY_YEAR|CVV|BILLING_ZIP)\}\}$/.test(value);
}

/** Adapts the shell result to the standard Fetch API used inside browser Workers. */
export function standardFetchFromEgress(secureFetch) {
  return async (input, init = {}) => {
    const request = new Request(input, init);
    const method = request.method.toUpperCase();
    const result = await secureFetch(request.url, {
      method,
      headers: request.headers,
      ...(method === "GET" || method === "HEAD" ? {} : { body: await request.text() }),
      signal: request.signal,
    });
    return new Response(result.body, {
      status: result.status,
      statusText: result.statusText,
      headers: result.headers,
    });
  };
}
