import type { DefaultAgent } from "../types.mjs";
import type { create as createBrowserAgent } from "./Agent.mjs";

type WorkerAgentResourceOptions = Readonly<createBrowserAgent.Options & {
  /** Origin retained in the private browser harness resource identity. */
  origin?: string | undefined;
}>;
type WorkerAgentIdentityOptions = Pick<
  WorkerAgentResourceOptions,
  "accountConnectionRequests" | "module" | "origin" | "sessionId" | "threadId"
>;
type WorkerAgentPreparationOptions =
  | Readonly<WorkerAgentIdentityOptions & { harness: false }>
  | Readonly<WorkerAgentIdentityOptions & { harness?: undefined } & (
      | { threadId: string }
      | { sessionId: string }
    )>;

export type WorkerLike = {
  onmessage: ((event: { data: unknown }) => void) | null;
  onerror: ((event: { message?: string }) => void) | null;
  onmessageerror: (() => void) | null;
  postMessage(message: unknown): void;
  terminate?(): void;
};

export type WorkerAgentOptions = Readonly<{
  worker?: WorkerLike | (() => WorkerLike) | undefined;
  workerFactory?: (() => WorkerLike) | undefined;
  maxPendingRpcs?: number | undefined;
  /** Reports a ready Worker Agent becoming unusable. Called at most once per Worker connection. */
  onFailure?: ((error: Error) => void) | undefined;
  /** Cancels private Worker preparation or boot; it does not govern a ready Agent. */
  signal?: AbortSignal | undefined;
}>;

/** Internal package seam used by browser/Agent.mjs to preserve Agent.create. */
export function createWorkerAgent(
  options?: WorkerAgentResourceOptions,
  workerOptions?: WorkerAgentOptions,
): Promise<DefaultAgent>;

/** Internal package preparation used by browser config. */
export function prepareWorkerAgent(
  options: WorkerAgentPreparationOptions,
  workerOptions?: WorkerAgentOptions,
): Promise<void>;

export type WorkerAgentRuntime = Readonly<{ dispose(): void }>;
export type WorkerAgentScope = WorkerLike;
export type WorkerAgentRuntimeOptions = Readonly<{
  createAgent?: (options: import("../host/Agent.mjs").create.Options) => Promise<DefaultAgent> | DefaultAgent;
  /** Test/integration seam for the Worker-owned browser durability capability. */
  createDurabilityStore?: () => import("../types.mjs").DurabilityStore;
  prewarmLocal?: (
    harness: false | { threadId: string; origin?: string | undefined },
    options: { module?: WebAssembly.Module | undefined },
  ) => Promise<void> | void;
}>;

/** Installs the package-owned RPC runtime in a module Worker global scope. */
export function installWorkerAgentRuntime(
  scope?: WorkerAgentScope,
  options?: WorkerAgentRuntimeOptions,
): WorkerAgentRuntime;
