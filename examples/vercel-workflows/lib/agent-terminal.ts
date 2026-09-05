export const AGENT_TERMINAL_EVENT = "nanocodex:agent-terminal-snapshot";
export const AGENT_TERMINAL_READY_EVENT = "nanocodex:agent-terminal-ready";

export type AgentTerminalMessage = {
  role: "you" | "agent" | "error";
  text: string;
};

export type AgentTerminalSnapshot = {
  messages: AgentTerminalMessage[];
  streamedText: string;
};

const CLEAR_SCREEN = "\x1b[3J\x1b[2J\x1b[H";
const RESET = "\x1b[0m";
const DIM = "\x1b[90m";
const ROLE_STYLE = {
  you: "\x1b[1;32m",
  agent: "\x1b[1;36m",
  error: "\x1b[1;31m",
} as const;

export function isAgentTerminalSnapshot(
  value: unknown,
): value is AgentTerminalSnapshot {
  if (!value || typeof value !== "object") return false;
  const snapshot = value as Record<string, unknown>;
  return typeof snapshot.streamedText === "string"
    && Array.isArray(snapshot.messages)
    && snapshot.messages.every(isAgentTerminalMessage);
}

export function renderAgentTerminal(snapshot: AgentTerminalSnapshot): string {
  const blocks = snapshot.messages.map((message) => renderMessage(message));
  if (snapshot.streamedText) {
    blocks.push(renderMessage({ role: "agent", text: snapshot.streamedText }, true));
  }
  if (blocks.length === 0) {
    blocks.push(
      `${DIM}Create or join a workflow, then send a prompt. Agent output will stream here.${RESET}`,
    );
  }
  return `${CLEAR_SCREEN}${blocks.join("\r\n\r\n")}\r\n`;
}

function isAgentTerminalMessage(value: unknown): value is AgentTerminalMessage {
  if (!value || typeof value !== "object") return false;
  const message = value as Record<string, unknown>;
  return (message.role === "you" || message.role === "agent" || message.role === "error")
    && typeof message.text === "string";
}

function renderMessage(message: AgentTerminalMessage, streaming = false): string {
  const label = message.role === "you"
    ? "YOU"
    : message.role === "agent"
      ? streaming ? "NANOCODEX · LIVE" : "NANOCODEX"
      : "ERROR";
  return `${ROLE_STYLE[message.role]}${label}${RESET}\r\n${safeTerminalText(message.text)}`;
}

function safeTerminalText(text: string): string {
  return text
    .replace(/\r\n?/g, "\n")
    .replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g, "�")
    .replace(/\n/g, "\r\n");
}
