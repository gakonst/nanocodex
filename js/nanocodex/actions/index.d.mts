import type { Agent, AgentActions } from "../types.mjs";

export * as events from "./events.mjs";
export * as session from "./session.mjs";
export * as turn from "./turn.mjs";
export * as voice from "./voice.mjs";

/** Decorates a base Agent with the standard `turn`, `session`, and `events` domains. */
export function agentActions(): agentActions.DecoratorFn;
export declare namespace agentActions {
  type Decorator = AgentActions;
  type DecoratorFn = (agent: Agent<object>) => Decorator;
}
