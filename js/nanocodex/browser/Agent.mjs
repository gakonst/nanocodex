import {
  createManagedAgent,
  managedTransportOptions,
} from "../runtime/managed-transport.mjs";

/** Create the browser Agent in its package-owned module Worker. */
export function create(options = {}) {
  if (managedTransportOptions(options?.transport) && options.requestPolicy !== undefined) {
    throw new TypeError('managed request policy must be configured by its owning host');
  }
  if (options.harness === 'claude') return import('./Claude.mjs').then(({ create }) => create(options));
  if (options.harness !== undefined && options.harness !== false && options.harness !== 'codex') throw new TypeError('unsupported harness family');
  if (managedTransportOptions(options?.transport)) {
    return createManagedAgent(options);
  }
  if (options.harnesses !== undefined || options.requestPolicy !== undefined) return import('./InlineAgent.mjs').then(({ create }) =>
    create(options.harness === false ? { ...options, harness: 'codex' } : options));
  return import("./WorkerAgent.mjs").then(({ createWorkerAgent }) =>
    createWorkerAgent(options));
}
