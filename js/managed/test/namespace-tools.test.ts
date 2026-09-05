import { describe, expect, it, vi } from "vitest";
import type { ToolMap } from "nanocodex";
// @ts-expect-error The runtime subpath is intentionally JavaScript-only.
import { ToolRouter, toolMapSource } from "nanocodex-tools/runtime/tool-router";

import {
  createNamespaceExecutionRuntime,
  createNamespaceExecutionTools as createRuntimeNamespaceExecutionTools,
  machineMountRoot,
} from "../src/namespace-tools";

const context = (overrides: Partial<{
  sessionId: string;
  parentCallId: string;
  callId: string;
}> = {}) => ({
  callId: overrides.callId ?? "call",
  model: "gpt-5.6-sol",
  parentCallId: overrides.parentCallId ?? "cell",
  sessionId: overrides.sessionId ?? "root-session",
  signal: new AbortController().signal,
});

describe("cwd-root namespace execution", () => {
  it("starts with no executable hand and fails closed at the brain cwd", async () => {
    const tools = createRuntimeNamespaceExecutionTools(() => []);

    await expect(tools.exec_command!.handler({ cmd: "pwd" }, context()))
      .rejects.toThrow("call mount when native execution is needed");
  });

  it("keeps canonical schemas and routes an explicit logical cwd to a sandbox hand", async () => {
    const sandboxExec = vi.fn(async () => ({
      output: "/workspace\n",
      wall_time_seconds: 0.01,
      exit_code: 0,
    }));
    const tools = createNamespaceExecutionTools(sandboxTools(sandboxExec), () => []);

    expect(tools.exec_command!.parameters).toMatchObject({
      required: ["cmd"],
      additionalProperties: false,
    });
    expect(JSON.stringify(tools.exec_command!.parameters)).not.toContain("environment");
    expect(tools.write_stdin!.parameters).toMatchObject({
      required: ["session_id"],
      additionalProperties: false,
    });
    expect(JSON.stringify(tools.write_stdin!.parameters)).not.toMatch(/environment|host/);

    await tools.exec_command!.handler({ cmd: "pwd", workdir: "/sandbox" }, context());
    expect(sandboxExec).toHaveBeenCalledWith(
      { cmd: "pwd", workdir: "/workspace" },
      expect.objectContaining({ sessionId: "root-session" }),
    );
  });

  it("routes by a portable machine mount and translates only the workdir", async () => {
    const exec = vi.fn(async () => ({ output: "ok", wall_time_seconds: 0, exit_code: 0 }));
    const resolve = vi.fn((_id: string, name: string) => (
      name === "exec_command" ? { handler: exec } : { handler: vi.fn() }
    ));
    const tools = createNamespaceExecutionTools(
      sandboxTools(),
      () => [{
        id: "laptop",
        workspace: "/Users/me/repo",
      }],
      resolve,
    );

    await tools.exec_command!.handler({
      cmd: "cargo test",
      workdir: "/laptop/crates/core",
      yield_time_ms: 30_000,
    }, context());

    expect(exec).toHaveBeenCalledWith({
      cmd: "cargo test",
      workdir: "/Users/me/repo/crates/core",
      yield_time_ms: 30_000,
    }, expect.anything());
    expect(resolve).toHaveBeenCalledWith("laptop", "exec_command", expect.anything());
  });

  it("lets the real tool router dispatch separate hands concurrently", async () => {
    let active = 0;
    let maxActive = 0;
    const exec = vi.fn(async () => {
      active += 1;
      maxActive = Math.max(maxActive, active);
      await new Promise((resolve) => setTimeout(resolve, 25));
      active -= 1;
      return { output: "ok", wall_time_seconds: 0.025, exit_code: 0 };
    });
    const tools = createRuntimeNamespaceExecutionTools(
      () => [
        { id: "hand-a", workspace: "/workspace" },
        { id: "hand-b", workspace: "/workspace" },
      ],
      (_id, name) => name === "exec_command" ? { handler: exec } : undefined,
    );
    const router = new ToolRouter([toolMapSource("namespace", tools)]);

    await Promise.all([
      router.execute("exec_command", { cmd: "one", workdir: "/hand-a" }, context({ callId: "one" })),
      router.execute("exec_command", { cmd: "two", workdir: "/hand-b" }, context({ callId: "two" })),
    ]);

    expect(tools.exec_command!.supportsParallelToolCalls).toBe(true);
    expect(maxActive).toBe(2);
  });

  it("captures one immutable machine binding per Code Mode cell", async () => {
    let machines = [{ id: "laptop", workspace: "/old" }];
    const oldExec = vi.fn(async (_input: unknown) => ({ output: "old", wall_time_seconds: 0, exit_code: 0 }));
    const newExec = vi.fn(async (_input: unknown) => ({ output: "new", wall_time_seconds: 0, exit_code: 0 }));
    let generation = "old";
    const tools = createNamespaceExecutionTools(
      sandboxTools(),
      () => machines,
      (_id, name) => ({
        handler: name === "exec_command"
          ? generation === "old" ? oldExec : newExec
          : vi.fn(),
      }),
    );

    await tools.exec_command!.handler({ cmd: "one", workdir: "/laptop" }, context());
    generation = "new";
    machines = [{ id: "laptop", workspace: "/new" }];
    await tools.exec_command!.handler({ cmd: "two", workdir: "/laptop" }, context({ callId: "two" }));
    await tools.exec_command!.handler(
      { cmd: "three", workdir: "/laptop" },
      context({ parentCallId: "next-cell", callId: "three" }),
    );

    expect(oldExec).toHaveBeenCalledTimes(2);
    expect(oldExec.mock.calls[1]![0]).toMatchObject({ workdir: "/old" });
    expect(newExec).toHaveBeenCalledTimes(1);
    expect(newExec.mock.calls[0]![0]).toMatchObject({ workdir: "/new" });
  });

  it("captures a fresh binding for each top-level call while retaining the same call on replay", async () => {
    const oldExec = vi.fn(async () => ({ output: "old", wall_time_seconds: 0, exit_code: 0 }));
    const newExec = vi.fn(async () => ({ output: "new", wall_time_seconds: 0, exit_code: 0 }));
    let currentExec = oldExec;
    const tools = createRuntimeNamespaceExecutionTools(
      () => [{ id: "laptop", workspace: "/workspace" }],
      (_id, name) => name === "exec_command" ? { handler: currentExec } : undefined,
    );
    const firstCall = context({ parentCallId: "", callId: "first-call" });
    const secondCall = context({ parentCallId: "", callId: "second-call" });

    await expect(tools.exec_command!.handler({ cmd: "pwd", workdir: "/laptop" }, firstCall))
      .resolves.toMatchObject({ output: "old" });
    currentExec = newExec; // The same machine reconnects with a new attachment lease.
    await expect(tools.exec_command!.handler({ cmd: "pwd", workdir: "/laptop" }, secondCall))
      .resolves.toMatchObject({ output: "new" });
    await expect(tools.exec_command!.handler({ cmd: "pwd", workdir: "/laptop" }, firstCall))
      .resolves.toMatchObject({ output: "old" });
    expect(oldExec).toHaveBeenCalledTimes(2);
    expect(newExec).toHaveBeenCalledTimes(1);
  });

  it("keeps a mount created after capture out of the calling cell", async () => {
    let machines: readonly { id: string; root: string; workspace: string }[] = [];
    const exec = vi.fn(async () => ({ output: "mounted", wall_time_seconds: 0, exit_code: 0 }));
    const runtime = createNamespaceExecutionRuntime(
      () => machines,
      (_id, name) => name === "exec_command" ? { handler: exec } : undefined,
    );
    runtime.capture(context());
    machines = [{ id: "sandbox:mounted", root: "/mnt-test-12345678", workspace: "/workspace" }];

    await expect(runtime.tools.exec_command!.handler({
      cmd: "pwd",
      workdir: "/mnt-test-12345678",
    }, context())).rejects.toThrow("no mount owns");
    await expect(runtime.tools.exec_command!.handler({
      cmd: "pwd",
      workdir: "/mnt-test-12345678",
    }, context({ parentCallId: "later-cell" }))).resolves.toMatchObject({ output: "mounted" });
  });

  it("rebinds provider-local sessions and rejects cross-agent use", async () => {
    const machineWrite = vi.fn(async ({ session_id }: { session_id: number }) => ({
      output: "more",
      wall_time_seconds: 0,
      session_id,
    }));
    const tools = createNamespaceExecutionTools(
      sandboxTools(),
      () => [{ id: "buildbox", workspace: "/srv/repo" }],
      (_id, name) => ({
        handler: name === "exec_command"
          ? vi.fn(async () => ({ output: "start", wall_time_seconds: 0, session_id: 7 }))
          : name === "write_stdin" ? machineWrite : vi.fn(),
      }),
    );
    const started = await tools.exec_command!.handler(
      { cmd: "long", workdir: "/buildbox" },
      context(),
    ) as { session_id: number };
    expect(started.session_id).not.toBe(7);

    const polled = await tools.write_stdin!.handler(
      { session_id: started.session_id },
      context({ callId: "poll" }),
    ) as { session_id: number };
    expect(polled.session_id).toBe(started.session_id);
    expect(machineWrite).toHaveBeenCalledWith(
      { session_id: 7 },
      expect.anything(),
    );
    await expect(tools.write_stdin!.handler(
      { session_id: started.session_id },
      context({ sessionId: "sibling-session", callId: "steal" }),
    )).rejects.toThrow("unknown or stale");
  });

  it("accepts many simultaneously retained process bindings", async () => {
    let providerSessionId = 0;
    const exec = vi.fn(async () => ({
      output: "started",
      wall_time_seconds: 0,
      session_id: ++providerSessionId,
    }));
    const writeStdin = vi.fn(async ({ session_id }: { session_id: number }) => ({
      output: "running",
      wall_time_seconds: 0,
      session_id,
    }));
    const tools = createNamespaceExecutionTools(
      sandboxTools(),
      () => [{ id: "buildbox", workspace: "/srv/repo" }],
      (_id, name) => ({
        handler: name === "exec_command"
          ? exec
          : name === "write_stdin" ? writeStdin : vi.fn(),
      }),
    );
    const retainedSessionCount = 256;
    const retained: number[] = [];

    for (let index = 0; index < retainedSessionCount; index += 1) {
      const result = await tools.exec_command!.handler(
        { cmd: `long-${index}`, workdir: "/buildbox" },
        context({ callId: `start-${index}` }),
      ) as { session_id: number };
      retained.push(result.session_id);
    }

    expect(new Set(retained).size).toBe(retainedSessionCount);
    await expect(tools.write_stdin!.handler(
      { session_id: retained.at(-1)! },
      context({ callId: "poll-last" }),
    )).resolves.toMatchObject({ session_id: retained.at(-1) });
    expect(writeStdin).toHaveBeenLastCalledWith(
      { session_id: retainedSessionCount },
      expect.anything(),
    );
  });

  it("retains every live Code Mode cell binding until lifecycle cleanup", async () => {
    let generation: "old" | "new" = "old";
    const oldExec = vi.fn(async () => ({ output: "old", wall_time_seconds: 0, exit_code: 0 }));
    const newExec = vi.fn(async () => ({ output: "new", wall_time_seconds: 0, exit_code: 0 }));
    const tools = createNamespaceExecutionTools(
      sandboxTools(),
      () => [{ id: "laptop", workspace: generation === "old" ? "/old" : "/new" }],
      (_id, name) => ({
        handler: name === "exec_command"
          ? generation === "old" ? oldExec : newExec
          : vi.fn(),
      }),
    );

    for (let index = 0; index < 256; index += 1) {
      await tools.exec_command!.handler(
        { cmd: `cell-${index}`, workdir: "/laptop" },
        context({ parentCallId: `cell-${index}`, callId: `call-${index}` }),
      );
    }
    generation = "new";
    await tools.exec_command!.handler(
      { cmd: "revisit", workdir: "/laptop" },
      context({ parentCallId: "cell-0", callId: "revisit" }),
    );

    expect(oldExec).toHaveBeenCalledTimes(257);
    expect(oldExec).toHaveBeenLastCalledWith(
      { cmd: "revisit", workdir: "/old" },
      expect.anything(),
    );
    expect(newExec).not.toHaveBeenCalled();
  });

  it("releases retained cells and process bindings with the owner session", async () => {
    let current = "old";
    const oldExec = vi.fn(async () => ({
      output: "started",
      wall_time_seconds: 0,
      session_id: 7,
    }));
    const newExec = vi.fn(async () => ({
      output: "rebound",
      wall_time_seconds: 0,
      exit_code: 0,
    }));
    const tools = createNamespaceExecutionTools(
      sandboxTools(),
      () => [{ id: "laptop", workspace: current === "old" ? "/old" : "/new" }],
      (_id, name) => ({
        handler: name === "exec_command"
          ? current === "old" ? oldExec : newExec
          : vi.fn(async () => ({ output: "more", wall_time_seconds: 0, session_id: 7 })),
      }),
    );
    const started = await tools.exec_command!.handler(
      { cmd: "long", workdir: "/laptop" },
      context(),
    ) as { session_id: number };

    tools.exec_command!.releaseSession?.("root-session");
    current = "new";

    await expect(tools.write_stdin!.handler(
      { session_id: started.session_id },
      context({ callId: "stale" }),
    )).rejects.toThrow("unknown or stale");
    await tools.exec_command!.handler(
      { cmd: "fresh", workdir: "/laptop" },
      context({ callId: "fresh" }),
    );
    expect(oldExec).toHaveBeenCalledTimes(1);
    expect(newExec).toHaveBeenCalledWith(
      { cmd: "fresh", workdir: "/new" },
      expect.anything(),
    );
  });

  it("lets sibling subagent sessions place work on separate hands concurrently", async () => {
    let release!: () => void;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    const started: string[] = [];
    const handler = (hand: string) => vi.fn(async () => {
      started.push(hand);
      await gate;
      return { output: hand, wall_time_seconds: 0.01, exit_code: 0 };
    });
    const laptop = handler("laptop");
    const buildbox = handler("buildbox");
    const tools = createNamespaceExecutionTools(
      sandboxTools(),
      () => [
        { id: "laptop", workspace: "/one" },
        { id: "buildbox", workspace: "/two" },
      ],
      (id, name) => ({
        handler: name === "exec_command"
          ? id === "laptop" ? laptop : buildbox
          : vi.fn(),
      }),
    );
    const child = (sessionId: string, agentId: string) => ({
      ...context({ sessionId, parentCallId: `cell-${agentId}`, callId: `call-${agentId}` }),
      subagent: {
        agentId,
        parentAgentId: null,
        sessionId,
        role: "builder",
        task: "test",
      },
    });

    const pending = Promise.all([
      tools.exec_command!.handler({ cmd: "test", workdir: "/laptop" }, child("child-a", "1")),
      tools.exec_command!.handler({ cmd: "test", workdir: "/buildbox" }, child("child-b", "2")),
    ]);
    await vi.waitFor(() => expect(started).toHaveLength(2));
    release();
    await expect(pending).resolves.toHaveLength(2);
    expect(laptop).toHaveBeenCalledTimes(1);
    expect(buildbox).toHaveBeenCalledTimes(1);
  });

  it("fails closed for unknown roots and derives safe deterministic mount names", async () => {
    const tools = createNamespaceExecutionTools(sandboxTools(), () => []);
    await expect(tools.exec_command!.handler(
      { cmd: "pwd", workdir: "/missing/repo" },
      context(),
    )).rejects.toThrow("no mount owns");
    expect(machineMountRoot("laptop")).toBe("/laptop");
    expect(machineMountRoot("Build Box")).toMatch(/^\/hand-build-box-[0-9a-f]{8}$/);
    expect(machineMountRoot("sandbox")).toMatch(/^\/hand-sandbox-/);
  });
});

function sandboxTools(
  exec = vi.fn(async () => ({ output: "", wall_time_seconds: 0, exit_code: 0 })),
): ToolMap {
  return {
    exec_command: {
      description: "sandbox exec",
      handler: exec,
    },
    write_stdin: {
      description: "sandbox stdin",
      handler: vi.fn(async () => ({ output: "", wall_time_seconds: 0, exit_code: 0 })),
    },
    preview: {
      description: "sandbox preview",
      handler: vi.fn(async () => ({ port: 3000, url: "https://preview", persistent: false })),
    },
  };
}

function createNamespaceExecutionTools(
  sandbox: ToolMap,
  machines: () => readonly { id: string; workspace: string }[],
  resolveMachineTool: Parameters<typeof createRuntimeNamespaceExecutionTools>[1] = () => undefined,
) {
  return createRuntimeNamespaceExecutionTools(
    () => [{ id: "sandbox", root: "/sandbox", workspace: "/workspace" }, ...machines()],
    (machineId, name, context) => machineId === "sandbox"
      ? sandbox[name]
      : resolveMachineTool(machineId, name, context),
  );
}
