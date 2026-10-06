const PRIVATE_HOST_SUFFIXES = [
  "internal", "invalid", "local", "localhost", "test", "home.arpa", "onion",
];
const DNS_LABEL = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/;

/**
 * Static registration validation only; this is not an SSRF-safe fetch boundary.
 * Delivery must also validate DNS answers and pin the connection to a public
 * address, preserve TLS hostname verification, and never follow redirects.
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
const MAX_WIRE_BYTES = 64 * 1024;
const REQUEST_HEADERS = new Set([
  "content-type", "webhook-id", "webhook-timestamp", "webhook-signature", "x-mcp-subscription-id",
]);
type Socket = {
  readable: ReadableStream<Uint8Array>;
  writable: WritableStream<Uint8Array>;
  opened: Promise<unknown>;
  closed: Promise<void>;
  close(): Promise<void>;
  startTls(options: { expectedServerHostname: string }): Socket;
};
type SocketModule = {
  connect(address: { hostname: string; port: number }, options: { secureTransport: "starttls" }): Socket;
};

/**
 * Sends exactly one HTTPS POST. DNS is resolved only through the fixed trusted
 * resolver, and the socket connects to a validated numeric address, never a
 * hostname. TLS authenticates the original DNS name before HTTP bytes are sent.
 * Cloudflare may prohibit some public socket destinations (including its own
 * IP ranges); such failures must not fall back to ordinary fetch.
 */
export async function mcpWebhookFetch(request: Request, options: { readBody?: boolean } = {}): Promise<Response> {
  const url = new URL(validateMcpWebhookUrl(request.url));
  if (request.method !== "POST") throw new Error("mcp_webhook_method_denied");
  const headerLines: string[] = [];
  for (const [name, value] of request.headers) {
    if (!REQUEST_HEADERS.has(name) || !/^[\x20-\x7e]*$/.test(value)) {
      throw new Error("mcp_webhook_header_denied");
    }
    headerLines.push(`${name}: ${value}\r\n`);
  }
  const controller = new AbortController();
  let socket: Socket | undefined;
  const close = () => { void socket?.close().catch(() => {}); };
  const abort = () => controller.abort(request.signal.reason ?? new DOMException("Webhook aborted", "AbortError"));
  request.signal.addEventListener("abort", abort, { once: true });
  if (request.signal.aborted) abort();
  const timer = setTimeout(() => controller.abort(new DOMException("Webhook timed out", "TimeoutError")), 10_000);
  const interrupted = new Promise<never>((_, reject) => {
    const stop = () => { close(); reject(controller.signal.reason); };
    controller.signal.addEventListener("abort", stop, { once: true });
    if (controller.signal.aborted) stop();
  });
  try {
    return await Promise.race([interrupted, (async () => {
      const body = await boundedStream(request.body, MAX_REQUEST_BYTES, controller.signal);
      const head = `POST ${url.pathname}${url.search} HTTP/1.1\r\nHost: ${url.host}\r\n`
        + headerLines.join("") + `Content-Length: ${body.byteLength}\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n`;
      const encodedHead = new TextEncoder().encode(head);
      if (encodedHead.byteLength > MAX_HEADER_BYTES) throw new Error("mcp_webhook_headers_too_large");
      const addresses = await resolvePublicAddresses(url.hostname, controller.signal);
      controller.signal.throwIfAborted();
      // Dynamic loading keeps static URL validation usable outside Workers. The
      // module name is constant, with no caller-controlled loader or fallback.
      // @ts-expect-error This DOM-only package does not declare the Workers built-in module.
      const { connect } = await import("cloudflare:sockets") as SocketModule;
      controller.signal.throwIfAborted();
      const address = addresses[0]!;
      socket = connect({ hostname: address.includes(":") ? `[${address}]` : address, port: Number(url.port || 443) }, { secureTransport: "starttls" });
      void socket.closed.catch(() => {});
      await socket.opened;
      controller.signal.throwIfAborted();
      socket = socket.startTls({ expectedServerHostname: url.hostname });
      void socket.closed.catch(() => {});
      await socket.opened;
      controller.signal.throwIfAborted();
      const writer = socket.writable.getWriter();
      try {
        await writer.write(encodedHead);
        if (body.byteLength) await writer.write(body);
      } finally { writer.releaseLock(); }
      // No redirect processing: even a public Location is never contacted.
      return await readHttpResponse(socket.readable, options.readBody === true);
    })()]);
  } finally {
    clearTimeout(timer);
    request.signal.removeEventListener("abort", abort);
    controller.abort();
    close();
  }
}

async function resolvePublicAddresses(hostname: string, signal: AbortSignal): Promise<string[]> {
  const answers = await Promise.all(["A", "AAAA"].map(async type => {
    const resolver = new URL("https://cloudflare-dns.com/dns-query");
    resolver.searchParams.set("name", hostname);
    resolver.searchParams.set("type", type);
    const response = await fetch(resolver, {
      headers: { accept: "application/dns-json" }, redirect: "manual", signal,
    });
    if (!response.ok || response.redirected) {
      void response.body?.cancel().catch(() => {});
      throw new Error("mcp_webhook_dns_failed");
    }
    const bytes = await boundedStream(response.body, 32 * 1024, signal);
    const data: unknown = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
    if (!record(data) || data.Status !== 0 || data.TC === true
      || (data.Answer !== undefined && !Array.isArray(data.Answer))) {
      throw new Error("mcp_webhook_dns_failed");
    }
    const addresses: string[] = [];
    for (const answer of (data.Answer ?? []) as unknown[]) {
      if (!record(answer)) throw new Error("mcp_webhook_dns_failed");
      if (answer.type !== 1 && answer.type !== 28) continue;
      if (typeof answer.data !== "string") throw new Error("mcp_webhook_dns_failed");
      const address = publicAddress(answer.data, answer.type === 1 ? 4 : 6);
      if (!address) throw new Error("mcp_webhook_private_address");
      addresses.push(address);
    }
    return addresses;
  }));
  // Both families are checked, including every answer in mixed public/private
  // RRsets, before choosing one pinned address. No re-resolution on connect.
  const addresses = [...new Set(answers.flat())];
  if (!addresses.length) throw new Error("mcp_webhook_dns_no_addresses");
  return addresses;
}

function publicAddress(value: string, family: 4 | 6): string | undefined {
  if (family === 4) {
    if (!/^(?:0|[1-9]\d{0,2})(?:\.(?:0|[1-9]\d{0,2})){3}$/.test(value)) return;
    const [a, b, c, d] = value.split(".").map(Number) as [number, number, number, number];
    if ([a, b, c, d].some(part => part > 255)
      || a === 0 || a === 10 || a === 127 || a >= 224
      || (a === 100 && b >= 64 && b <= 127) || (a === 169 && b === 254)
      || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168)
      || (a === 192 && b === 0 && (c === 0 || c === 2))
      || (a === 192 && b === 88 && c === 99)
      || (a === 198 && (b === 18 || b === 19 || (b === 51 && c === 100)))
      || (a === 203 && b === 0 && c === 113)) return;
    return value;
  }
  // The DNS IPv6 syntax must be a literal, without zone IDs, brackets or an
  // embedded IPv4 spelling. URL canonicalization expands equivalent spellings.
  if (!/^[0-9a-f:]+$/i.test(value) || !value.includes(":")) return;
  let normalized: string;
  try { normalized = new URL(`https://[${value}]/`).hostname.slice(1, -1); }
  catch { return; }
  const halves = normalized.split("::");
  const left = halves[0] ? halves[0].split(":") : [];
  const right = halves[1] ? halves[1].split(":") : [];
  const groups = halves.length === 2
    ? [...left, ...Array(8 - left.length - right.length).fill("0"), ...right]
    : left;
  const [first, second] = groups.map(part => parseInt(part, 16));
  // Fail closed outside allocated global unicast, and on special-purpose,
  // documentation and transition prefixes (including Teredo and 6to4).
  if (groups.length !== 8 || first! < 0x2000 || first! > 0x3fff
    || (first === 0x2001 && (second! < 0x0200 || second === 0x0db8))
    || first === 0x2002 || (first === 0x3fff && second! < 0x1000)) return;
  return normalized;
}

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
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

/** Minimal bounded HTTP/1.x reader; never delegates redirects to an HTTP client. */
async function readHttpResponse(stream: ReadableStream<Uint8Array>, readBody: boolean): Promise<Response> {
  const reader = stream.getReader();
  let pending = new Uint8Array(0), wireBytes = 0, headerBytes = 0, readingBody = false;
  const more = async (): Promise<boolean> => {
    const { done, value } = await reader.read();
    if (done) return false;
    wireBytes += value.byteLength;
    if (readingBody && wireBytes > MAX_WIRE_BYTES) throw new Error("mcp_webhook_response_too_large");
    // A terminal status must survive an arbitrarily large accompanying body.
    // Retain only a bounded prefix until the headers establish the body policy.
    const retained = value.subarray(0, MAX_WIRE_BYTES - pending.length);
    const next = new Uint8Array(pending.length + retained.length);
    next.set(pending); next.set(retained, pending.length); pending = next;
    return true;
  };
  const take = async (size: number): Promise<Uint8Array<ArrayBuffer>> => {
    while (pending.length < size) if (!await more()) throw new Error("mcp_webhook_response_truncated");
    const value = pending.slice(0, size); pending = pending.slice(size); return value;
  };
  const line = async (): Promise<string> => {
    for (;;) {
      for (let i = 0; i + 1 < pending.length; i++) {
        if (pending[i] !== 13 || pending[i + 1] !== 10) continue;
        headerBytes += i + 2;
        if (headerBytes > MAX_HEADER_BYTES) throw new Error("mcp_webhook_response_headers_too_large");
        const value = pending.slice(0, i); pending = pending.slice(i + 2);
        if (value.some(byte => (byte < 32 && byte !== 9) || byte > 126)) throw new Error("mcp_webhook_invalid_response");
        return new TextDecoder().decode(value);
      }
      if (headerBytes + pending.length > MAX_HEADER_BYTES) throw new Error("mcp_webhook_response_headers_too_large");
      if (!await more()) throw new Error("mcp_webhook_response_truncated");
    }
  };
  const header = (value: string): [string, string] => {
    const match = /^([!#$%&'*+.^_`|~0-9A-Za-z-]+):[ \t]*(.*)$/.exec(value);
    if (!match) throw new Error("mcp_webhook_invalid_response");
    return [match[1]!.toLowerCase(), match[2]!.trim()];
  };
  try {
    let status = 0, headers = new Headers();
    for (let interim = 0; interim <= 4; interim++) {
      const match = /^HTTP\/1\.[01] ([1-5][0-9]{2})(?: [\x20-\x7e]*)?$/.exec(await line());
      if (!match) throw new Error("mcp_webhook_invalid_response");
      status = Number(match[1]); headers = new Headers();
      for (;;) {
        const value = await line(); if (!value) break;
        const [name, content] = header(value);
        if ((name === "content-length" || name === "transfer-encoding") && headers.has(name)) throw new Error("mcp_webhook_invalid_response");
        headers.append(name, content);
      }
      if (status >= 200) break;
      if (status === 101 || interim === 4) throw new Error("mcp_webhook_invalid_response");
    }
    // Preserve only headers relevant to verification/retry. Never project
    // cookies or hop-by-hop framing onto the materialized response.
    const projected = new Headers();
    for (const name of ["content-type", "retry-after", "location"]) {
      const value = headers.get(name); if (value !== null) projected.set(name, value);
    }
    if (!readBody || status === 204 || status === 205 || status >= 300) {
      return new Response(null, { status, headers: projected });
    }
    readingBody = true;
    if (wireBytes > MAX_WIRE_BYTES) throw new Error("mcp_webhook_response_too_large");
    const length = headers.get("content-length"), transfer = headers.get("transfer-encoding");
    if ((length !== null && transfer !== null) || (transfer !== null && transfer.toLowerCase() !== "chunked")
      || (length !== null && (!/^\d+$/.test(length) || !Number.isSafeInteger(Number(length))))
      || (headers.has("content-encoding") && headers.get("content-encoding")!.toLowerCase() !== "identity")) {
      throw new Error("mcp_webhook_invalid_response");
    }
    const parts: Uint8Array[] = [];
    let size = 0;
    const append = (bytes: Uint8Array) => {
      size += bytes.byteLength;
      if (size > MAX_RESPONSE_BYTES) throw new Error("mcp_webhook_response_too_large");
      parts.push(bytes);
    };
    if (transfer !== null) {
      for (;;) {
        const chunk = /^([0-9a-f]+)(?:;[\x20-\x7e]*)?$/i.exec(await line());
        if (!chunk) throw new Error("mcp_webhook_invalid_response");
        const count = parseInt(chunk[1]!, 16);
        if (!Number.isSafeInteger(count) || count > MAX_RESPONSE_BYTES - size) throw new Error("mcp_webhook_response_too_large");
        if (count === 0) {
          for (;;) { const trailer = await line(); if (!trailer) break; header(trailer); }
          break;
        }
        append(await take(count));
        const end = await take(2);
        if (end[0] !== 13 || end[1] !== 10) throw new Error("mcp_webhook_invalid_response");
      }
    } else if (length !== null) {
      if (Number(length) > MAX_RESPONSE_BYTES) throw new Error("mcp_webhook_response_too_large");
      append(await take(Number(length)));
    } else {
      for (;;) {
        append(pending); pending = new Uint8Array(0);
        if (!await more()) break;
      }
    }
    const body = new Uint8Array(size);
    let offset = 0;
    for (const part of parts) { body.set(part, offset); offset += part.byteLength; }
    return new Response(body, { status, headers: projected });
  } finally {
    void reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
