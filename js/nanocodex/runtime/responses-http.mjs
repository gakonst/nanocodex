/** Pull-based fetch ownership. Successful Responses bodies are never buffered. */
export function createResponsesHttp(open) {
  const requests = new Map();
  let nextHandle = 1;
  return Object.freeze({
    httpOpen(endpoint, apiKey, sessionId, metadata, body) {
      const handle = nextHandle++;
      const controller = new AbortController();
      const entry = { controller, reader: undefined, closed: false };
      requests.set(handle, entry);
      entry.ready = Promise.resolve().then(async () => {
        if (entry.closed) throw new Error("HTTPS request cancelled");
        const response = await open(endpoint, apiKey, sessionId, metadata, body, controller.signal);
        if (entry.closed) {
          await response.body?.cancel();
          throw new Error("HTTPS request cancelled");
        }
        if (!response.ok) {
          const body = await response.text();
          const delay = response.headers.get("retry-after");
          throw JSON.stringify({ kind: "handshake_rejected", status: response.status, body,
            ...(/^\d+$/.test(delay ?? "") ? { retry_after: Number(delay) } : {}) });
        }
        if (!response.body) throw new Error("HTTPS response omitted its streaming body");
        entry.reader = response.body.getReader();
        return JSON.stringify({ status: response.status,
          reasoning_included: response.headers.has("x-reasoning-included"),
          turn_state: response.headers.get("x-codex-turn-state") });
      });
      // Rust can drop the request before ever awaiting readiness.
      void entry.ready.catch(() => {});
      return handle;
    },
    httpReady(handle) { return required(handle).ready; },
    async httpNext(handle) {
      const entry = required(handle);
      await entry.ready;
      const { done, value } = await entry.reader.read();
      return done ? null : value;
    },
    httpClose(handle) {
      const entry = requests.get(handle);
      if (!entry) return;
      requests.delete(handle);
      entry.closed = true;
      entry.controller.abort();
      void entry.reader?.cancel().catch(() => {});
    },
    dispose() { for (const handle of requests.keys()) this.httpClose(handle); },
  });
  function required(handle) {
    const entry = requests.get(handle);
    if (!entry) throw new Error("unknown HTTPS handle");
    return entry;
  }
}

export function responsesHttpHeaders(apiKey, sessionId, metadata) {
  const threadId = metadata.threadId ?? sessionId;
  const headers = new Headers({ Authorization: `Bearer ${apiKey}`,
    "Content-Type": "application/json", Accept: "text/event-stream",
    "x-openai-internal-codex-responses-lite": "true", "session-id": sessionId,
    "thread-id": threadId, "x-client-request-id": threadId,
    "User-Agent": "nanocodex-js" });
  if (metadata.accountId) headers.set("ChatGPT-Account-ID", metadata.accountId);
  if (metadata.fedramp) headers.set("X-OpenAI-Fedramp", "true");
  if (metadata.turnState) headers.set("x-codex-turn-state", metadata.turnState);
  return headers;
}
