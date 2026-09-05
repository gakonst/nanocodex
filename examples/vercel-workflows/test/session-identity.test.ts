import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  getOrCreate: vi.fn(),
}));

vi.mock("@vercel/sandbox", () => ({
  Sandbox: { getOrCreate: mocks.getOrCreate },
}));

import { POST as attachTerminal } from "../app/api/sessions/[sessionId]/terminal/route";
import { vercelSandboxTools } from "../workflows/sandbox-tools";

const originalTerminalToken = process.env.NANOCODEX_TERMINAL_TOKEN;
const PUBLIC_SESSION_ID = "wrun_canonical";
const MARKER = "agent-and-terminal-share-this-file";

describe("canonical Workflow and Sandbox session identity", () => {
  beforeEach(() => {
    process.env.NANOCODEX_TERMINAL_TOKEN = "terminal-secret";
    mocks.getOrCreate.mockReset();
  });

  afterEach(() => {
    if (originalTerminalToken === undefined) delete process.env.NANOCODEX_TERMINAL_TOKEN;
    else process.env.NANOCODEX_TERMINAL_TOKEN = originalTerminalToken;
  });

  it("exposes an agent-written file through a terminal attachment", async () => {
    const sandboxes = new Map<string, ReturnType<typeof fakeSandbox>>();
    mocks.getOrCreate.mockImplementation(async ({ name }: { name: string }) => {
      let sandbox = sandboxes.get(name);
      if (!sandbox) {
        sandbox = fakeSandbox();
        sandboxes.set(name, sandbox);
      }
      return sandbox;
    });

    const tools = vercelSandboxTools(PUBLIC_SESSION_ID);
    await tools.sandbox_write_file!.handler(
      { path: "identity.txt", content: MARKER },
      {
        callId: "write",
        parentCallId: "turn",
        sessionId: PUBLIC_SESSION_ID,
        signal: new AbortController().signal,
      },
    );

    const response = await attachTerminal(
      new Request(`https://example.test/api/sessions/${PUBLIC_SESSION_ID}/terminal`, {
        method: "POST",
        headers: { authorization: "Bearer terminal-secret" },
      }),
      { params: Promise.resolve({ sessionId: PUBLIC_SESSION_ID }) },
    );

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toEqual({
      url: "wss://controller.example/pty",
      token: "interactive-token",
    });
    expect(sandboxes.size).toBe(1);
    const sandbox = sandboxes.get("nanocodex-wrun_canonical");
    expect(sandbox?.terminalReads).toEqual([MARKER]);
    expect(mocks.getOrCreate).toHaveBeenCalledTimes(2);
    expect(mocks.getOrCreate.mock.calls.map(([options]) => options.name)).toEqual([
      "nanocodex-wrun_canonical",
      "nanocodex-wrun_canonical",
    ]);
  });
});

function fakeSandbox() {
  const files = new Map<string, string>();
  const terminalReads: string[] = [];
  return {
    domain: (port: number) => `https://sandbox-${port}.vercel.run`,
    fs: {
      mkdir: vi.fn(async () => undefined),
      readFile: vi.fn(async (path: string) => Buffer.from(files.get(path) ?? "")),
      readdir: vi.fn(async () => []),
      stat: vi.fn(async (path: string) => ({
        isFile: () => files.has(path),
        size: Buffer.byteLength(files.get(path) ?? "", "utf8"),
      })),
      writeFile: vi.fn(async (path: string, content: string) => {
        files.set(path, content);
      }),
    },
    openInteractive: vi.fn(async () => {
      terminalReads.push(files.get("/vercel/sandbox/identity.txt") ?? "");
      return {
        url: "wss://controller.example/pty",
        token: "interactive-token",
      };
    }),
    runCommand: vi.fn(async () => ({
      exitCode: 0,
      stderr: async () => "",
    })),
    terminalReads,
  };
}
