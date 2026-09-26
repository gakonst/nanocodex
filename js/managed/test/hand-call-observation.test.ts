import { afterEach, describe, expect, it, vi } from "vitest";
import { handToolKind, observeHandCall } from "../src/hand-call-observation";

afterEach(() => vi.restoreAllMocks());

describe("privacy-safe Hand stage observations", () => {
  it("logs a bounded opaque source call id and a fixed tool kind", () => {
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    observeHandCall("namespace.invoke", "mcp__cua_repl__js", performance.now(), "ambiguous",
      "call_abc123/code-76");
    expect(info).toHaveBeenCalledOnce();
    const row = info.mock.calls[0]?.[0];
    expect(row).toMatchObject({ type: "hand.tool.stage", stage: "namespace.invoke",
      tool: "cua", outcome: "ambiguous", call_id: "call_abc123/code-76" });
    expect(row.duration_ms).toBeGreaterThanOrEqual(0);
    expect(Object.keys(row).sort()).toEqual(["call_id", "duration_ms", "outcome", "stage", "tool", "type"]);
    expect(handToolKind("some-private-provider-tool")).toBe("other");
  });
  it("omits unsafe or overlong call ids", () => {
    const info = vi.spyOn(console, "info").mockImplementation(() => {});
    observeHandCall("sandbox.preflight", "exec_command", performance.now(), "unavailable", "secret token\nvalue");
    observeHandCall("account.fetch", "write_stdin", performance.now(), "failed", "a".repeat(129));
    expect(info.mock.calls.map(([row]) => row)).toEqual([
      expect.objectContaining({ tool: "exec_command", outcome: "unavailable" }),
      expect.objectContaining({ tool: "write_stdin", outcome: "failed" }),
    ]);
    expect(info.mock.calls.every(([row]) => !("call_id" in row))).toBe(true);
  });
});
