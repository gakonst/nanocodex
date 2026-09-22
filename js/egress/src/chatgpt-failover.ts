/** Only explicit subscription exhaustion qualifies; a generic 429 may be an RPM limit. */
export function chatGptLimitReset(value: unknown, retryAfter?: string | null, now = Date.now()): number | undefined {
  if (!record(value)) return undefined;
  const response = record(value.response) ? value.response : undefined;
  const error = record(value.error) ? value.error : response && record(response.error) ? response.error : value;
  if (![error.code, error.type].some((code) => code === "usage_limit_reached"
    || code === "usage_limit_exceeded" || code === "insufficient_quota")) return undefined;
  const reset = positive(error.resets_at) ?? positive(error.reset_at);
  const delay = positive(error.resets_in_seconds) ?? positive(error.retry_after);
  const headerDelay = retryAfter && /^\d+(\.\d+)?$/.test(retryAfter) ? Number(retryAfter) * 1_000 : undefined;
  const headerDate = retryAfter ? Date.parse(retryAfter) : NaN;
  const until = reset !== undefined ? reset * 1_000
    : delay !== undefined ? now + delay * 1_000
    : headerDelay !== undefined ? now + headerDelay
    : Number.isFinite(headerDate) ? headerDate : undefined;
  // If the provider omits a reset, probe again after one minute, never spin.
  return Math.ceil(until !== undefined && until > now ? until : now + 60_000);
}

function positive(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : undefined;
}

function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Keep provider checkpoints inside their account. The SDK's server_error recovery
 * reconnects and replays full history rather than reusing previous_response_id. */
export function chatGptFailoverSocket(
  upstreamResponse: Response,
  headers: Headers,
  switchAccount: (resetAt: number) => Promise<boolean>,
  ctx?: Pick<ExecutionContext, "waitUntil">,
): Response {
  const upstream = upstreamResponse.webSocket;
  if (!upstream) throw new Error("ChatGPT WebSocket upgrade missing");
  const [client, server] = Object.values(new WebSocketPair());
  upstream.accept();
  server.accept();
  let closed = false;
  let pending = 0;
  let outputStarted = false;
  let tail = Promise.resolve();
  const close = (code = 1011, reason = "ChatGPT connection closed") => {
    if (closed) return;
    closed = true;
    for (const socket of [server, upstream]) {
      if (socket.readyState === WebSocket.OPEN) {
        try { socket.close(code === 1005 || code === 1006 ? 1011 : code, reason); } catch { socket.close(1011); }
      }
    }
  };
  const enqueue = (operation: () => Promise<void>) => {
    tail = tail.then(operation).catch(() => close());
    ctx?.waitUntil(tail);
  };
  server.addEventListener("message", (event) => {
    if (closed) return;
    if (typeof event.data === "string") {
      try {
        const frame = JSON.parse(event.data);
        if (frame.type === "response.create") {
          if (pending === 0) outputStarted = false;
          pending += 1;
        }
      } catch { /* Let the provider validate the frame. */ }
    }
    if (upstream.readyState === WebSocket.OPEN) upstream.send(event.data);
  });
  upstream.addEventListener("message", (event) => enqueue(async () => {
    if (closed) return;
    if (typeof event.data === "string" && event.data.length <= 64 * 1024) {
      let frame: unknown;
      try { frame = JSON.parse(event.data); } catch { /* Pass through opaque frames. */ }
      if (record(frame)) {
        const terminal = frame.type === "error" || frame.type === "response.failed"
          || frame.type === "response.completed" || frame.type === "response.incomplete";
        const resetAt = terminal ? chatGptLimitReset(frame) : undefined;
        if (resetAt !== undefined) {
          const switched = await switchAccount(resetAt);
          if (closed) return;
          if (switched && pending === 1 && !outputStarted) {
            server.send(JSON.stringify({ type: "error", error: {
              type: "server_error", code: "server_error", retry_after: 0,
              message: "ChatGPT account switched after reaching its subscription limit. Reconnect and retry with full history.",
            } }));
            close(1012, "ChatGPT account switched");
            return;
          }
        }
        if (terminal) pending = Math.max(0, pending - 1);
        else if (typeof frame.type === "string" && frame.type.startsWith("response.")
          && frame.type !== "response.created" && frame.type !== "response.in_progress") outputStarted = true;
      }
    } else {
      outputStarted = true;
    }
    if (server.readyState === WebSocket.OPEN) server.send(event.data);
  }));
  server.addEventListener("close", (event) => close(event.code, event.reason));
  server.addEventListener("error", () => close());
  upstream.addEventListener("close", (event) => enqueue(async () => close(event.code, event.reason)));
  upstream.addEventListener("error", () => enqueue(async () => close()));
  return new Response(null, { status: 101, headers, webSocket: client });
}
