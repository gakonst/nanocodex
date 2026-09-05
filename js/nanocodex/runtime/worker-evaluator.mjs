const WORKER_PROTOCOL = "nanocodex.code-evaluator.v1";
let nextEvaluation = 1;

/** Creates the browser's per-cell, synchronously terminable Code Mode boundary. */
export function createWorkerEvaluator(options = {}) {
  const createWorker = options.createWorker ?? createCodeEvaluatorWorker;
  if (options.createWorker === undefined && typeof globalThis.Worker !== "function") {
    throw new TypeError("browser Code Mode isolation requires Worker");
  }
  if (typeof createWorker !== "function") {
    throw new TypeError("browser Code Mode isolation requires a Worker factory");
  }

  return (source, environment) => new Promise((resolve, reject) => {
    environment.signal?.throwIfAborted();
    const worker = createWorker();
    const evaluationId = nextEvaluation++;
    let closed = false;

    const close = () => {
      if (closed) return false;
      closed = true;
      environment.signal?.removeEventListener("abort", onAbort);
      worker.onmessage = null;
      worker.onerror = null;
      worker.onmessageerror = null;
      worker.terminate();
      return true;
    };
    const fail = (error) => {
      if (!close()) return;
      reject(error instanceof Error ? error : new Error(String(error)));
    };
    const onAbort = () => {
      const reason = environment.signal?.reason ?? new Error("Code Mode execution was cancelled");
      // AbortSignal dispatch is synchronous. Let every parent-held nested tool
      // observe cancellation before terminating its guest evaluator boundary.
      queueMicrotask(() => fail(reason));
    };

    worker.onmessage = ({ data }) => {
      if (closed || data?.protocol !== WORKER_PROTOCOL || data.evaluationId !== evaluationId) return;
      if (data.type === "tool.call") {
        const tool = environment.tools[data.name];
        Promise.resolve().then(() => {
          if (typeof tool !== "function") {
            throw new Error(`unknown application tool: ${data.name}`);
          }
          return tool(data.input);
        }).then(
          (value) => postToolResult(worker, evaluationId, data.id, true, value, () => closed),
          (error) => postToolResult(
            worker,
            evaluationId,
            data.id,
            false,
            cloneableError(error),
            () => closed,
          ),
        );
        return;
      }
      if (data.type === "output") {
        try {
          if (data.kind === "text") environment.text(data.value);
          else if (data.kind === "image") environment.image(data.value, data.detail);
          else if (data.kind === "generatedImage") environment.generatedImage(data.value);
          else throw new Error(`unknown Code Mode output kind: ${data.kind}`);
        } catch (error) {
          fail(error);
        }
        return;
      }
      if (data.type === "console") {
        const logger = typeof environment.console?.[data.level] === "function"
          ? environment.console[data.level]
          : environment.console?.log;
        logger?.(...data.values);
        return;
      }
      if (data.type !== "completed" && data.type !== "failed") {
        fail(new Error(`unknown Code Mode evaluator message: ${data.type}`));
        return;
      }
      try {
        for (const [key, value] of data.storedWrites ?? []) environment.store(key, value);
      } catch (error) {
        fail(error);
        return;
      }
      if (!close()) return;
      if (data.type === "completed") resolve();
      else reject(new Error(data.error || "Code Mode evaluator failed"));
    };
    worker.onerror = (event) => fail(new Error(event?.message || "Code Mode evaluator Worker failed"));
    worker.onmessageerror = () => fail(new Error("Code Mode evaluator Worker returned unreadable data"));
    environment.signal?.addEventListener("abort", onAbort, { once: true });
    try {
      worker.postMessage({
        protocol: WORKER_PROTOCOL,
        evaluationId,
        egress: options.egress,
        type: "evaluate",
        source,
        storedEntries: environment.storedEntries,
        toolDefinitions: environment.toolDefinitions,
        toolNames: Object.keys(environment.tools),
      });
    } catch (error) {
      fail(error);
    }
  });
}

// Vite recognizes only this literal Worker + URL shape as a worker entry. Keep
// the test factory above out of the production construction path so the
// evaluator and its imports are emitted as one runnable browser asset.
function createCodeEvaluatorWorker() {
  return new Worker(
    new URL("./code-evaluator.worker.mjs", import.meta.url),
    { name: "nanocodex-code-evaluator", type: "module" },
  );
}

function postToolResult(worker, evaluationId, id, ok, value, isClosed) {
  if (isClosed()) return;
  try {
    worker.postMessage({
      protocol: WORKER_PROTOCOL,
      evaluationId,
      type: "tool.result",
      id,
      ok,
      value,
    });
  } catch (error) {
    worker.postMessage({
      protocol: WORKER_PROTOCOL,
      evaluationId,
      type: "tool.result",
      id,
      ok: false,
      value: `tool result could not cross the evaluator Worker boundary: ${errorMessage(error)}`,
    });
  }
}

function cloneableError(error) {
  try {
    structuredClone(error);
    return error;
  } catch {
    return errorMessage(error);
  }
}

function errorMessage(error) {
  return error && (error.stack || error.message) ? error.stack || error.message : String(error);
}
