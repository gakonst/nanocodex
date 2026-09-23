/** One construction-owned socket, transferred only to the matching root host. */
export function prepareConnection(endpoint, sessionId, signal) {
  signal?.throwIfAborted();
  let transferred = false;
  let disposed = false;
  let opened;
  let closing;
  let rejectDisposed;
  const disposedPromise = new Promise((_, reject) => { rejectDisposed = reject; });
  const opening = Promise.resolve().then(() => {
    signal?.throwIfAborted();
    return endpoint.createWebSocket(endpoint.websocketUrl, sessionId, { authorization: "preconnect" });
  }).then(value => {
    opened = value;
    if (disposed) {
      void close().catch(() => {});
      throw new Error("Cloudflare Agent preparation was disposed");
    }
    return value;
  });
  const promise = Promise.race([opening, disposedPromise]);
  void promise.catch(() => {});
  const abort = () => { void dispose().catch(() => {}); };
  signal?.addEventListener("abort", abort, { once: true });
  return { take, dispose };

  function take(url, id, request) {
    if (disposed || transferred || url !== endpoint.websocketUrl || id !== sessionId
      || (request.threadId ?? id) !== sessionId || request.authorization !== "preconnect") {
      throw new Error("Cloudflare Agent preparation does not match the root transport");
    }
    transferred = true;
    signal?.removeEventListener("abort", abort);
    return promise;
  }
  function close() {
    if (opened !== undefined) closing ??= Promise.resolve().then(() => opened.socket.close());
    return closing ?? Promise.resolve();
  }
  function dispose() {
    if (transferred) return Promise.resolve();
    if (!disposed) {
      disposed = true;
      signal?.removeEventListener("abort", abort);
      rejectDisposed(new Error("Cloudflare Agent preparation was disposed"));
    }
    return close();
  }
}
