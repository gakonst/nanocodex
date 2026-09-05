import type { ReactNode } from "react";

export type AgentEvent = Readonly<{
  request_id: string;
  seq: number;
  type: string;
  payload: Readonly<Record<string, unknown>>;
}>;

export type AgentTurnResult = Readonly<{
  finalMessage: string;
  dispose(): void;
}>;

export type AgentTurn = Readonly<{
  historyEntryId?: string | undefined;
  steer(options: Readonly<{ input: string }>): Promise<unknown>;
  cancel(): Promise<unknown>;
  result(): Promise<AgentTurnResult>;
  dispose(): void;
}>;

export type AgentEventWatcher = Readonly<{
  onEvent(listener: (event: AgentEvent) => void): () => void;
  onHistory?(listener: (events: readonly AgentEvent[]) => void): () => void;
  loadOlder?(): Promise<boolean>;
  off(): void;
}>;

/** Normalized structural Agent contract consumed by the headless controller. */
export type Agent = Readonly<{
  sessionId: string;
  /** Optional canonical SDK resource used by presentation-owned voice controls. */
  voiceSource?:
    | import("nanocodex").DefaultAgent
    | import("nanocodex/managed").ManagedAgent
    | import("nanocodex/connect").ConnectAgent
    | undefined;
  turn: Readonly<{
    prompt(options: Readonly<{ input: string }>): AgentTurn;
  }>;
  events: Readonly<{
    watch(): AgentEventWatcher;
  }>;
}>;

export type ToolStatus = "running" | "completed" | "cancelled" | "failed";

export type ToolActivity = Readonly<{
  callId: string;
  name: string;
  /** Concise backwards-compatible argument summary. */
  arguments: string;
  /** Bounded serialized tool input for expandable presentation. */
  input?: string | undefined;
  /** Concise backwards-compatible terminal result summary. */
  result?: string | undefined;
  /** Bounded serialized tool output, including successful generic results. */
  output?: string | undefined;
  status: ToolStatus;
  durationNs?: number | undefined;
  images?: readonly string[] | undefined;
  /** Provider-neutral execution metadata retained with the terminal result. */
  metadata?: unknown;
  children: readonly ToolActivity[];
}>;

export type PlanUpdate = Readonly<{
  explanation?: string | undefined;
  plan: readonly Readonly<{
    step: string;
    status: "pending" | "in_progress" | "completed";
  }>[];
}>;

export type AgentEntry = Readonly<(
  | { id: string; kind: "user"; text: string; promptId?: number | undefined }
  | { id: string; kind: "reasoning"; text: string; streaming: boolean }
  | { id: string; kind: "assistant"; text: string; streaming: boolean }
  | { id: string; kind: "tool"; tool: ToolActivity }
  | { id: string; kind: "plan"; update: PlanUpdate }
  | { id: string; kind: "error"; text: string }
) & { turnId?: string | undefined }>;

export type AgentControllerEvent = Readonly<{
  type: string;
  timestamp: number;
  [key: string]: unknown;
}>;

export type SubmitOptions = Readonly<{
  /** Auto-steers an active turn by default; queue always starts a new queued root turn. */
  intent?: "queue" | "steer" | undefined;
}>;

export type AgentControllerSnapshot = Readonly<{
  entries: readonly AgentEntry[];
  running: boolean;
  status: string;
  pendingTurns: number;
  isLoadingOlder: boolean;
  canLoadOlder: boolean;
  hasOlder: boolean | undefined;
  visible: boolean;
  submit(input: string, options?: SubmitOptions): Promise<AgentTurn | undefined>;
  steer(input: string, options?: Omit<SubmitOptions, "intent">): Promise<AgentTurn | undefined>;
  cancel(): Promise<boolean>;
  clear(): void;
  loadOlder(): Promise<boolean>;
  /** Permanently releases this mounted controller. Normal React unmount cleanup is automatic. */
  dispose(): void;
  /** Imperative counterpart to the visible option for non-render ownership boundaries. */
  setVisible(visible: boolean): void;
}>;

export type UseAgentControllerOptions = Readonly<{
  /** Bounds retained presentation entries. @default 200 */
  maxEntries?: number | undefined;
  /** Hidden controllers keep reducing events and publish one coalesced catch-up snapshot. @default true */
  visible?: boolean | undefined;
  onEvent?: ((event: AgentControllerEvent) => void) | undefined;
}>;

export function useAgentController(
  agent: Agent | undefined,
  options?: UseAgentControllerOptions,
): AgentControllerSnapshot;

export function AgentController(props: Readonly<UseAgentControllerOptions & {
  agent: Agent | undefined;
  children(controller: AgentControllerSnapshot): ReactNode;
}>): ReactNode;
