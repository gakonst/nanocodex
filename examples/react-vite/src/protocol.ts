import type {
  AgentEvent,
  ReasoningMode,
  Thinking,
} from "nanocodex";

export type StartMessage = {
  type: "start";
  transport: "openai" | "mpp";
  thinking: Thinking;
  reasoningMode?: ReasoningMode;
};

export type PromptMessage = {
  type: "prompt";
  id: number;
  prompt: string;
};

export type AgentWorkerCommand = StartMessage | PromptMessage;

export type AgentWorkerMessage =
  | {
      type: "ready";
      transport: "openai" | "mpp";
      rootAddress?: string;
      accessKeyAddress?: string;
      channelId?: string;
    }
  | { type: "event"; event: AgentEvent }
  | {
      type: "result";
      id: number;
      message: string;
      payment?: { channelId?: string; cumulative: string; mcpCumulative?: string };
    }
  | { type: "error"; id?: number; message: string };
