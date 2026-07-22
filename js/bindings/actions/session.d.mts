import type { Agent, DefaultAgent, ForkOptions, Thinking } from "../types.mjs";

/** Forks the latest checkpoint, or the exact completed Turn supplied in `options.at`. */
export function fork(agent: Agent<object>, options?: fork.Options): Promise<fork.ReturnType>;
export declare namespace fork {
  type Options = ForkOptions;
  type ReturnType = DefaultAgent;
}

/** Creates a clean sibling with the Agent's configuration and tools. */
export function spawn(agent: Agent<object>): Promise<spawn.ReturnType>;
export declare namespace spawn {
  type ReturnType = DefaultAgent;
}

/** Changes the reasoning effort for subsequently accepted turns. */
export function setThinking(agent: Agent<object>, thinking: Thinking): Promise<void>;
