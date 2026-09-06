import type { DefaultAgent, Thinking } from "../types.mjs";

// Adapter-specific extend() signatures do not change subagent ownership.
type SubagentOwner = Omit<DefaultAgent, "extend">;

declare const subagentToolBrand: unique symbol;

/** Opaque selector for the Rust-owned subagent tool set. */
export type Tool = Readonly<{
  [subagentToolBrand]: true;
}>;

export type Subagents = readonly [Tool];

export interface Options {
  /** Optional finite concurrency limit. Omit for unlimited active turns. */
  maxConcurrency?: number | undefined;
}

export type AgentId = number;
export type AgentStatus =
  | Readonly<{ state: "pending" | "running" | "interrupted" | "closing" | "closed" }>
  | Readonly<{ state: "completed"; output: unknown }>
  | Readonly<{ state: "failed"; error: string }>;
export type AgentSummary = Readonly<{
  agent_id: AgentId;
  role: string;
  task: string;
  parent_agent_id: AgentId | null;
  status: AgentStatus;
  last_output?: unknown;
}>;
export type JsonSchema = boolean | Readonly<Record<string, unknown>>;
export type SpawnOptions = Readonly<{
  role: string;
  task: string;
  model?: "sol" | "terra" | "luna" | "astra" | undefined;
  thinking?: Thinking | undefined;
  outputSchema: JsonSchema;
}>;
export type BatchSpawnOptions = Readonly<{
  role: string;
  task: string;
  outputSchema: JsonSchema;
}>;
export type SpawnReport = Readonly<{
  agent_id: AgentId;
  role: string;
  status: Readonly<{ state: "running" }>;
}>;
export type WaitOptions = Readonly<{
  agentIds: readonly AgentId[];
  timeoutMs?: number | undefined;
}>;
export type WaitReport = Readonly<{
  agents: readonly AgentSummary[];
  timed_out: boolean;
}>;
export type LifecycleReport = Readonly<{ agents: readonly AgentSummary[] }>;
export type DirectoryEntry = AgentSummary & Readonly<{
  can_message: boolean;
  can_manage: boolean;
}>;
export type DirectoryOptions = Readonly<{
  includeCompleted?: boolean | undefined;
  includeSelf?: boolean | undefined;
}>;
export type DirectoryReport = Readonly<{ agents: readonly DirectoryEntry[] }>;
export type MessagePriority = "deferred" | "urgent";
export type MessagePurpose = "delegate" | "coordinate" | "finding" | "question" | "reply";
export type MessageSender =
  | Readonly<{ kind: "root" }>
  | Readonly<{ kind: "agent"; agent_id: AgentId }>;
export type SendOptions = Readonly<{
  agentId: AgentId;
  message: string;
  priority?: MessagePriority | undefined;
  purpose?: MessagePurpose | undefined;
  inReplyTo?: number | undefined;
}>;
export type MessageReceipt = Readonly<{
  message_id: number;
  thread_id: number;
  from: MessageSender;
  to_agent_id: AgentId;
  disposition: "started" | "queued" | "steered";
}>;

/** Returns a spreadable Rust-backed tool extension for an Agent's tools array. */
export function create(options?: Options): Subagents;
/** Starts a structured SDK child through the Rust registry. */
export function spawn(agent: SubagentOwner, options: SpawnOptions): Promise<SpawnReport>;
/** Atomically reserves and starts an ordered batch of canonical Rust subagents. */
export function spawnMany(
  agent: SubagentOwner,
  options: readonly BatchSpawnOptions[],
): Promise<readonly SpawnReport[]>;
/** Waits on numeric SDK child IDs through the Rust registry. */
export function wait(agent: SubagentOwner, options: WaitOptions): Promise<WaitReport>;
/** Lists the SDK task tree through the Rust registry. */
export function list(agent: SubagentOwner, options?: DirectoryOptions): Promise<DirectoryReport>;
/** Delivers a structured SDK message through the Rust registry. */
export function send(agent: SubagentOwner, options: SendOptions): Promise<MessageReceipt>;
/** Interrupts the SDK child and its descendants. */
export function interrupt(agent: SubagentOwner, agentId: AgentId): Promise<LifecycleReport>;
/** Closes the SDK child and its descendants. */
export function close(agent: SubagentOwner, agentId: AgentId): Promise<LifecycleReport>;
