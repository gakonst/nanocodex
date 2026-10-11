// Node-side fetch for Miniflare journeys. Import it in place of the global fetch:
//   import { fetch } from "./support/miniflare-fetch.mjs";
//
// workerd can answer a request before reading its body, for example with an early
// 401/403/400/404/405, and then close the HTTP/1.1 connection without sending
// "Connection: close". undici still treats that socket as reusable and can dispatch the
// next request on it before it sees the FIN. That request then fails with "fetch failed"
// ("other side closed" or ECONNRESET). See #957.
//
// So a body-bearing request to a loopback http: (local Miniflare) URL asks undici not to reuse its
// socket. Everything else goes to the global fetch unchanged: GET and HEAD (SSE, WebSocket
// upgrades, keep-alive), non-loopback or non-http: URLs, inputs whose URL cannot be parsed, and requests
// that already set an explicit Connection header. Nothing is retried, so a request that fails
// still fails the journey.
const loopback = hostname => hostname === "localhost" || hostname === "[::1]" || /^127\.\d+\.\d+\.\d+$/.test(hostname);

export function fetch(input, init = {}) {
  const request = input instanceof Request ? input : undefined;
  const method = String(init?.method ?? request?.method ?? "GET").toUpperCase();
  if (method === "GET" || method === "HEAD") return globalThis.fetch(input, init);
  let url;
  try { url = new URL(request ? request.url : input instanceof URL ? input.href : String(input)); } catch { return globalThis.fetch(input, init); }
  if (url.protocol !== "http:" || !loopback(url.hostname)) return globalThis.fetch(input, init);
  // Init headers replace a Request's headers, exactly as the global fetch treats them.
  const headers = new Headers(init?.headers ?? request?.headers);
  if (headers.has("connection")) return globalThis.fetch(input, init);
  headers.set("connection", "close");
  return globalThis.fetch(input, { ...init, headers });
}
