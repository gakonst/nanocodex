export const TERMINAL_SESSION_EVENT = "nanocodex:workflow-session";

const MAX_TERMINAL_DIMENSION = 4096;
const terminalEncoder = new TextEncoder();

export type TerminalExit = {
  type: "exit";
  code?: number;
};

export function terminalSocketUrl(url: string, token: string): string {
  const endpoint = new URL(url);
  if (endpoint.protocol !== "ws:" && endpoint.protocol !== "wss:") {
    throw new Error("terminal endpoint is not a WebSocket URL");
  }
  endpoint.searchParams.set("token", token);
  return endpoint.toString();
}

export function terminalStartFrame(cols: number, rows: number): string {
  const dimensions = terminalDimensions(cols, rows);
  return JSON.stringify({
    type: "start",
    command: "bash",
    args: ["-l"],
    env: ["TERM=xterm-256color"],
    cwd: "/workspace",
    ...dimensions,
  });
}

export function terminalResizeFrame(cols: number, rows: number): string {
  return JSON.stringify({ type: "resize", ...terminalDimensions(cols, rows) });
}

export function terminalInputFrame(data: string): Uint8Array<ArrayBuffer> {
  return terminalEncoder.encode(data) as Uint8Array<ArrayBuffer>;
}

export function parseTerminalTextFrame(data: string): TerminalExit | null {
  try {
    const value = JSON.parse(data) as Record<string, unknown>;
    if (value.type !== "exit") return null;
    return {
      type: "exit",
      ...(typeof value.code === "number" ? { code: value.code } : {}),
    };
  } catch {
    return null;
  }
}

function terminalDimensions(
  cols: number,
  rows: number,
): { cols: number; rows: number } {
  if (
    !Number.isInteger(cols) ||
    !Number.isInteger(rows) ||
    cols < 1 ||
    rows < 1 ||
    cols > MAX_TERMINAL_DIMENSION ||
    rows > MAX_TERMINAL_DIMENSION
  ) {
    throw new Error("terminal dimensions must be integers between 1 and 4096");
  }
  return { cols, rows };
}
