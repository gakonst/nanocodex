import type {
  Agent,
  AgentSessionContext,
  DefaultAgent,
  ForkOptions,
  HarnessModel,
  RealtimeTranscriptEntry,
  SessionCapabilities,
  SessionCheckpoint,
  SessionInfo,
  SessionPersistence,
  ServiceTier,
  Thinking,
} from "../types.mjs";

/** Returns this session's identity, harness family, and lineage. */
export function info(agent: Agent<object>): SessionInfo;

/** Returns the lifecycle operations this session's backend supports. */
export function capabilities(agent: Agent<object>): SessionCapabilities;

/** Returns where this session is persisted, or null when it lives only in memory. */
export function persistence(agent: Agent<object>): SessionPersistence | null;

/** Exports the latest committed safe boundary as a portable checkpoint. */
export function checkpoint(agent: Agent<object>): Promise<SessionCheckpoint>;

/** Cancels every nonterminal turn issued through this Agent. */
export function cancel(agent: Agent<object>): Promise<void>;

/** Appends adapter-owned developer context and returns the latest safe session context. */
export function appendDeveloperMessage(
  agent: Agent<object>,
  text: string,
): Promise<AgentSessionContext>;

/** Starts the canonical Codex Realtime adapter lifecycle at a safe boundary. */
export function startRealtimeConversation(agent: Agent<object>): Promise<AgentSessionContext>;

/** Ends the canonical Codex Realtime adapter lifecycle at a safe boundary. */
export function endRealtimeConversation(agent: Agent<object>): Promise<AgentSessionContext>;

/** Formats structured Realtime input using canonical Codex delegation markers. */
export function realtimeDelegation(
  agent: Agent<object>,
  input: string,
  transcript?: readonly RealtimeTranscriptEntry[],
): Promise<string>;

/** Formats an unconsumed transcript tail, or returns undefined for an empty tail. */
export function realtimeTailDelegation(
  agent: Agent<object>,
  transcript: readonly RealtimeTranscriptEntry[],
): Promise<string | undefined>;

/** Compacts retained history immediately without fabricating a user prompt. */
export function compact(agent: Agent<object>): Promise<void>;

/** Returns complete read-only model context at the latest safe boundary. */
export function context(agent: Agent<object>): Promise<AgentSessionContext>;

/**
 * Forks the latest committed boundary, or the completed result or portable
 * checkpoint supplied in `options.at`. `options.origin` marks side conversations.
 */
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

/** Changes the model to another model id of the session's harness family. */
export function setModel(agent: Agent<object>, model: HarnessModel): Promise<void>;

/** Enables or disables priority processing for subsequently accepted turns. */
export function setFastMode(agent: Agent<object>, enabled: boolean): Promise<void>;

/**
 * Selects the processing tier for subsequently accepted turns. Governed by
 * `capabilities().serviceTier`; a tier the backend cannot select rejects with
 * `code: "unsupported_capability"`.
 */
export function setServiceTier(agent: Agent<object>, serviceTier: ServiceTier): Promise<void>;

/** Stops the driver and joins every resource owned by this Agent. */
export function shutdown(agent: Agent<object>): Promise<void>;
