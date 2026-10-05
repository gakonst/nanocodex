import type { Backend, Codex, Claude } from '../runtime/backend.mjs';
import type { create } from './Agent.mjs';
import type { Options as ExplicitClaudeOptions } from '../runtime/claude.mjs';
import type { DurabilityStore, Thinking } from '../types.mjs';

type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;

type AutomaticTools = {
  transport?: never; auth?: never; harness?: never; tools?: never; filesystem?: never;
  harnesses?: never; endpoint?: never; apiKey?: never;
};
/** Workspace is a native directory, defaulting to process.cwd(). Shell execution has host permissions. */
export type CommonOptions = AutomaticTools & {
  backend: Backend;
  model?: string;
  thinking?: Thinking;
  instructions?: string;
  sessionId?: string;
  workspace?: string;
  module?: unknown;
} & (
  | { durability?: never; durabilityId?: never }
  | { durability: DurabilityStore; durabilityId: string }
);
export type CodexOptions = DistributiveOmit<create.Options, 'backend' | 'transport' | 'tools' | 'filesystem' | 'harness' | 'harnesses'> & AutomaticTools & { backend: Codex };
export type ClaudeOptions = DistributiveOmit<ExplicitClaudeOptions, 'auth' | 'tools' | 'harness' | 'harnesses' | 'endpoint' | 'fetch' | 'compatibilityProfile' | 'subscriptionIdentity' | 'model'> & AutomaticTools & { backend: Claude; model?: string };
export type Options = CodexOptions | ClaudeOptions | CommonOptions;
