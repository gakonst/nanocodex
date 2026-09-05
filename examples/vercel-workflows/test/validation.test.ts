import { describe, expect, it } from "vitest";

import {
  RequestError,
  parsePrompt,
  parseSessionId,
  parseStartIndex,
} from "../lib/validation";

describe("request validation", () => {
  it("accepts bounded prompt and workflow identifiers", () => {
    expect(parseSessionId("wrun_01ABC-def")).toBe("wrun_01ABC-def");
    expect(parsePrompt({ id: "turn:1", input: "hello" })).toEqual({
      id: "turn:1",
      input: "hello",
    });
    expect(parseStartIndex("42")).toBe(42);
  });

  it("rejects traversal, malformed cursors, and empty prompts", () => {
    expect(() => parseSessionId("../subscription")).toThrow(RequestError);
    expect(() => parseStartIndex("-1")).toThrow("non-negative integer");
    expect(() => parsePrompt({ id: "turn", input: "   " })).toThrow("must not be empty");
  });

  it("enforces the one MiB prompt boundary in encoded bytes", () => {
    expect(() => parsePrompt({ id: "turn", input: "é".repeat(524_289) }))
      .toThrow("exceeds 1 MiB");
  });
});
