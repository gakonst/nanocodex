import type { Agent, AgentSessionContext, DefaultAgent, DocumentForkSeed, DocumentWrite, SessionDocument, ForkOptions, RealtimeTranscriptEntry, Thinking } from "../types.mjs";

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

/** Forks the latest checkpoint, or the exact completed result supplied in `options.at`. */
export function fork(agent: Agent<object>, options?: fork.Options): Promise<fork.ReturnType>;
export declare namespace fork {
  type Options = ForkOptions;
  type ReturnType = DefaultAgent;
}

/** Reads committed session data; requires durability and returns null for a missing key. */
export function document(agent: Agent<object>, key: string): Promise<SessionDocument | null>;

/** Atomically commits all writes if every expected version and creation policy matches. */
export function compareExchangeDocuments(agent: Agent<object>, writes: readonly DocumentWrite[]): Promise<void>;

/** Stages writes for the successful completion of the durable turn with this operation ID. */
export function stageDocumentWrites(agent: Agent<object>, operationId: string, writes: readonly DocumentWrite[]): Promise<void>;

/** Exports an exact completed durable boundary, including policy-selected document data. */
export function documentFork(agent: Agent<object>, operationId: string): Promise<DocumentForkSeed>;

/** Creates a clean sibling with the Agent's configuration and tools. */
export function spawn(agent: Agent<object>): Promise<spawn.ReturnType>;
export declare namespace spawn {
  type ReturnType = DefaultAgent;
}

/** Changes the reasoning effort for subsequently accepted turns. */
export function setThinking(agent: Agent<object>, thinking: Thinking): Promise<void>;

/** Enables or disables priority processing for subsequently accepted turns. */
export function setFastMode(agent: Agent<object>, enabled: boolean): Promise<void>;

/** Stops the driver and joins every resource owned by this Agent. */
export function shutdown(agent: Agent<object>): Promise<void>;
