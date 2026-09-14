import { describe, expect, it, vi } from "vitest";

import type { ToolContext } from "nanocodex";
import { memorySessionTools } from "../src/memory-session-tools";

const sessionId = "018f1f9a-7b3c-7a09-8000-000000000009";

const context = {
  callId: "call-1",
  parentCallId: "root",
  sessionId,
  model: "test",
  signal: new AbortController().signal,
} satisfies ToolContext;

describe("managed memory and session tool boundary", () => {
  it("exposes only the intended closed agent tools and sanitizes citations", async () => {
    const requireCapability = vi.fn();
    const recordCitations = vi.fn();
    const findSessions = vi.fn(async () => ({
      query: "deploy",
      results: [{
        thread_id: sessionId,
        title: "Deploy",
        turn_id: "turn-1",
        cursor: "1",
        score: 0.75,
        snippet: "deployed",
        provider_secret: "hidden",
      }],
      citations: [],
      credentials: "hidden",
    }));
    const tools = memorySessionTools({
      findSessions,
      readSession: vi.fn(),
      memory: vi.fn(),
      requireCapability,
      requireRootMemoryMutation: vi.fn(),
      recordCitations,
    });

    expect(tools.map((tool) => tool.name)).toEqual([
      "find_session",
      "find_sessions",
      "read_session",
      "memory",
    ]);
    for (const tool of tools.slice(0, 3)) expect(tool.parameters).toMatchObject({
      additionalProperties: false,
    });
    expect((tools[3]!.parameters?.oneOf as { additionalProperties: boolean }[])
      .map((operation) => operation.additionalProperties)).toEqual([
      false, false, false, false,
    ]);

    await expect(tools[0]!.handler({ query: " deploy ", limit: 1 }, context)).resolves.toEqual({
      sessions: [{
        session_id: sessionId,
        title: "Deploy",
        turn_id: "turn-1",
        cursor: "1",
        score: 0.75,
        preview: "deployed",
      }],
    });
    expect(requireCapability).toHaveBeenCalledExactlyOnceWith("history:read", context);
    expect(findSessions).toHaveBeenCalledExactlyOnceWith({ query: "deploy", limit: 1 });
    expect(recordCitations).toHaveBeenCalledExactlyOnceWith([{
      thread_id: sessionId,
      title: "Deploy",
      sources: [{ turn_id: "turn-1", cursor: "1" }],
    }]);
    await expect(tools[1]!.handler({ query: "deploy", limit: 1 }, context)).resolves.toEqual(
      await tools[0]!.handler({ query: "deploy", limit: 1 }, context),
    );
  });

  it("fences reads and root-only mutations through active-turn authorization", async () => {
    const requireCapability = vi.fn();
    const requireRootMemoryMutation = vi.fn((toolContext: ToolContext) => {
      if (toolContext.subagent !== undefined) throw new Error("memory_root_only");
    });
    const memory = vi.fn(async (operation: { operation: string }) => operation.operation === "scan"
      ? { operation: "scan" as const, abstained: true, candidates: [] }
      : { operation: "delete" as const, key: { id: 1, version: 1 } });
    const tools = memorySessionTools({
      findSessions: vi.fn(),
      readSession: vi.fn(),
      memory,
      requireCapability,
      requireRootMemoryMutation,
      recordCitations: vi.fn(),
    });
    const memoryTool = tools.find((tool) => tool.name === "memory")!;

    await expect(memoryTool.handler({ operation: "scan", query: "scope" }, context))
      .resolves.toMatchObject({ operation: "scan", abstained: true });
    expect(requireCapability).toHaveBeenLastCalledWith("memory:read", context);
    expect(requireRootMemoryMutation).not.toHaveBeenCalled();

    const subagent = {
      ...context,
      subagent: {
        agentId: "2",
        parentAgentId: "1",
        sessionId,
        role: "worker",
        task: "mutate",
      },
    } satisfies ToolContext;
    await expect(memoryTool.handler({
      operation: "delete",
      key: { id: 1, version: 1 },
    }, subagent)).rejects.toThrow("memory_root_only");
    expect(memory).toHaveBeenCalledTimes(1);

    await expect(memoryTool.handler({
      operation: "delete",
      key: { id: 1, version: 1 },
    }, context)).resolves.toMatchObject({ operation: "delete" });
    expect(requireCapability).toHaveBeenLastCalledWith("memory:write", context);
  });

  it("checks each history call's own context before accessing persistence", async () => {
    const findSessions = vi.fn();
    const readSession = vi.fn();
    const denied = { ...context, sessionId: crypto.randomUUID() };
    const tools = memorySessionTools({
      findSessions, readSession, memory: vi.fn(), recordCitations: vi.fn(),
      requireRootMemoryMutation: vi.fn(),
      requireCapability: (_capability, caller) => {
        expect(caller).toBe(denied);
        throw new Error("forbidden");
      },
    });
    for (const tool of tools.filter((tool) => tool.name !== "memory")) {
      await expect(tool.handler(tool.name === "read_session"
        ? { session_id: sessionId } : { query: "deploy" }, denied)).rejects.toThrow("forbidden");
    }
    expect(findSessions).not.toHaveBeenCalled();
    expect(readSession).not.toHaveBeenCalled();
  });
});
