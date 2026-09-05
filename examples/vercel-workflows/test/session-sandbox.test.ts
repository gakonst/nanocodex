import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  getOrCreate: vi.fn(),
}));

vi.mock("@vercel/sandbox", () => ({
  Sandbox: { getOrCreate: mocks.getOrCreate },
}));

import {
  prepareSessionSandbox,
  sessionSandboxName,
} from "../workflows/session-sandbox";

describe("shared Vercel Sandbox session", () => {
  beforeEach(() => {
    mocks.getOrCreate.mockReset();
  });

  it("uses the same deterministic persistent Sandbox for tools and terminal", async () => {
    const runCommand = vi.fn(async () => ({
      exitCode: 0,
      stderr: vi.fn(async () => ""),
    }));
    const sandbox = { runCommand };
    mocks.getOrCreate.mockResolvedValue(sandbox);

    await expect(prepareSessionSandbox("wrun_abc-123")).resolves.toBe(sandbox);
    expect(sessionSandboxName("wrun_abc-123")).toBe("nanocodex-wrun_abc-123");
    expect(mocks.getOrCreate).toHaveBeenCalledWith({
      name: "nanocodex-wrun_abc-123",
      runtime: "node24",
      persistent: true,
      timeout: 600_000,
      ports: [3000, 5173, 8000, 8080],
      keepLastSnapshots: { count: 3, expiration: 604_800_000 },
      tags: { application: "nanocodex", session: "wrun_abc-123" },
    });
    expect(runCommand).toHaveBeenCalledWith(expect.objectContaining({
      cmd: "bash",
      sudo: true,
      timeoutMs: 10_000,
    }));
  });

  it("rejects a Sandbox whose workspace alias cannot be prepared", async () => {
    mocks.getOrCreate.mockResolvedValue({
      runCommand: vi.fn(async () => ({
        exitCode: 1,
        stderr: vi.fn(async () => "bad link"),
      })),
    });
    await expect(prepareSessionSandbox("wrun_failed"))
      .rejects.toThrow("failed to prepare /workspace: bad link");
  });
});
