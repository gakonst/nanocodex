import type { DefaultAgent } from "../types.mjs";
import type { create as createAgent } from "./Agent.mjs";

export type AgentStatus = "idle" | "pending" | "success" | "error";

export type AgentSnapshot =
  | Readonly<{ data: undefined; error: undefined; status: "idle" }>
  | Readonly<{ data: undefined; error: undefined; status: "pending" }>
  | Readonly<{ data: DefaultAgent; error: undefined; status: "success" }>
  | Readonly<{ data: undefined; error: unknown; status: "error" }>;

export type AgentParameters = Readonly<{
  enabled?: boolean | undefined;
  threadId?: string | undefined;
}>;

export type Config = Readonly<{
  getAgent(parameters?: AgentParameters): AgentSnapshot;
  subscribeAgent(parameters: AgentParameters, listener: () => void): () => void;
  refetchAgent(parameters?: AgentParameters): void;
  destroy(): Promise<void>;
}>;

export type CreateConfigParameters = Readonly<{
  /** Defaults inherited by every Agent created by this config. */
  agent?: createAgent.Options | undefined;
  /** Stable origin included in both preparation and creation of the browser harness. */
  origin?: string | undefined;
  /** Agent startup retries after the first attempt. Defaults to 2. */
  retry?: number | undefined;
  /** Backoff before a startup retry. Defaults to 400ms × attempt. */
  retryDelay?: ((attempt: number, error: unknown) => number) | undefined;
}>;

/** Creates the stable browser runtime consumed by framework bindings. */
export function createConfig(options?: CreateConfigParameters): Config;
