const PRIVATE_HOST_SUFFIXES = [
  "internal", "invalid", "local", "localhost", "test", "home.arpa", "onion",
];
const DNS_LABEL = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/;

/**
 * Static registration validation only; this is not an SSRF-safe fetch boundary.
 * Delivery must use the public-only Workers fetch boundary, preserve TLS
 * hostname verification, and never follow redirects.
 * Webhook destinations are HTTPS URLs with a public DNS name (no IP literals).
 */
export function validateMcpWebhookUrl(value: unknown): string {
  if (typeof value !== "string" || value.length > 8_192
    || !/^https:\/\//i.test(value) || /[\s\\\u0000-\u001f\u007f]/.test(value)
    || value.includes("#")) {
    throw new Error("invalid_mcp_webhook_url");
  }
  let url: URL;
  try { url = new URL(value); }
  catch { throw new Error("invalid_mcp_webhook_url"); }
  const authority = value.slice(value.indexOf("://") + 3).split(/[/?]/, 1)[0]!;
  const hostname = url.hostname.toLowerCase().replace(/\.$/, "");
  const labels = hostname.split(".");
  if (url.protocol !== "https:" || url.username || url.password || authority.includes("@")
    || url.port === "0" || hostname.length > 253 || labels.length < 2 || labels.some(label => !DNS_LABEL.test(label))
    || /^[0-9.]+$/.test(hostname)
    || PRIVATE_HOST_SUFFIXES.some(suffix => hostname === suffix || hostname.endsWith(`.${suffix}`))) {
    throw new Error("invalid_mcp_webhook_url");
  }
  url.hostname = hostname;
  return url.href;
}

const MAX_REQUEST_BYTES = 64 * 1024;
const MAX_RESPONSE_BYTES = 4 * 1024;
const MAX_HEADER_BYTES = 16 * 1024;
const REQUEST_HEADERS = new Set([
  "content-type", "webhook-id", "webhook-timestamp", "webhook-signature", "x-mcp-subscription-id",
]);

/**
 * Sends one HTTPS POST through Workers' public Internet fetch boundary.
 * Requires global_fetch_strictly_public in every deployed environment to avoid
 * the legacy same-zone origin bypass. workerd must use a public-only network
 * service. Never use an origin/service binding or caller-provided Request.cf.
 * DNS routing and TLS hostname authentication are enforced by the runtime.
 */
export async function mcpWebhookFetch(request: Request, options: { readBody?: boolean } = {}): Promise<Response> {
  const url = validateMcpWebhookUrl(request.url);
  if (request.method !== "POST") throw new Error("mcp_webhook_method_denied");
  const headers = new Headers();
  let headerBytes = 0;
  for (const [name, value] of request.headers) {
    if (!REQUEST_HEADERS.has(name) || !/^[\x20-\x7e]*$/.test(value)) {
      throw new Error("mcp_webhook_header_denied");
    }
    headerBytes += name.length + value.length + 4;
    if (headerBytes > MAX_HEADER_BYTES) throw new Error("mcp_webhook_headers_too_large");
    headers.set(name, value);
  }
  const controller = new AbortController();
  const abort = () => controller.abort(request.signal.reason ?? new DOMException("Webhook aborted", "AbortError"));
  request.signal.addEventListener("abort", abort, { once: true });
  if (request.signal.aborted) abort();
  const timer = setTimeout(() => controller.abort(new DOMException("Webhook timed out", "TimeoutError")), 10_000);
  const interrupted = new Promise<never>((_, reject) => {
    const stop = () => reject(controller.signal.reason);
    controller.signal.addEventListener("abort", stop, { once: true });
    if (controller.signal.aborted) stop();
  });
  let response: Response | undefined;
  try {
    return await Promise.race([interrupted, (async () => {
      const body = await boundedStream(request.body, MAX_REQUEST_BYTES, controller.signal);
      controller.signal.throwIfAborted();
      // Construct fresh options: never inherit credentials, routing metadata,
      // Host, Authorization, cookies, or redirect policy from the input Request.
      response = await fetch(url, { method: "POST", headers, body,
        redirect: "manual", signal: controller.signal });
      controller.signal.throwIfAborted();
      const projected = new Headers();
      for (const name of ["content-type", "retry-after", "location"]) {
        const value = response.headers.get(name);
        if (value !== null) projected.set(name, value);
      }
      // Delivery needs only status/retry metadata. Preserve terminal statuses
      // even if the receiver attaches a huge or unending error body.
      const readBody = options.readBody === true && response.status < 300
        && response.status !== 204 && response.status !== 205;
      const bytes = readBody ? await boundedStream(response.body, MAX_RESPONSE_BYTES, controller.signal) : null;
      return new Response(bytes, { status: response.status, headers: projected });
    })()]);
  } finally {
    clearTimeout(timer);
    request.signal.removeEventListener("abort", abort);
    controller.abort();
    void response?.body?.cancel().catch(() => {});
  }
}

async function boundedStream(stream: ReadableStream<Uint8Array> | null, limit: number, signal: AbortSignal): Promise<Uint8Array<ArrayBuffer>> {
  if (!stream) return new Uint8Array();
  const reader = stream.getReader();
  const cancel = () => { void reader.cancel().catch(() => {}); };
  signal.addEventListener("abort", cancel, { once: true });
  const parts: Uint8Array[] = [];
  let size = 0;
  try {
    for (;;) {
      signal.throwIfAborted();
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > limit) throw new Error("mcp_webhook_body_too_large");
      parts.push(value);
    }
    signal.throwIfAborted();
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const part of parts) { bytes.set(part, offset); offset += part.byteLength; }
    return bytes;
  } finally {
    signal.removeEventListener("abort", cancel);
    void reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
