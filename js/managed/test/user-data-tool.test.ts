import { describe, expect, it, vi } from "vitest";

import type { ToolContext } from "nanocodex";
import { userDataTool } from "../src/user-data-tool";

const context = {
  callId: "call-1",
  parentCallId: "root",
  sessionId: crypto.randomUUID(),
  model: "test",
  signal: new AbortController().signal,
} satisfies ToolContext;

describe("user data agent tool", () => {
  it("checks reads and writes against the active tool-call context", async () => {
    const execute = vi.fn(async (operation) => operation);
    const requireCapability = vi.fn();
    const tool = userDataTool({ execute, requireCapability });

    await expect(tool.handler({ operation: "document_get", key: "whoop/profile" }, context))
      .resolves.toMatchObject({ operation: "document_get" });
    expect(requireCapability).toHaveBeenLastCalledWith("data:read", context);

    await expect(tool.handler({
      operation: "timeseries_write",
      series: "whoop.heart_rate_bpm",
      points: [{ timestamp_ms: 1, value: 70 }],
    }, context)).resolves.toMatchObject({ operation: "timeseries_write" });
    expect(requireCapability).toHaveBeenLastCalledWith("data:write", context);
  });

  it("does not execute when authorization rejects the operation", async () => {
    const execute = vi.fn();
    const tool = userDataTool({
      execute,
      requireCapability: () => { throw new Error("forbidden"); },
    });
    await expect(tool.handler({ operation: "document_list" }, context)).rejects.toThrow("forbidden");
    expect(execute).not.toHaveBeenCalled();
  });
});
