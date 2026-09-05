import { createServer, type Server } from "node:http";
import { readFile } from "node:fs/promises";

const WEB_ROOT = new URL("../web/", import.meta.url);
const SECURITY_HEADERS = {
  "cross-origin-opener-policy": "same-origin",
  "permissions-policy": "camera=(), microphone=(), geolocation=()",
  "referrer-policy": "no-referrer",
  "x-content-type-options": "nosniff",
  "x-frame-options": "DENY",
};

export async function startWebClient(options: { host?: string; port?: number } = {}) {
  const [html, script, css] = await Promise.all([
    readFile(new URL("index.html", WEB_ROOT)),
    readFile(new URL("dist/app.js", WEB_ROOT)),
    readFile(new URL("app.css", WEB_ROOT)),
  ]);
  const server = createServer((request, response) => {
    const pathname = new URL(request.url ?? "/", "http://127.0.0.1").pathname;
    if (request.method !== "GET" && request.method !== "HEAD") {
      send(response, 405, "text/plain; charset=utf-8", Buffer.from("method not allowed\n"));
      return;
    }
    if (pathname === "/" || pathname === "/index.html") {
      send(response, 200, "text/html; charset=utf-8", html, {
        "cache-control": "no-store",
        "content-security-policy": "default-src 'none'; connect-src http: https: ws: wss:; script-src 'self'; style-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
      }, request.method === "HEAD");
    } else if (pathname === "/app.js" || pathname === "/dist/app.js") {
      send(response, 200, "text/javascript; charset=utf-8", script, {
        "cache-control": "public, max-age=3600",
      }, request.method === "HEAD");
    } else if (pathname === "/app.css") {
      send(response, 200, "text/css; charset=utf-8", css, {
        "cache-control": "public, max-age=3600",
      }, request.method === "HEAD");
    } else if (pathname === "/health") {
      send(response, 200, "application/json; charset=utf-8", Buffer.from('{"status":"ok"}\n'));
    } else {
      send(response, 404, "text/plain; charset=utf-8", Buffer.from("not found\n"));
    }
  });
  // Rivet's local engine owns 6420 and its internal control plane owns 6421.
  await listen(server, options.host ?? "127.0.0.1", options.port ?? 6422);
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("web client did not bind TCP");
  return {
    url: `http://${address.address.includes(":") ? `[${address.address}]` : address.address}:${address.port}`,
    close: () => new Promise<void>((resolve, reject) => {
      server.close((error) => error ? reject(error) : resolve());
    }),
  };
}

function listen(server: Server, host: string, port: number): Promise<void> {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(port, host, resolve);
  });
}

function send(
  response: import("node:http").ServerResponse,
  status: number,
  contentType: string,
  body: Buffer,
  headers: Record<string, string> = {},
  head = false,
): void {
  response.writeHead(status, {
    ...SECURITY_HEADERS,
    ...headers,
    "content-length": String(body.byteLength),
    "content-type": contentType,
  });
  response.end(head ? undefined : body);
}
