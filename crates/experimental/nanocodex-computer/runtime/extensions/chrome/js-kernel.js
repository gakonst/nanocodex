import { isKernelRequest, isKernelResponse, kernelError } from "./js-kernel-protocol.js";
import { runKernelWorker } from "./js-kernel-worker.js";

// A single abort invalidates this shared worker, including every queued cell.
export class KernelWorker {
  pending = new Map();
  nextRequestId = 0;
  worker = null;

  executeJs(script, signal) {
    if (signal?.aborted) return Promise.reject(signal.reason);
    const worker = this.getWorker();
    const id = String(this.nextRequestId++);
    return new Promise((resolve, reject) => {
      const request = { resolve, reject, signal };
      if (signal != null) {
        request.abort = () => this.restart(signal.reason);
        signal.addEventListener("abort", request.abort, { once: true });
      }
      this.pending.set(id, request);
      try {
        worker.postMessage({ id, script, type: "js-kernel:execute" });
      } catch (reason) {
        this.restart(reason);
      }
    });
  }

  getWorker() {
    if (this.worker !== null) return this.worker;
    const url = URL.createObjectURL(new Blob([`(${runKernelWorker.toString()})()`], { type: "text/javascript" }));
    try {
      const worker = new Worker(url);
      worker.addEventListener("message", ({ data }) => {
        if (isKernelResponse(data)) this.handleResponse(data);
      });
      worker.addEventListener("error", ({ message }) => {
        this.restart(new Error(message || "JavaScript worker failed"));
      });
      worker.addEventListener("messageerror", () => {
        this.restart(new Error("JavaScript worker returned an invalid result"));
      });
      this.worker = worker;
      return worker;
    } finally {
      URL.revokeObjectURL(url);
    }
  }

  handleResponse(response) {
    const request = this.pending.get(response.id);
    if (request == null) return;
    this.pending.delete(response.id);
    if (request.abort != null) request.signal?.removeEventListener("abort", request.abort);
    if (response.ok) {
      request.resolve(response.value);
    } else {
      const error = new Error(response.error?.message ?? "JavaScript execution failed");
      error.name = response.error?.name ?? "Error";
      if (response.error?.stack != null) error.stack = response.error.stack;
      request.reject(error);
    }
  }

  restart(reason) {
    this.worker?.terminate();
    this.worker = null;
    for (const [id, request] of this.pending) {
      this.pending.delete(id);
      if (request.abort != null) request.signal?.removeEventListener("abort", request.abort);
      request.reject(reason);
    }
  }

  reset() {
    this.restart(new Error("JavaScript kernel was reset"));
    return Promise.resolve();
  }
}

export function installKernelSandbox() {
  const kernel = new KernelWorker();
  const controllers = new Map();
  async function execute(request, port) {
    const controller = new AbortController();
    controllers.set(request.id, controller);
    try {
      const value = await kernel.executeJs(request.script, controller.signal);
      port.postMessage({ id: request.id, ok: true, value });
    } catch (reason) {
      port.postMessage({ id: request.id, ok: false, error: kernelError(reason) });
    } finally {
      controllers.delete(request.id);
      port.close();
    }
  }
  function cancel(request, port) {
    controllers.get(request.id)?.abort();
    port.postMessage({ id: request.id, ok: true });
    port.close();
  }
  async function reset(request, port) {
    try {
      await kernel.reset();
      port.postMessage({ id: request.id, ok: true });
    } catch (reason) {
      port.postMessage({ id: request.id, ok: false, error: kernelError(reason) });
    } finally {
      port.close();
    }
  }
  const handlers = { "js-kernel:execute": execute, "js-kernel:cancel": cancel, "js-kernel:reset": reset };
  window.addEventListener("message", (event) => {
    if (event.source !== window.parent || !isKernelRequest(event.data)) return;
    const port = event.ports[0];
    if (port != null) handlers[event.data.type](event.data, port);
  });
  window.parent.postMessage({ type: "js-kernel:ready" }, "*");
}

installKernelSandbox();
