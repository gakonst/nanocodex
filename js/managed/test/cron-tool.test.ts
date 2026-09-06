import type { ToolContext } from "nanocodex";
import { describe, expect, it, vi } from "vitest";
import { createCronTool } from "../src/cron-tool";

const context = {
  callId: "cron-call", parentCallId: "cell", sessionId: crypto.randomUUID(),
  model: "test", signal: new AbortController().signal,
} satisfies ToolContext;

describe("managed cron tool protocol", () => {
  it("normalizes the scheduler contract and preserves caller authorization context", async () => {
    const saved = {
      id: "morning", cron: "0 9 * * *", input: "Summarize the morning", timezone: "UTC",
      enabled: true, session_mode: "new" as const, next_run_at: 123,
      last_agent_id: null, last_run_at: null, last_turn_id: null, last_skipped_at: null,
      created_at: 1, updated_at: 1,
    };
    const create = vi.fn(async () => saved);
    const tool = createCronTool(create);
    expect(tool.name).toBe("create_cron");
    expect(tool.parameters).toMatchObject({ required: ["id", "cron", "input"], additionalProperties: false });
    await expect(tool.handler({ id: "morning", cron: " 0  9 * * * ", input: "Summarize the morning" }, context)).resolves.toBe(saved);
    expect(create).toHaveBeenCalledExactlyOnceWith("morning", {
      cron: "0 9 * * *", input: "Summarize the morning", timezone: "UTC", enabled: true, session_mode: "new",
    }, context);
    await tool.handler({ id: "paused", cron: "0 9 * * *", input: "Continue the task", timezone: "Europe/Athens", enabled: false, session_mode: "continue" }, context);
    expect(create).toHaveBeenLastCalledWith("paused", {
      cron: "0 9 * * *", input: "Continue the task", timezone: "Europe/Athens", enabled: false, session_mode: "continue",
    }, context);
  });

  it.each([
    null, [], { id: "../other", cron: "* * * * *", input: "Check" },
    { id: "cron", cron: "* * * * * *", input: "Check" },
    { id: "cron", cron: "* * * * *", input: "Check", authorization: {} },
    { id: "cron", cron: "* * * * *", input: "Check", agent_id: "other" },
  ])("rejects malformed input and authority overrides before saving %#", async (input) => {
    const create = vi.fn();
    await expect(async () => createCronTool(create).handler(input, context)).rejects.toThrow();
    expect(create).not.toHaveBeenCalled();
  });
});
