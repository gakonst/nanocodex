import { Worker as NodeWorker } from "node:worker_threads";

const bootstrapUrl = new URL("./node-web-worker-bootstrap.mjs", import.meta.url);

/** A minimal browser Worker facade backed by a real Node module Worker. */
export class NodeWebWorker {
  constructor(url, options = {}) {
    this.onmessage = null;
    this.onerror = null;
    this.onmessageerror = null;
    this.worker = new NodeWorker(bootstrapUrl, {
      name: options.name,
      type: "module",
      workerData: { target: String(url) },
    });
    this.worker.on("message", (data) => this.onmessage?.({ data }));
    this.worker.on("error", (error) => this.onerror?.(error));
    this.worker.on("messageerror", (error) => this.onmessageerror?.(error));
  }

  postMessage(value, transfer) {
    this.worker.postMessage(value, transfer);
  }

  terminate() {
    void this.worker.terminate();
  }
}
