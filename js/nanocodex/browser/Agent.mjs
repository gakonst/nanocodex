import {
  createManagedAgent,
  managedTransportOptions,
} from "../runtime/managed-transport.mjs";

/** Create the browser Agent in its package-owned module Worker. */
export function create(options = {}) {
  if (managedTransportOptions(options?.transport)) return createManagedAgent(options);
  return import("./WorkerAgent.mjs").then(({ createWorkerAgent }) =>
    createWorkerAgent(options));
}
