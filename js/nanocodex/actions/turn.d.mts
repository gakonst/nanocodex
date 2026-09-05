import type {
  Agent,
  PromptInput,
  SessionSnapshot,
  Turn,
  TurnResult,
  TurnUsage,
} from "../types.mjs";

/** Accepts a prompt on an owned Agent and returns its independently awaitable Turn. */
export function prompt<const agent extends Agent<object>>(
  agent: agent,
  options: prompt.Options,
): prompt.ReturnType<agent>;
export declare namespace prompt {
  type Options = { input: PromptInput; id?: string | undefined };
  type ReturnType<agent extends Agent<object> = Agent<object>> = Turn<agent>;
}

/** Waits for durable admission and returns its request ID when one was assigned. */
export function accepted(turn: Turn): Promise<accepted.ReturnType>;
export declare namespace accepted {
  type ReturnType = string | undefined;
}

/** Waits for a Turn's typed completed result. */
export function getResult(turn: Turn): Promise<getResult.ReturnType>;
export declare namespace getResult {
  type ReturnType = TurnResult;
}

/** Materializes a completed result's serializable session snapshot. */
export function getSnapshot(result: TurnResult): Promise<getSnapshot.ReturnType>;
export declare namespace getSnapshot {
  type ReturnType = SessionSnapshot;
}

/** Materializes exact aggregate token usage from a completed result. */
export function getUsage(result: TurnResult): Promise<getUsage.ReturnType>;
export declare namespace getUsage {
  type ReturnType = TurnUsage;
}

/** Adds input to an active Turn. */
export function steer(turn: Turn, options: steer.Options): Promise<void>;
export declare namespace steer {
  type Options = { input: PromptInput };
  type ReturnType = void;
}

/** Cancels an active or queued Turn. */
export function cancel(turn: Turn): Promise<void>;
export declare namespace cancel {
  type ReturnType = void;
}
