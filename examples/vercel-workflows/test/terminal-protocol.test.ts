import { describe, expect, it } from "vitest";

import {
  parseTerminalTextFrame,
  terminalInputFrame,
  terminalResizeFrame,
  terminalSocketUrl,
  terminalStartFrame,
} from "../lib/terminal-protocol";

describe("Vercel Sandbox interactive protocol", () => {
  it("constructs the authenticated controller WebSocket URL", () => {
    expect(terminalSocketUrl("wss://controller.example/pty?attempt=1", "a b&c"))
      .toBe("wss://controller.example/pty?attempt=1&token=a+b%26c");
    expect(() => terminalSocketUrl("https://controller.example/pty", "token"))
      .toThrow("not a WebSocket URL");
  });

  it("starts bash in the shared workspace with exact PTY dimensions", () => {
    expect(JSON.parse(terminalStartFrame(120, 42))).toEqual({
      type: "start",
      command: "bash",
      args: ["-l"],
      env: ["TERM=xterm-256color"],
      cwd: "/workspace",
      cols: 120,
      rows: 42,
    });
    expect(JSON.parse(terminalResizeFrame(80, 24))).toEqual({
      type: "resize",
      cols: 80,
      rows: 24,
    });
  });

  it("sends stdin as binary UTF-8 and parses only exit control text", () => {
    expect([...terminalInputFrame("λ\r")]).toEqual([...new TextEncoder().encode("λ\r")]);
    expect(parseTerminalTextFrame('{"type":"exit","code":7}')).toEqual({
      type: "exit",
      code: 7,
    });
    expect(parseTerminalTextFrame('{"type":"other"}')).toBeNull();
    expect(parseTerminalTextFrame("plain terminal output")).toBeNull();
  });

  it.each([[0, 24], [80, 0], [1.5, 24], [80, 4097]])(
    "rejects unsafe dimensions %s x %s",
    (cols, rows) => {
      expect(() => terminalResizeFrame(cols, rows)).toThrow("between 1 and 4096");
    },
  );
});
