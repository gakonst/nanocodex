import { describe, expect, it } from "vitest";

import {
  parseTerminalTextFrame,
  terminalResizeFrame,
  terminalSocketUrl,
} from "../lib/terminal-protocol";

describe("Vercel Sandbox interactive protocol", () => {
  it("constructs the authenticated controller WebSocket URL", () => {
    expect(terminalSocketUrl("wss://controller.example/pty?attempt=1", "a b&c"))
      .toBe("wss://controller.example/pty?attempt=1&token=a+b%26c");
    expect(() => terminalSocketUrl("https://controller.example/pty", "token"))
      .toThrow("not a WebSocket URL");
  });

  it("parses only exit control text", () => {
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
