import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  getOrCreate: vi.fn(),
}));

vi.mock("@vercel/sandbox", () => ({
  Sandbox: { getOrCreate: mocks.getOrCreate },
}));

import { prepareSessionSandbox } from "../workflows/session-sandbox";

describe("shared Vercel Sandbox session", () => {
  beforeEach(() => {
    mocks.getOrCreate.mockReset();
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
