export type AgentTerminalMode = "preview" | "full" | "hidden";
export type AgentStatus = "idle" | "starting" | "ready" | "stopped" | "error";

export type AgentTerminalState = Readonly<{
  error: string | undefined;
  retry(): void;
  status: AgentStatus;
}>;
