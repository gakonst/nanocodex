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

describe("managed session history tool boundary", () => {
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
      requireCapability,
      recordCitations,
    });

    expect(tools.map((tool) => tool.name)).toEqual([
      "find_session",
      "find_sessions",
      "read_session",
    ]);
    for (const tool of tools) expect(tool.parameters).toMatchObject({
      additionalProperties: false,
    });
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

  it("projects read-session results and citations without exposing storage fields", async () => {
    const requireCapability = vi.fn();
    const recordCitations = vi.fn();
    const readSession = vi.fn(async () => ({ turns: [{ thread_id: sessionId, title: "Deploy", turn_id: "turn-1", cursor: "1",
      user: "Deploy this", assistant: "Deployed", provider_secret: "hidden" }], citations: [] }));
    const tools = memorySessionTools({ findSessions: vi.fn(), readSession, requireCapability, recordCitations });
    const read = tools.find(tool => tool.name === "read_session")!;
    expect(await read.handler({ session_id: sessionId, turn_ids: ["turn-1"] }, context)).toEqual({ turns: [{
      session_id: sessionId, title: "Deploy", turn_id: "turn-1", cursor: "1", user: "Deploy this", assistant: "Deployed",
    }] });
    expect(requireCapability).toHaveBeenCalledExactlyOnceWith("history:read", context);
    expect(readSession).toHaveBeenCalledExactlyOnceWith({ session_id: sessionId, turn_ids: ["turn-1"] });
    expect(recordCitations).toHaveBeenCalledExactlyOnceWith([{ thread_id: sessionId, title: "Deploy", sources: [{ turn_id: "turn-1", cursor: "1" }] }]);
  });

  it("checks each history call's own context before accessing persistence", async () => {
    const findSessions = vi.fn();
    const readSession = vi.fn();
    const denied = { ...context, sessionId: crypto.randomUUID() };
    const tools = memorySessionTools({
      findSessions, readSession, recordCitations: vi.fn(),
      requireCapability: (_capability, caller) => {
        expect(caller).toBe(denied);
        throw new Error("forbidden");
      },
    });
    for (const tool of tools) {
      await expect(tool.handler(tool.name === "read_session"
        ? { session_id: sessionId } : { query: "deploy" }, denied)).rejects.toThrow("forbidden");
    }
    expect(findSessions).not.toHaveBeenCalled();
    expect(readSession).not.toHaveBeenCalled();
  });
});
