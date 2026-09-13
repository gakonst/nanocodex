// Self-contained because KernelWorker serializes this function into a blob.
// The browser's sandbox CSP applies to both the blob worker and its evaluation.
export function runKernelWorker() {
  let scope = { evaluate: globalThis.eval };
  let queue = Promise.resolve();
  globalThis.addEventListener("message", ({ data: { id, script } }) => {
    queue = queue.then(async () => {
      try {
        let candidate = scope;
        const cell = scope.evaluate(`(async function () {
arguments[0]({ evaluate(source) { return eval(source); } });
${script}
})`);
        await cell((nextScope) => { candidate = nextScope; });
        scope = candidate;
        globalThis.postMessage({ id, ok: true, value: "success" });
      } catch (reason) {
        const error = reason instanceof Error
          ? { message: reason.message, name: reason.name }
          : { message: typeof reason === "string" ? reason : "JavaScript execution failed", name: "Error" };
        if (reason instanceof Error && reason.stack != null) error.stack = reason.stack;
        globalThis.postMessage({ id, ok: false, error });
      }
    });
  });
}
