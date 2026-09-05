const DEFAULT_TIMEOUT_MS = 10_000;

/** Open Nanocodex's same-origin Responses proxy and consume its setup frame. */
export function openHostManagedWebSocket(endpoint, sessionId, options = {}) {
  if (typeof sessionId !== "string" || !sessionId) {
    throw new TypeError("host-managed WebSocket requires a session ID");
  }
  const WebSocketImpl = options.WebSocketImpl ?? globalThis.WebSocket;
  if (typeof WebSocketImpl !== "function") {
    throw new Error("WebSocket is unavailable in this runtime");
  }
  const socketUrl = resolveWebSocketUrl(endpoint);
  socketUrl.searchParams.set("session_id", sessionId);
  socketUrl.searchParams.set("thread_id", options.threadId ?? sessionId);
  return waitForProxyHandshake(
    new WebSocketImpl(socketUrl),
    options.timeoutMs ?? DEFAULT_TIMEOUT_MS,
  );
}

export function defaultHostManagedWebSocketUrl(location = globalThis.location) {
  if (!location?.href) {
    throw new Error("host-managed transport requires websocketUrl outside a browser location");
  }
  const url = new URL("/api/responses", location.href);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return url.href;
}

function resolveWebSocketUrl(endpoint) {
  const base = globalThis.location?.href;
  const url = base === undefined ? new URL(endpoint) : new URL(endpoint, base);
  if (url.protocol !== "ws:" && url.protocol !== "wss:") {
    throw new TypeError("host-managed websocketUrl must use ws: or wss:");
  }
  return url;
}

function waitForProxyHandshake(socket, timeoutMs) {
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0) {
    socket.close();
    throw new TypeError("host-managed WebSocket timeout must be a positive integer");
  }
  return new Promise((resolve, reject) => {
    let settled = false;
    const timer = setTimeout(
      () => fail(new Error("Agent connection timed out")),
      timeoutMs,
    );
    const cleanup = () => {
      clearTimeout(timer);
      socket.removeEventListener("message", onMessage);
      socket.removeEventListener("error", onError);
      socket.removeEventListener("close", onClose);
    };
    const fail = (error) => {
      if (settled) return;
      settled = true;
      cleanup();
      socket.close();
      reject(error);
    };
    const onMessage = (event) => {
      const message = parseHandshake(event.data);
      if (message?.type === "nanocodex.proxy.ready") {
        settled = true;
        cleanup();
        resolve(socket);
        return;
      }
      if (message?.type === "nanocodex.proxy.rejected"
        && Number.isInteger(message.status)
        && message.status >= 100
        && message.status <= 599) {
        const status = message.status;
        const body = typeof message.error === "string" ? message.error : `HTTP ${status}`;
        const retryAfter = Number(message.retryAfter);
        fail(Object.assign(
          new Error(`Agent connection rejected with HTTP ${status}: ${body}`),
          {
            status,
            body,
            ...(Number.isFinite(retryAfter) && retryAfter >= 0 ? { retryAfter } : {}),
          },
        ));
        return;
      }
      fail(new Error("Agent connection returned an invalid handshake"));
    };
    const onError = () => fail(new Error("WebSocket connection failed"));
    const onClose = (event) => fail(
      new Error(`WebSocket closed during connection with code ${event.code}`),
    );
    socket.addEventListener("message", onMessage);
    socket.addEventListener("error", onError);
    socket.addEventListener("close", onClose);
  });
}

function parseHandshake(data) {
  if (typeof data !== "string") return undefined;
  try {
    const value = JSON.parse(data);
    return typeof value === "object" && value !== null && !Array.isArray(value)
      ? value
      : undefined;
  } catch {
    return undefined;
  }
}
