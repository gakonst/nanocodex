export type ConnectDialogProxyEnv = {
  NANOCODEX_CONNECT_DIALOG?: Fetcher;
};

const CONNECT_DIALOG_PREFIX = "/connect-dialog";
const CONNECT_DIALOG_FRAME_ANCESTORS = [
  "'self'",
  "http://nanocodex.localhost:*",
  "http://*.nanocodex.localhost:*",
  "https://nanocodex-connect-playground.gakonst.workers.dev",
  "chrome-extension://jpkimkgbgbpcaldbnhlhbkbadmpeffle",
].join(" ");

export function isConnectDialogPath(pathname: string): boolean {
  return pathname === CONNECT_DIALOG_PREFIX
    || pathname.startsWith(`${CONNECT_DIALOG_PREFIX}/`);
}

/**
 * Serves the isolated Connect application on the canonical Nanocodex origin.
 *
 * The bound Worker owns only static assets. Canonical account credentials are
 * intentionally withheld from it, while the path presented to its asset
 * binding has the public mount prefix removed.
 */
export async function routeConnectDialog(
  request: Request,
  env: ConnectDialogProxyEnv,
  url: URL,
): Promise<Response | undefined> {
  if (!isConnectDialogPath(url.pathname)) return undefined;
  if (request.method !== "GET" && request.method !== "HEAD") {
    return json({ error: "method_not_allowed" }, {
      headers: { allow: "GET, HEAD" },
      status: 405,
    });
  }
  if (!env.NANOCODEX_CONNECT_DIALOG) {
    return json({ error: "connect_dialog_unavailable" }, { status: 503 });
  }

  const upstreamUrl = new URL(url);
  upstreamUrl.pathname = url.pathname === CONNECT_DIALOG_PREFIX
    ? "/"
    : url.pathname.slice(CONNECT_DIALOG_PREFIX.length);
  const headers = new Headers(request.headers);
  headers.delete("authorization");
  headers.delete("cookie");
  headers.delete("proxy-authorization");

  try {
    const upstream = await env.NANOCODEX_CONNECT_DIALOG.fetch(new Request(upstreamUrl, {
      headers,
      method: request.method,
      redirect: "manual",
    }));
    return projectResponse(upstream, url, upstreamUrl, request.method);
  } catch (error) {
    console.error(JSON.stringify({
      type: "connect_dialog.backend_failure",
      path: url.pathname,
      error: error instanceof Error
        ? { name: error.name, message: error.message }
        : { name: typeof error, message: String(error) },
    }));
    return json({ error: "connect_dialog_unavailable" }, { status: 503 });
  }
}

function projectResponse(
  upstream: Response,
  publicUrl: URL,
  upstreamUrl: URL,
  method: string,
): Response {
  const headers = new Headers(upstream.headers);
  headers.delete("set-cookie");
  headers.delete("set-cookie2");
  headers.set("x-content-type-options", "nosniff");
  if (headers.get("content-type")?.startsWith("text/html")) {
    headers.set("content-security-policy", `frame-ancestors ${CONNECT_DIALOG_FRAME_ANCESTORS}`);
  }

  const location = headers.get("location");
  if (location) {
    const target = new URL(location, upstreamUrl);
    if (target.origin !== upstreamUrl.origin) {
      return json({ error: "connect_dialog_invalid_redirect" }, { status: 502 });
    }
    if (!isConnectDialogPath(target.pathname)) {
      target.pathname = target.pathname === "/"
        ? CONNECT_DIALOG_PREFIX
        : `${CONNECT_DIALOG_PREFIX}${target.pathname}`;
    }
    target.protocol = publicUrl.protocol;
    target.host = publicUrl.host;
    headers.set("location", target.href);
  }

  return new Response(method === "HEAD" ? null : upstream.body, {
    headers,
    status: upstream.status,
    statusText: upstream.statusText,
  });
}

function json(body: unknown, init: ResponseInit): Response {
  return Response.json(body, {
    ...init,
    headers: {
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      ...init.headers,
    },
  });
}
