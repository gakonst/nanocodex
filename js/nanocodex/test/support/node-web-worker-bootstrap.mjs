import { parentPort, workerData } from "node:worker_threads";
import WebSocket from "ws";

import { NodeWebWorker } from "./node-web-worker.mjs";

const queued = [];
let onmessage;

Object.defineProperty(globalThis, "onmessage", {
  configurable: true,
  get: () => onmessage,
  set(value) {
    onmessage = value;
    while (queued.length && typeof onmessage === "function") {
      onmessage({ data: queued.shift() });
    }
  },
});
globalThis.postMessage = (value, transfer) => parentPort.postMessage(value, transfer);
globalThis.WebSocket = WebSocket;
globalThis.Worker = NodeWebWorker;

parentPort.on("message", (data) => {
  if (typeof onmessage === "function") onmessage({ data });
  else queued.push(data);
});

await import(workerData.target);
