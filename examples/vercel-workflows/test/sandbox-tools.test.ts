import type { Sandbox } from "@vercel/sandbox";
import { describe, expect, it, vi } from "vitest";

import {
  createVercelSandboxTools,
  workspacePath,
} from "../workflows/sandbox-tools";

const MIB = 1024 * 1024;
const OUTPUT_LIMIT = 128 * 1024;
const context = {
  callId: "call",
  parentCallId: "parent",
  sessionId: "session",
  signal: new AbortController().signal,
};

describe("Vercel Sandbox workspace paths", () => {
  it("canonicalizes paths under the physical workspace", () => {
    expect(workspacePath(".")).toBe("/vercel/sandbox");
    expect(workspacePath("././")).toBe("/vercel/sandbox");
    expect(workspacePath("src//./index.ts")).toBe("/vercel/sandbox/src/index.ts");
    expect(workspacePath("/workspace/out.txt")).toBe("/vercel/sandbox/out.txt");
    expect(workspacePath("/vercel/sandbox/out.txt")).toBe("/vercel/sandbox/out.txt");
  });

  it.each([
    "",
    "../secret",
    "safe/../../secret",
    "/workspace/../secret",
    "/workspace2/secret",
    "/etc/passwd",
    "nul\0byte",
    "x".repeat(1025),
  ])("rejects an invalid or escaping path: %s", (path) => {
    expect(() => workspacePath(path)).toThrow();
  });
});

describe("Vercel Sandbox tools", () => {
  it("runs bounded commands and preserves non-zero results", async () => {
    const sandbox = makeSandbox({
      runCommand: vi.fn(async () => finishedCommand({
        exitCode: 7,
        stdout: "partial",
        stderr: "failed",
        durationMs: 42,
      })),
    });
    const tools = createVercelSandboxTools(async () => sandbox.client);

    await expect(invoke(tools, "sandbox_exec", { command: "false" })).resolves.toEqual({
      success: false,
      exit_code: 7,
      stdout: "partial",
      stderr: "failed",
      stdout_truncated: false,
      stderr_truncated: false,
      duration_ms: 42,
    });
    expect(sandbox.runCommand).toHaveBeenCalledWith({
      cmd: "bash",
      args: ["-lc", "false"],
      cwd: "/vercel/sandbox",
      timeoutMs: 60_000,
    });
  });

  it("caps command output by UTF-8 bytes without splitting code points", async () => {
    const sandbox = makeSandbox({
      runCommand: vi.fn(async () => finishedCommand({
        stdout: "é".repeat(OUTPUT_LIMIT),
        stderr: "x".repeat(OUTPUT_LIMIT + 1),
      })),
    });
    const result = await invoke(
      createVercelSandboxTools(async () => sandbox.client),
      "sandbox_exec",
      { command: "produce-output" },
    );

    expect(Buffer.byteLength(result.stdout, "utf8")).toBe(OUTPUT_LIMIT);
    expect(result.stdout.endsWith("é")).toBe(true);
    expect(result.stdout_truncated).toBe(true);
    expect(result.stderr_truncated).toBe(true);
  });

  it("writes empty and exactly-1-MiB UTF-8 files", async () => {
    const sandbox = makeSandbox();
    const tools = createVercelSandboxTools(async () => sandbox.client);

    await expect(invoke(tools, "sandbox_write_file", {
      path: "empty.txt",
      content: "",
    })).resolves.toEqual({ path: "/workspace/empty.txt", bytes_written: 0 });
    const exact = "😀".repeat(MIB / 4);
    await expect(invoke(tools, "sandbox_write_file", {
      path: "unicode.txt",
      content: exact,
    })).resolves.toEqual({ path: "/workspace/unicode.txt", bytes_written: MIB });
    expect(sandbox.writeFile).toHaveBeenLastCalledWith(
      "/vercel/sandbox/unicode.txt",
      exact,
      "utf8",
    );
  });

  it("rejects oversized writes and invalid UTF-8 reads", async () => {
    const sandbox = makeSandbox({ readFile: vi.fn(async () => Buffer.from([0xc3, 0x28])) });
    const tools = createVercelSandboxTools(async () => sandbox.client);

    await expect(invoke(tools, "sandbox_write_file", {
      path: "large.txt",
      content: "😀".repeat(MIB / 4 + 1),
    })).rejects.toThrow("content exceeds 1 MiB");
    await expect(invoke(tools, "sandbox_read_file", { path: "binary.dat" })).rejects.toThrow(
      "file is not valid UTF-8",
    );
  });

  it("starts a native detached command and waits for readiness", async () => {
    const detached = { cmdId: "cmd-1" };
    const runCommand = vi.fn(async (params: { detached?: boolean }) => (
      params.detached ? detached : finishedCommand()
    ));
    const sandbox = makeSandbox({ runCommand });
    const tools = createVercelSandboxTools(async () => sandbox.client);

    await expect(invoke(tools, "sandbox_start_process", {
      command: "python3 -m http.server 3000 --directory .",
      cwd: "/workspace",
      ready_port: 3000,
      ready_timeout_ms: 12_345,
    })).resolves.toEqual({
      process_id: "cmd-1",
      command: "python3 -m http.server 3000 --directory .",
      status: "running",
      ready_port: 3000,
    });
    expect(runCommand).toHaveBeenNthCalledWith(1, {
      cmd: "bash",
      args: ["-lc", "python3 -m http.server 3000 --directory ."],
      cwd: "/vercel/sandbox",
      detached: true,
    });
    expect(runCommand).toHaveBeenNthCalledWith(2, {
      cmd: "bash",
      args: ["-lc", "until (exec 3<>/dev/tcp/127.0.0.1/3000) 2>/dev/null; do sleep 0.1; done"],
      timeoutMs: 12_345,
    });
  });

  it("rejects malformed ports before creating a sandbox", async () => {
    const factory = vi.fn(async () => makeSandbox().client);
    const tools = createVercelSandboxTools(factory);
    await expect(invoke(tools, "sandbox_start_process", {
      command: "node server.js",
      ready_port: 80,
    })).rejects.toThrow("ready_port must be an integer");
    await expect(invoke(tools, "sandbox_preview", { port: 3001 })).rejects.toThrow(
      "port must be one of",
    );
    expect(factory).not.toHaveBeenCalled();
  });

  it("returns only configured Vercel preview domains", async () => {
    const sandbox = makeSandbox();
    const tools = createVercelSandboxTools(async () => sandbox.client);
    await expect(invoke(tools, "sandbox_preview", { port: 8080 })).resolves.toEqual({
      port: 8080,
      url: "https://sb-8080.vercel.run",
      persistent: false,
    });
    expect(sandbox.domain).toHaveBeenCalledWith(8080);
  });
});

function makeSandbox(overrides: {
  readFile?: ReturnType<typeof vi.fn>;
  runCommand?: ReturnType<typeof vi.fn>;
} = {}) {
  const readFile = overrides.readFile ?? vi.fn(async () => Buffer.from("hello"));
  const writeFile = vi.fn(async () => {});
  const mkdir = vi.fn(async () => undefined);
  const readdir = vi.fn(async () => []);
  const stat = vi.fn(async () => ({ isFile: () => true, size: 5 }));
  const runCommand = overrides.runCommand ?? vi.fn(async () => finishedCommand());
  const domain = vi.fn((port: number) => `https://sb-${port}.vercel.run`);
  const client = {
    domain,
    fs: { mkdir, readFile, readdir, stat, writeFile },
    runCommand,
  } as unknown as Pick<Sandbox, "domain" | "fs" | "runCommand">;
  return { client, domain, mkdir, readFile, readdir, runCommand, stat, writeFile };
}

function finishedCommand(overrides: {
  exitCode?: number;
  stdout?: string;
  stderr?: string;
  durationMs?: number;
} = {}) {
  return {
    exitCode: overrides.exitCode ?? 0,
    durationMs: overrides.durationMs ?? 1,
    stdout: vi.fn(async () => overrides.stdout ?? ""),
    stderr: vi.fn(async () => overrides.stderr ?? ""),
  };
}

async function invoke(
  tools: ReturnType<typeof createVercelSandboxTools>,
  name: string,
  input: unknown,
): Promise<any> {
  const tool = tools[name];
  if (!tool) throw new Error(`missing tool: ${name}`);
  return tool.handler(input, context);
}
