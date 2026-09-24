import { describe, expect, it } from "vitest";

import {
  isAgentTerminalSnapshot,
  renderAgentTerminal,
} from "../lib/agent-terminal";

describe("agent terminal renderer", () => {
  it("does not allow transcript content to inject terminal control sequences", () => {
    const output = renderAgentTerminal({
      messages: [{ role: "error", text: "bad\x1b[2Jline\u0000" }],
      streamedText: "",
    });

    expect(output.match(/\x1b\[2J/g)).toHaveLength(1);
    expect(output).toContain("bad�[2Jline�");
  });

  it("validates the untyped browser event boundary", () => {
    expect(isAgentTerminalSnapshot({
      messages: [{ role: "agent", text: "done" }],
      streamedText: "",
    })).toBe(true);
    expect(isAgentTerminalSnapshot({
      messages: [{ role: "system", text: "not public" }],
      streamedText: "",
    })).toBe(false);
  });
});
