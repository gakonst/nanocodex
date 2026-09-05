import { afterEach, describe, expect, test, vi } from "vitest";

import {
  agentOsRuntimeOptions,
  createRivetSandboxTools,
  restoreRivetPreviewServers,
  workspacePath,
} from "../src/sandbox-tools.js";

const MIB = 1024 * 1024;
const OUTPUT_LIMIT = 128 * 1024;
const databaseExecute = vi.fn(async (sql: string, ...bindings: unknown[]) => (
  sql.startsWith("SELECT port FROM nanocodex_preview_servers WHERE port")
    ? [{ port: bindings[0] }]
    : []
));
const durableSpawn = vi.fn(async () => ({ pid: 41, state: "running", startedAtMs: 1 }));
const context = {
  actorId: "actor-123",
  db: { execute: databaseExecute },
  client: () => ({
    nanocodex: {
      getForId: () => ({ process: { spawn: durableSpawn } }),
    },
  }),
} as unknown as Parameters<typeof createRivetSandboxTools>[0];

afterEach(() => {
  vi.clearAllMocks();
  vi.unstubAllEnvs();
});

describe("Rivet AgentOS workspace paths", () => {
  test("keeps JavaScript listeners alive beyond the signed preview lifetime", () => {
    expect(agentOsRuntimeOptions.limits.jsRuntime).toEqual({
      cpuTimeLimitMs: 960_000,
      wallClockLimitMs: 960_000,
    });
  });

  test("canonicalizes paths under the workspace", () => {
    expect(workspacePath(".")).toBe("/workspace");
    expect(workspacePath("././")).toBe("/workspace");
    expect(workspacePath("src//./index.ts")).toBe("/workspace/src/index.ts");
    expect(workspacePath("/workspace/out.txt")).toBe("/workspace/out.txt");
  });

  test.each([
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

describe("Rivet AgentOS tools", () => {
  test("runs bounded commands and preserves non-zero results", async () => {
    const actions = makeActions({
      exec: vi.fn(async () => ({ exitCode: 7, stdout: "partial", stderr: "failed" })),
    });
    const tools = createRivetSandboxTools(context, actions.value);

    await expect(invoke(tools, "sandbox_exec", { command: "false" })).resolves.toEqual({
      success: false,
      exit_code: 7,
      stdout: "partial",
      stderr: "failed",
      stdout_truncated: false,
      stderr_truncated: false,
    });
    expect(actions.exec).toHaveBeenCalledWith(context, "false", {
      cwd: "/workspace",
      timeout: 60_000,
      captureStdio: true,
    });
  });

  test("caps command output by UTF-8 bytes without splitting code points", async () => {
    const actions = makeActions({
      exec: vi.fn(async () => ({
        exitCode: 0,
        stdout: "é".repeat(OUTPUT_LIMIT),
        stderr: "x".repeat(OUTPUT_LIMIT + 1),
      })),
    });
    const result = await invoke(
      createRivetSandboxTools(context, actions.value),
      "sandbox_exec",
      { command: "produce-output" },
    );

    expect(Buffer.byteLength(result.stdout, "utf8")).toBe(OUTPUT_LIMIT);
    expect(result.stdout.endsWith("é")).toBe(true);
    expect(result.stdout_truncated).toBe(true);
    expect(result.stderr_truncated).toBe(true);
  });

  test("writes empty and exactly-1-MiB UTF-8 files", async () => {
    const actions = makeActions();
    const tools = createRivetSandboxTools(context, actions.value);

    await expect(invoke(tools, "sandbox_write_file", {
      path: "empty.txt",
      content: "",
    })).resolves.toEqual({ path: "/workspace/empty.txt", bytes_written: 0 });
    const exact = "😀".repeat(MIB / 4);
    await expect(invoke(tools, "sandbox_write_file", {
      path: "unicode.txt",
      content: exact,
    })).resolves.toEqual({ path: "/workspace/unicode.txt", bytes_written: MIB });
    expect(actions.writeFile).toHaveBeenLastCalledWith(
      context,
      "/workspace/unicode.txt",
      exact,
    );
  });

  test("rejects oversized writes and invalid UTF-8 reads", async () => {
    const actions = makeActions({
      readFile: vi.fn(async () => new Uint8Array([0xc3, 0x28])),
    });
    const tools = createRivetSandboxTools(context, actions.value);

    await expect(invoke(tools, "sandbox_write_file", {
      path: "large.txt",
      content: "😀".repeat(MIB / 4 + 1),
    })).rejects.toThrow("content exceeds 1 MiB");
    await expect(invoke(tools, "sandbox_read_file", { path: "binary.dat" })).rejects.toThrow(
      "file is not valid UTF-8",
    );
  });

  test("starts a native process and waits for its HTTP port", async () => {
    const actions = makeActions();
    const tools = createRivetSandboxTools(context, actions.value);

    await expect(invoke(tools, "sandbox_start_process", {
      command: "node",
      args: ["/workspace/server.mjs"],
      ready_port: 3000,
      ready_timeout_ms: 12_345,
    })).resolves.toEqual({
      process_id: 41,
      command: "node",
      args: ["/workspace/server.mjs"],
      status: "running",
      ready_port: 3000,
    });
    expect(durableSpawn).toHaveBeenCalledWith(
      "node",
      ["/workspace/server.mjs"],
      { cwd: "/workspace", output: { retainEvents: true } },
    );
    expect(actions.vmFetch).toHaveBeenCalledWith(context, 3000, "http://127.0.0.1/");
    expect(databaseExecute).toHaveBeenCalledWith(
      expect.stringContaining("INSERT INTO nanocodex_preview_servers"),
      3000,
      "node",
      '["/workspace/server.mjs"]',
      "/workspace",
    );
  });

  test("kills a process whose readiness probe times out", async () => {
    const actions = makeActions({
      vmFetch: vi.fn(async () => { throw new Error("connection refused"); }),
    });
    const tools = createRivetSandboxTools(context, actions.value);

    await expect(invoke(tools, "sandbox_start_process", {
      command: "exit 1",
      args: [],
      ready_port: 3000,
      ready_timeout_ms: 1,
    })).rejects.toThrow("port 3000 did not become ready");
    expect(actions.killProcess).toHaveBeenCalledWith(context, 41);
  });

  test("returns an absolute signed preview when a public endpoint is configured", async () => {
    vi.stubEnv(
      "NANOCODEX_PUBLIC_URL",
      "https://nanocodex-production:pk_public@example.rivet.dev",
    );
    const actions = makeActions();
    const tools = createRivetSandboxTools(context, actions.value);

    await expect(invoke(tools, "sandbox_preview", {
      port: 3000,
      ttl_seconds: 600,
    })).resolves.toEqual({
      port: 3000,
      path: "/fetch/preview-token",
      url: "https://example.rivet.dev/gateway/actor-123@pk_public/request/fetch/preview-token",
      expires_at: "2030-01-01T00:00:00.000Z",
      persistent: false,
    });
    expect(actions.createPreviewUrl).toHaveBeenCalledWith(context, 3000, 600);
    expect(actions.vmFetch).toHaveBeenCalledWith(context, 3000, "http://127.0.0.1/");
    expect(databaseExecute).toHaveBeenCalledWith(
      "UPDATE nanocodex_preview_servers SET expires_at_ms = ? WHERE port = ?",
      Date.parse("2030-01-01T00:00:00.000Z"),
      3000,
    );
  });

  test("refuses to sign a preview for an unreachable port", async () => {
    const actions = makeActions({
      vmFetch: vi.fn(async () => { throw new Error("connection refused"); }),
    });

    await expect(invoke(
      createRivetSandboxTools(context, actions.value),
      "sandbox_preview",
      { port: 3000 },
    )).rejects.toThrow("port 3000 is not reachable");
    expect(actions.createPreviewUrl).not.toHaveBeenCalled();
  });

  test("bounds preview lifetime to the actor's keep-alive window", async () => {
    const actions = makeActions();

    await expect(invoke(
      createRivetSandboxTools(context, actions.value),
      "sandbox_preview",
      { port: 3000, ttl_seconds: 901 },
    )).rejects.toThrow("ttl_seconds must be an integer between 60 and 900");
    expect(actions.createPreviewUrl).not.toHaveBeenCalled();
  });

  test("refuses to put a secret Rivet token in a preview URL", async () => {
    vi.stubEnv(
      "NANOCODEX_PUBLIC_URL",
      "https://nanocodex-production:sk_secret@example.rivet.dev",
    );
    const actions = makeActions();

    await expect(invoke(
      createRivetSandboxTools(context, actions.value),
      "sandbox_preview",
      { port: 3000 },
    )).rejects.toThrow("client-safe pk_ token");
  });
});

describe("Rivet AgentOS preview recovery", () => {
  test("restarts an unexpired preview listener when its VM wakes", async () => {
    const execute = vi.fn(async (sql: string) => (
      sql.includes("SELECT port, command")
        ? [{
            port: 3000,
            command: "node",
            args_json: '["/workspace/server.mjs"]',
            cwd: "/workspace",
          }]
        : []
    ));
    const wakeContext = {
      db: { execute },
      log: { error: vi.fn() },
    } as unknown as Parameters<typeof restoreRivetPreviewServers>[0];
    const vm = {
      process: { spawn: vi.fn(async () => ({ pid: 42 })) },
      network: { httpRequest: vi.fn(async () => ({ status: 200 })) },
    } as unknown as Parameters<typeof restoreRivetPreviewServers>[1];

    await restoreRivetPreviewServers(wakeContext, vm);

    expect(vm.process.spawn).toHaveBeenCalledWith(
      "node",
      ["/workspace/server.mjs"],
      { cwd: "/workspace", output: { retainEvents: true } },
    );
    expect(vm.network.httpRequest).toHaveBeenCalledWith({
      port: 3000,
      path: "/",
      method: "GET",
      headers: {},
    });
  });
});

function makeActions(overrides: Record<string, unknown> = {}) {
  const actions = {
    createPreviewUrl: vi.fn(async () => ({
      path: "/fetch/preview-token",
      token: "preview-token",
      port: 3000,
      expiresAt: Date.parse("2030-01-01T00:00:00.000Z"),
    })),
    exec: vi.fn(async () => ({ exitCode: 0, stdout: "", stderr: "" })),
    killProcess: vi.fn(async () => undefined),
    mkdir: vi.fn(async () => undefined),
    readFile: vi.fn(async () => new TextEncoder().encode("hello")),
    readdirEntries: vi.fn(async () => []),
    vmFetch: vi.fn(async () => ({
      status: 200,
      statusText: "OK",
      headers: {},
      body: new Uint8Array(),
    })),
    writeFile: vi.fn(async () => undefined),
    ...overrides,
  };
  return {
    ...actions,
    value: actions as unknown as Parameters<typeof createRivetSandboxTools>[1],
  };
}

async function invoke(
  tools: ReturnType<typeof createRivetSandboxTools>,
  name: string,
  input: unknown,
): Promise<any> {
  const tool = tools[name];
  if (!tool) throw new Error(`missing tool: ${name}`);
  return tool.handler(input, {
    callId: "call",
    parentCallId: "parent",
    sessionId: "session",
    signal: new AbortController().signal,
  });
}
