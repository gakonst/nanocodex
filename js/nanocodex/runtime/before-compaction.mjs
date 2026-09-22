// Awaited host effect barrier. The Rust execution policy owns replay receipts;
// the host callback owns idempotency if its commit precedes a lost Rust receipt.
export function createBeforeCompaction(callback, { timeoutMs = 30_000 } = {}) {
  if (callback !== undefined && typeof callback !== "function") {
    throw new TypeError("beforeCompaction must be a function");
  }
  const active = new Map();
  let disposed = false;
  function cancel(boundaryId) {
    active.get(boundaryId)?.abort(new Error("pre-compaction preservation cancelled"));
  }
  return {
    async preserve(request) {
      if (disposed) throw new Error("pre-compaction host is disposed");
      if (!callback) throw new Error("beforeCompaction callback is not configured");
      if (active.has(request.boundaryId)) throw new Error("compaction boundary is already active");
      const controller = new AbortController();
      active.set(request.boundaryId, controller);
      let timer;
      const interrupted = new Promise((_, reject) => {
        controller.signal.addEventListener("abort", () => reject(controller.signal.reason), { once: true });
        timer = setTimeout(() => controller.abort(new Error("pre-compaction preservation timed out")), timeoutMs);
      });
      try {
        const input = Object.freeze({ ...request,
          messages: Object.freeze(request.messages.map(message => Object.freeze({ ...message }))),
          signal: controller.signal });
        const receipt = await Promise.race([Promise.resolve().then(() => {
          controller.signal.throwIfAborted();
          return callback(input);
        }), interrupted]);
        controller.signal.throwIfAborted();
        if (!receipt || typeof receipt.receiptId !== "string" || !receipt.receiptId.trim()
          || new TextEncoder().encode(receipt.receiptId).byteLength > 256) {
          throw new TypeError("beforeCompaction must return a durable receiptId (1–256 UTF-8 bytes)");
        }
        return { receiptId: receipt.receiptId };
      } finally {
        clearTimeout(timer);
        active.delete(request.boundaryId);
      }
    },
    cancel,
    dispose() {
      disposed = true;
      for (const boundaryId of active.keys()) cancel(boundaryId);
    },
  };
}
