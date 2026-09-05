import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  prepareSessionSandbox: vi.fn(),
}));

vi.mock("@/workflows/session-sandbox", () => ({
  prepareSessionSandbox: mocks.prepareSessionSandbox,
}));

import { POST } from "../app/api/sessions/[sessionId]/terminal/route";

const originalToken = process.env.NANOCODEX_TERMINAL_TOKEN;

describe("workspace terminal attach route", () => {
  beforeEach(() => {
    process.env.NANOCODEX_TERMINAL_TOKEN = "terminal-secret";
    mocks.prepareSessionSandbox.mockReset();
  });

  afterEach(() => {
    if (originalToken === undefined) delete process.env.NANOCODEX_TERMINAL_TOKEN;
    else process.env.NANOCODEX_TERMINAL_TOKEN = originalToken;
  });

  it("returns a fresh interactive credential for the session Sandbox", async () => {
    const openInteractive = vi.fn(async () => ({
      url: "wss://controller.example/pty",
      token: "one-time-token",
    }));
    mocks.prepareSessionSandbox.mockResolvedValue({ openInteractive });

    const response = await request("wrun_abc", "Bearer terminal-secret");
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    await expect(response.json()).resolves.toEqual({
      url: "wss://controller.example/pty",
      token: "one-time-token",
    });
    expect(mocks.prepareSessionSandbox).toHaveBeenCalledWith("wrun_abc");
    expect(openInteractive).toHaveBeenCalledOnce();
  });

  it("does not resolve a Sandbox before authorization succeeds", async () => {
    const response = await request("wrun_abc", "Bearer wrong");
    expect(response.status).toBe(401);
    expect(mocks.prepareSessionSandbox).not.toHaveBeenCalled();
  });

  it("fails closed when terminal access is disabled", async () => {
    delete process.env.NANOCODEX_TERMINAL_TOKEN;
    const response = await request("wrun_abc", "Bearer terminal-secret");
    expect(response.status).toBe(503);
    expect(mocks.prepareSessionSandbox).not.toHaveBeenCalled();
  });
});

function request(sessionId: string, authorization: string): Promise<Response> {
  return POST(
    new Request(`https://example.test/api/sessions/${sessionId}/terminal`, {
      method: "POST",
      headers: { authorization },
    }),
    { params: Promise.resolve({ sessionId }) },
  );
}
