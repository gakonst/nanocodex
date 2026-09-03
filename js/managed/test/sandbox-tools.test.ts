import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const sandboxSdk = vi.hoisted(() => ({ getSandbox: vi.fn() }));

vi.mock("@cloudflare/sandbox", () => ({ getSandbox: sandboxSdk.getSandbox }));

import {
  cloudflareSandboxPreviewUrl,
  cloudflareSandboxTools,
  createCloudflareSandboxTools,
  deleteCloudflareSandboxWorkspace,
  openSandboxPreviewCapability,
  proxyCloudflareSandboxPreview,
} from "../src/sandbox-tools";
import type { Sandbox } from "../src/sandbox-runtime";

const context = {
  callId: "call",
  model: "gpt-5.6-sol",
  parentCallId: "parent",
  sessionId: "session",
  signal: new AbortController().signal,
};

describe("Cloudflare sandbox tools", () => {
  beforeEach(() => sandboxSdk.getSandbox.mockReset());
  afterEach(() => vi.restoreAllMocks());

  it("asks the sandbox provider to prepare before every tool operation", async () => {
    const sandbox = fakeSandbox();
    const create = vi.fn(async () => sandbox);
    const tools = createCloudflareSandboxTools(create);

    await Promise.all([
      tools.sandbox_exec!.handler({ command: "uname -a" }, context),
      tools.sandbox_write_file!.handler({ path: "proof.txt", content: "ok" }, context),
    ]);

    expect(create).toHaveBeenCalledTimes(2);
    await tools.sandbox_list_files!.handler({}, context);
    expect(create).toHaveBeenCalledTimes(3);
    expect(sandbox.exec).toHaveBeenCalledWith("uname -a", {
      cwd: "/workspace",
    });
    expect(sandbox.exec.mock.calls.filter(([command]) => isWorkspaceFlush(command))).toHaveLength(2);
  });

  it("acknowledges workspace writes only after the retained mount flushes", async () => {
    const sandbox = fakeSandbox();
    const order: string[] = [];
    sandbox.writeFile.mockImplementation(async () => { order.push("write"); });
    sandbox.exec.mockImplementation(async (command: string) => {
      expect(command).toBe("sync -f /workspace");
      order.push("flush");
      return executionResult("");
    });
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.sandbox_write_file!.handler({
      path: "proof.txt",
      content: "retained",
    }, context)).resolves.toEqual({ path: "/workspace/proof.txt", bytes_written: 8 });

    expect(order).toEqual(["write", "flush"]);
  });

  it("fails a workspace mutation when its retained flush fails", async () => {
    const sandbox = fakeSandbox();
    sandbox.exec.mockResolvedValue({
      success: false,
      exitCode: 1,
      stdout: "",
      stderr: "transport failed",
      duration: 1,
    });
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.sandbox_write_file!.handler({
      path: "proof.txt",
      content: "not-yet-durable",
    }, context)).rejects.toThrow("failed to flush the retained workspace: transport failed");
  });

  it("attempts to flush partial writes when foreground execution rejects", async () => {
    const sandbox = fakeSandbox();
    const executionError = new Error("command transport stopped");
    sandbox.exec
      .mockRejectedValueOnce(executionError)
      .mockResolvedValueOnce(executionResult(""));
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.sandbox_exec!.handler({ command: "generate files" }, context))
      .rejects.toBe(executionError);
    expect(sandbox.exec.mock.calls).toEqual([
      ["generate files", { cwd: "/workspace" }],
      ["sync -f /workspace", { cwd: "/" }],
    ]);
  });

  it("prepares one shared sandbox once across concurrent parent and child tool sets", async () => {
    const namespace = fakeNamespace();
    const sandbox = preparingSandbox("empty");
    sandboxSdk.getSandbox.mockReturnValue(sandbox);
    const parent = cloudflareSandboxTools(namespace, "shared-session");
    const child = cloudflareSandboxTools(namespace, "shared-session");

    await Promise.all([
      parent.sandbox_exec!.handler({ command: "printf parent" }, context),
      child.sandbox_exec!.handler({ command: "printf child" }, {
        ...context,
        sessionId: "child-session",
        subagent: {
          agentId: "1",
          parentAgentId: null,
          sessionId: "child-session",
          role: "worker",
          task: "exercise the shared sandbox",
        },
      }),
    ]);

    expect(sandboxSdk.getSandbox).toHaveBeenCalledTimes(1);
    expect(sandbox.mountBucket).toHaveBeenCalledTimes(1);
    expect(sandbox.mountBucket).toHaveBeenCalledWith(
      "NANOCODEX_WORKSPACES",
      "/workspace",
      { prefix: "/sessions/shared-session/" },
    );
  });

  it("reuses a healthy retained workspace after the managed host reconnects", async () => {
    const sandbox = preparingSandbox("empty");
    sandboxSdk.getSandbox.mockReturnValue(sandbox);

    await cloudflareSandboxTools(fakeNamespace(), "retained-session")
      .sandbox_exec!.handler({ command: "printf first" }, context);
    await cloudflareSandboxTools(fakeNamespace(), "retained-session")
      .sandbox_exec!.handler({ command: "printf reconnected" }, context);

    expect(sandbox.mountBucket).toHaveBeenCalledTimes(1);
    expect(sandbox.exec.mock.calls.filter(([command]) => isMountProbe(command))).toHaveLength(3);
  });

  it("retries a transient retained mount health failure during background work", async () => {
    const sandbox = preparingSandbox("empty");
    const probeStates: MountState[] = [
      "empty",
      "mounted",
      "mounted-unhealthy",
      "mounted",
    ];
    sandbox.exec.mockImplementation(async (command: string) => executionResult(
      isMountProbe(command) ? probeStates.shift()! : "clone-visible",
    ));
    sandboxSdk.getSandbox.mockReturnValue(sandbox);
    const tools = cloudflareSandboxTools(fakeNamespace(), "clone-session");

    const clone = "git clone https://example.invalid/repo.git repo";
    const started = await tools.sandbox_start_process!.handler({ command: clone }, context);
    const result = await tools.sandbox_exec!.handler({ command: "git -C repo status" }, context);

    expect(started).toMatchObject({ command: clone });
    expect(result).toMatchObject({ success: true, stdout: "clone-visible" });
    expect(sandbox.startProcess).toHaveBeenCalledWith(clone, {
      cwd: "/workspace",
      autoCleanup: false,
    });
    expect(sandbox.exec.mock.calls.filter(([command]) => isMountProbe(command))).toHaveLength(4);
    expect(sandbox.mountBucket).toHaveBeenCalledTimes(1);
  });

  it("remounts retained storage after a container replacement before the next command", async () => {
    const sandbox = preparingSandbox("empty");
    const events: string[] = [];
    let retainedMarker = "";
    sandbox.mountBucket.mockImplementation(async () => {
      events.push("mount");
      sandbox.setMountState("mounted");
    });
    sandbox.exec.mockImplementation(async (command: string) => {
      if (isMountProbe(command)) {
        events.push("probe");
        return executionResult(sandbox.getMountState());
      }
      if (command === "printf retained-marker > marker.txt") {
        events.push("write-marker");
        retainedMarker = "retained-marker";
        return executionResult("");
      }
      if (command === "cat marker.txt") {
        events.push("read-marker");
        return executionResult(
          sandbox.getMountState() === "mounted" ? retainedMarker : "ephemeral-marker",
        );
      }
      if (isWorkspaceFlush(command)) {
        events.push("flush");
        return executionResult("");
      }
      throw new Error(`unexpected command: ${command}`);
    });
    sandboxSdk.getSandbox.mockReturnValue(sandbox);
    const tools = cloudflareSandboxTools(fakeNamespace(), "replacement-session");

    await tools.sandbox_exec!.handler({
      command: "printf retained-marker > marker.txt",
    }, context);
    sandbox.setMountState("empty");
    const second = await tools.sandbox_exec!.handler({ command: "cat marker.txt" }, context);

    expect(second).toMatchObject({ stdout: "retained-marker" });
    expect(events).toEqual([
      "probe",
      "mount",
      "probe",
      "write-marker",
      "flush",
      "probe",
      "mount",
      "probe",
      "read-marker",
      "flush",
    ]);
    expect(sandbox.mountBucket).toHaveBeenCalledTimes(2);
  });

  it("accepts a concurrent mount winner only after the retained mount probes healthy", async () => {
    const namespace = fakeNamespace();
    const sandbox = preparingSandbox("empty");
    const mountError = new Error("S3FS mount failed: MOUNTPOINT directory /workspace is not empty");
    sandbox.mountBucket.mockImplementationOnce(async () => {
      sandbox.setMountState("mounted");
      throw mountError;
    });
    sandboxSdk.getSandbox.mockReturnValue(sandbox);

    await expect(cloudflareSandboxTools(namespace, "raced-session")
      .sandbox_exec!.handler({ command: "printf reused" }, context)).resolves.toMatchObject({
        success: true,
      });

    expect(sandbox.mountBucket).toHaveBeenCalledTimes(1);
    expect(sandbox.exec.mock.calls.filter(([command]) => isMountProbe(command))).toHaveLength(2);
  });

  it("retries preparation after a real mount failure", async () => {
    const namespace = fakeNamespace();
    const sandbox = preparingSandbox("empty");
    const mountError = new Error("R2 mount unavailable");
    sandbox.mountBucket
      .mockRejectedValueOnce(mountError)
      .mockImplementationOnce(async () => sandbox.setMountState("mounted"));
    sandboxSdk.getSandbox.mockReturnValue(sandbox);
    const tools = cloudflareSandboxTools(namespace, "retry-session");

    await expect(tools.sandbox_exec!.handler({ command: "printf first" }, context))
      .rejects.toBe(mountError);
    await expect(tools.sandbox_exec!.handler({ command: "printf retry" }, context))
      .resolves.toMatchObject({ success: true });

    expect(sandboxSdk.getSandbox).toHaveBeenCalledTimes(2);
    expect(sandbox.mountBucket).toHaveBeenCalledTimes(2);
  });

  it.each([
    ["occupied", "unmounted /workspace directory is not empty"],
    ["mounted-unhealthy", "existing /workspace mount is unhealthy"],
  ] as const)("refuses to remount a %s workspace", async (state, message) => {
    const sandbox = preparingSandbox(state);
    sandboxSdk.getSandbox.mockReturnValue(sandbox);

    await expect(cloudflareSandboxTools(fakeNamespace(), `${state}-session`)
      .sandbox_exec!.handler({ command: "printf unsafe" }, context)).rejects.toThrow(message);

    expect(sandbox.mountBucket).not.toHaveBeenCalled();
    expect(sandbox.exec.mock.calls.filter(([command]) => isMountProbe(command))).toHaveLength(
      state === "mounted-unhealthy" ? 3 : 1,
    );
  });

  it("preserves the local R2 mount contract", async () => {
    const sandbox = preparingSandbox("empty");
    sandboxSdk.getSandbox.mockReturnValue(sandbox);

    await cloudflareSandboxTools(fakeNamespace(), "local-session", true)
      .sandbox_exec!.handler({ command: "printf local" }, context);

    expect(sandbox.mountBucket).toHaveBeenCalledWith(
      "NANOCODEX_WORKSPACES",
      "/workspace",
      { prefix: "/sessions/local-session/", localBucket: true },
    );
  });

  it("does not impose command or readiness timeout limits", async () => {
    const sandbox = fakeSandbox();
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await tools.sandbox_exec!.handler({
      command: "cargo test",
      timeout_ms: Number.MAX_SAFE_INTEGER,
    }, context);
    await tools.sandbox_start_process!.handler({
      command: "cargo watch",
      ready_port: 3_000,
    }, context);

    expect(sandbox.exec).toHaveBeenCalledWith("cargo test", {
      cwd: "/workspace",
      timeout: Number.MAX_SAFE_INTEGER,
    });
    const process = await sandbox.startProcess.mock.results[0]!.value;
    expect(process.waitForPort).toHaveBeenCalledWith(3_000, undefined);
  });

  it("returns the process identity when port readiness fails", async () => {
    const sandbox = fakeSandbox();
    const process = fakeProcess();
    process.waitForPort.mockRejectedValue(new Error("port never became ready"));
    process.getStatus.mockResolvedValue("running");
    sandbox.startProcess.mockResolvedValue(process);
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.sandbox_start_process!.handler({
      command: "start server",
      ready_port: 3_000,
    }, context)).resolves.toMatchObject({
      process_id: "process",
      status: "running",
      terminal: false,
      ready: false,
      ready_error: "port never became ready",
    });
    expect(process.kill).toHaveBeenCalledOnce();
    expect(process.getStatus).toHaveBeenCalledTimes(2);
    expect(sandbox.exec).toHaveBeenLastCalledWith("sync -f /workspace", { cwd: "/" });
  });

  it("runs the requested background command unchanged and flushes terminal observations", async () => {
    const sandbox = fakeSandbox();
    const tools = createCloudflareSandboxTools(async () => sandbox);
    const command = "cargo test --workspace";

    await expect(tools.sandbox_start_process!.handler({ command }, context))
      .resolves.toMatchObject({ command });
    expect(sandbox.startProcess.mock.calls[0]![0]).toBe(command);

    sandbox.getProcess.mockResolvedValue(fakeProcess({
      command,
      status: "completed",
      exitCode: 0,
    }));
    await expect(tools.sandbox_get_process!.handler({ process_id: "process" }, context))
      .resolves.toMatchObject({ command, status: "completed", terminal: true });
    expect(sandbox.exec).toHaveBeenLastCalledWith("sync -f /workspace", {
      cwd: "/",
    });
  });

  it("flushes a background process that finishes while it is starting", async () => {
    const sandbox = fakeSandbox();
    const process = fakeProcess({ status: "running" });
    process.getStatus.mockResolvedValue("completed");
    sandbox.startProcess.mockResolvedValue(process);
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.sandbox_start_process!.handler({ command: "quick task" }, context))
      .resolves.toMatchObject({ command: "quick task", status: "completed" });
    expect(sandbox.exec).toHaveBeenLastCalledWith("sync -f /workspace", {
      cwd: "/",
    });
  });

  it("retrieves authoritative background process state and bounded logs", async () => {
    const sandbox = fakeSandbox();
    const process = fakeProcess({
      id: "proc_123",
      pid: 42,
      command: "cargo test",
      status: "failed" as const,
      exitCode: 101,
    });
    process.getLogs.mockResolvedValue({
      stdout: "compiled\n",
      stderr: "test failed\n",
    });
    sandbox.getProcess.mockResolvedValue(process);
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.sandbox_get_process!.handler({ process_id: "proc_123" }, context))
      .resolves.toEqual({
        found: true,
        process_id: "proc_123",
        pid: 42,
        command: "cargo test",
        status: "failed",
        terminal: true,
        exit_code: 101,
        stdout: "compiled\n",
        stderr: "test failed\n",
        stdout_truncated: false,
        stderr_truncated: false,
      });
    expect(sandbox.getProcess).toHaveBeenCalledWith("proc_123");
    expect(process.getLogs).toHaveBeenCalledOnce();
  });

  it("polls running and missing processes without repeatedly transferring logs", async () => {
    const sandbox = fakeSandbox();
    const running = fakeProcess({
      id: "proc_running",
      command: "cargo test",
      status: "running" as const,
    });
    running.getLogs.mockResolvedValue({ stdout: "compiling", stderr: "" });
    sandbox.getProcess
      .mockResolvedValueOnce(running)
      .mockResolvedValueOnce(null);
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.sandbox_get_process!.handler({ process_id: "proc_running" }, context))
      .resolves.toEqual({
        found: true,
        process_id: "proc_running",
        pid: 1,
        command: "cargo test",
        status: "running",
        terminal: false,
        exit_code: null,
      });
    expect(running.getLogs).not.toHaveBeenCalled();
    await expect(tools.sandbox_get_process!.handler({ process_id: "proc_missing" }, context))
      .resolves.toEqual({ found: false, process_id: "proc_missing" });
    await expect(tools.sandbox_get_process!.handler({ process_id: "../other" }, context))
      .rejects.toThrow("process_id must be a safe Sandbox process ID");
  });

  it("returns partial running output only when explicitly requested", async () => {
    const sandbox = fakeSandbox();
    const running = fakeProcess({ id: "proc_running", status: "running" });
    running.getLogs.mockResolvedValue({ stdout: "compiling", stderr: "warning" });
    sandbox.getProcess.mockResolvedValue(running);
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.sandbox_get_process!.handler({
      process_id: "proc_running",
      include_output: true,
    }, context)).resolves.toMatchObject({
      terminal: false,
      stdout: "compiling",
      stderr: "warning",
    });
    expect(running.getLogs).toHaveBeenCalledOnce();
  });

  it("terminates a running background process and reports its refreshed status", async () => {
    const sandbox = fakeSandbox();
    const process = fakeProcess({ id: "proc_running", status: "running" });
    process.getStatus.mockResolvedValue("killed");
    sandbox.getProcess.mockResolvedValue(process);
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.sandbox_kill_process!.handler(
      { process_id: "proc_running" },
      context,
    )).resolves.toEqual({
      found: true,
      process_id: "proc_running",
      status: "killed",
      terminal: true,
      kill_requested: true,
    });
    expect(process.kill).toHaveBeenCalledOnce();
    expect(process.waitForExit).not.toHaveBeenCalled();
    expect(process.getStatus).toHaveBeenCalledOnce();
  });

  it("reports an asynchronously stopping process without waiting indefinitely", async () => {
    const sandbox = fakeSandbox();
    const process = fakeProcess({ id: "proc_running", status: "running" });
    process.getStatus.mockResolvedValue("running");
    sandbox.getProcess.mockResolvedValue(process);
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.sandbox_kill_process!.handler(
      { process_id: "proc_running" },
      context,
    )).resolves.toMatchObject({
      status: "running",
      terminal: false,
      kill_requested: true,
    });
    expect(process.waitForExit).not.toHaveBeenCalled();
  });

  it("does not kill missing or already-terminal background processes", async () => {
    const sandbox = fakeSandbox();
    const completed = fakeProcess({ id: "proc_completed", status: "completed" });
    sandbox.getProcess
      .mockResolvedValueOnce(completed)
      .mockResolvedValueOnce(null);
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.sandbox_kill_process!.handler(
      { process_id: "proc_completed" },
      context,
    )).resolves.toEqual({
      found: true,
      process_id: "proc_completed",
      status: "completed",
      terminal: true,
      kill_requested: false,
    });
    await expect(tools.sandbox_kill_process!.handler(
      { process_id: "proc_missing" },
      context,
    )).resolves.toEqual({ found: false, process_id: "proc_missing" });
    expect(completed.kill).not.toHaveBeenCalled();
    expect(completed.getStatus).not.toHaveBeenCalled();
  });

  it("truncates background process stdout and stderr on UTF-8 boundaries", async () => {
    const sandbox = fakeSandbox();
    const process = fakeProcess({
      id: "proc_flood",
      command: "flood",
      status: "completed" as const,
      exitCode: 0,
    });
    process.getLogs.mockResolvedValue({
      stdout: "🦀".repeat(40_000),
      stderr: "λ".repeat(80_000),
    });
    sandbox.getProcess.mockResolvedValue(process);
    const tools = createCloudflareSandboxTools(async () => sandbox);

    const result = (await tools.sandbox_get_process!.handler(
      { process_id: "proc_flood" },
      context,
    )) as Record<string, unknown>;
    expect(result.stdout_truncated).toBe(true);
    expect(result.stderr_truncated).toBe(true);
    expect(new TextEncoder().encode(String(result.stdout)).byteLength).toBe(128 * 1024);
    expect(new TextEncoder().encode(String(result.stderr)).byteLength).toBe(128 * 1024);
  });

  it("keeps account credentials and origin authority out of sandbox previews", async () => {
    const containerFetch = vi.fn(async (_request: Request, _port: number) => new Response("preview", {
      status: 201,
      headers: {
        "clear-site-data": '"cookies"',
        "content-security-policy-report-only": "default-src 'none'",
        host: "sandbox.internal",
        "set-cookie": "nanocodex=overwritten",
      },
    }));
    sandboxSdk.getSandbox.mockReturnValue({
      containerFetch,
      wsConnect: vi.fn(),
    });
    const request = new Request("https://nanocodex.example/sandbox-preview/capability/app?q=kept", {
      method: "POST",
      headers: {
        authorization: "Bearer account-secret",
        cookie: "nanocodex=session-secret",
        "cf-access-jwt-assertion": "access-secret",
        "cf-ray": "private-ray",
        forwarded: "host=nanocodex.example",
        "forwarded-host": "nanocodex.example",
        host: "nanocodex.example",
        origin: "https://nanocodex.example",
        referer: "https://nanocodex.example/account",
        "x-preview-token": "private-token",
        "x-forwarded-host": "nanocodex.example",
        "x-nanocodex-owner-id": "account-id",
        "x-preview-header": "kept",
      },
      body: "payload",
    });

    const response = await proxyCloudflareSandboxPreview(
      fakeNamespace(),
      "preview-session",
      8_080,
      request,
      "/app",
    );

    const forwarded = containerFetch.mock.calls[0]![0];
    expect(forwarded.url).toBe("http://sandbox.internal/app?q=kept");
    expect(forwarded.headers.get("authorization")).toBeNull();
    expect(forwarded.headers.get("cookie")).toBeNull();
    expect(forwarded.headers.get("cf-access-jwt-assertion")).toBeNull();
    expect(forwarded.headers.get("cf-ray")).toBeNull();
    expect(forwarded.headers.get("forwarded")).toBeNull();
    expect(forwarded.headers.get("forwarded-host")).toBeNull();
    expect(forwarded.headers.get("host")).toBeNull();
    expect(forwarded.headers.get("origin")).toBeNull();
    expect(forwarded.headers.get("referer")).toBeNull();
    expect(forwarded.headers.get("x-preview-token")).toBeNull();
    expect(forwarded.headers.get("x-forwarded-host")).toBeNull();
    expect(forwarded.headers.get("x-nanocodex-owner-id")).toBeNull();
    expect(forwarded.headers.get("x-preview-header")).toBe("kept");
    expect(await forwarded.text()).toBe("payload");
    expect(response.status).toBe(201);
    expect(await response.text()).toBe("preview");
    expect(response.headers.get("set-cookie")).toBeNull();
    expect(response.headers.get("clear-site-data")).toBeNull();
    expect(response.headers.get("content-security-policy-report-only")).toBeNull();
    expect(response.headers.get("host")).toBeNull();
    const policy = response.headers.get("content-security-policy")!;
    expect(policy).toContain("sandbox");
    expect(policy).toContain("allow-scripts");
    expect(policy).not.toContain("allow-same-origin");
    expect(response.headers.get("referrer-policy")).toBe("no-referrer");
  });

  it("keeps root-relative redirects inside the preview capability", async () => {
    const containerFetch = vi.fn(async (request: Request) => new Response(null, {
      status: 302,
      headers: {
        location: request.url.endsWith("/scoped")
          ? "/sandbox-preview/capability/login"
          : "/login?next=%2Fapp#form",
      },
    }));
    sandboxSdk.getSandbox.mockReturnValue({
      containerFetch,
      wsConnect: vi.fn(),
    });

    const response = await proxyCloudflareSandboxPreview(
      fakeNamespace(),
      "preview-session",
      8_080,
      new Request("https://nanocodex.example/sandbox-preview/capability/private"),
      "/private",
    );

    expect(response.status).toBe(302);
    expect(response.headers.get("location")).toBe(
      "/sandbox-preview/capability/login?next=%2Fapp#form",
    );
    const scopedResponse = await proxyCloudflareSandboxPreview(
      fakeNamespace(),
      "preview-session",
      8_080,
      new Request("https://nanocodex.example/sandbox-preview/capability/scoped"),
      "/scoped",
    );
    expect(scopedResponse.headers.get("location")).toBe("/sandbox-preview/capability/login");
  });

  it("keeps root-relative HTML asset requests inside the preview capability", async () => {
    const containerFetch = vi.fn(async (request: Request) => request.url.endsWith("/app.css")
      ? new Response("css")
      : new Response([
        "<!doctype html><html><head>",
        '<link rel="stylesheet" href="/app.css">',
        '<script type="module" src="/@vite/client"></script>',
        '<img srcset="/small.png 1x, /large.png 2x">',
        '<img src="/sandbox-preview/capability/already-scoped.png">',
        '<script src="//cdn.example/library.js"></script>',
        "</head></html>",
      ].join(""), { headers: { "content-type": "text/html; charset=utf-8" } }));
    sandboxSdk.getSandbox.mockReturnValue({
      containerFetch,
      wsConnect: vi.fn(),
    });
    const namespace = fakeNamespace();
    const previewUrl = "https://nanocodex.example/sandbox-preview/capability/";

    const documentResponse = await proxyCloudflareSandboxPreview(
      namespace,
      "preview-session",
      8_080,
      new Request(previewUrl),
      "/",
    );
    const html = await documentResponse.text();
    expect(html).toContain('href="/sandbox-preview/capability/app.css"');
    expect(html).toContain('src="/sandbox-preview/capability/@vite/client"');
    expect(html).toContain(
      'srcset="/sandbox-preview/capability/small.png 1x, /sandbox-preview/capability/large.png 2x"',
    );
    expect(html).toContain('src="/sandbox-preview/capability/already-scoped.png"');
    expect(html).toContain('src="//cdn.example/library.js"');
    expect(html).toContain('{"imports":{"/":"/sandbox-preview/capability/"}}');
    expect(html).toContain("globalThis.WebSocket=new Proxy");
    expect(html).toContain('const p="/sandbox-preview/capability"');

    const assetUrl = new URL("/sandbox-preview/capability/app.css", previewUrl);
    await proxyCloudflareSandboxPreview(
      namespace,
      "preview-session",
      8_080,
      new Request(assetUrl),
      "/app.css",
    );
    expect(containerFetch.mock.calls[1]![0].url).toBe("http://sandbox.internal/app.css");
  });

  it("retains the preview capability for Vite HMR WebSockets", async () => {
    const sockets = new WebSocketPair();
    const wsConnect = vi.fn(async (_request: Request, _port: number) => ({
      status: 101,
      headers: new Headers({
        connection: "Upgrade",
        host: "sandbox.internal",
        "sec-websocket-accept": "accepted",
        "sec-websocket-protocol": "preview-v1",
        "set-cookie": "nanocodex=overwritten",
        upgrade: "websocket",
        "x-preview-private": "not-forwarded",
      }),
      webSocket: sockets[0],
    }) as unknown as Response);
    sandboxSdk.getSandbox.mockReturnValue({
      containerFetch: vi.fn(),
      wsConnect,
    });
    const request = new Request("https://nanocodex.example/sandbox-preview/capability/?token=hmr", {
      headers: {
        connection: "Upgrade",
        cookie: "nanocodex=session-secret",
        host: "nanocodex.example",
        origin: "https://nanocodex.example",
        "sec-websocket-key": "socket-key",
        "sec-websocket-protocol": "preview-v1",
        "sec-websocket-version": "13",
        upgrade: "websocket",
        "x-forwarded-host": "nanocodex.example",
      },
    });

    const response = await proxyCloudflareSandboxPreview(
      fakeNamespace(),
      "preview-session",
      8_080,
      request,
      "/",
    );

    const forwarded = wsConnect.mock.calls[0]![0];
    expect(forwarded.url).toBe("http://sandbox.internal/?token=hmr");
    expect(forwarded.headers.get("connection")).toBe("Upgrade");
    expect(forwarded.headers.get("upgrade")).toBe("websocket");
    expect(forwarded.headers.get("sec-websocket-key")).toBe("socket-key");
    expect(forwarded.headers.get("sec-websocket-protocol")).toBe("preview-v1");
    expect(forwarded.headers.get("sec-websocket-version")).toBe("13");
    expect(forwarded.headers.get("cookie")).toBeNull();
    expect(forwarded.headers.get("host")).toBeNull();
    expect(forwarded.headers.get("origin")).toBeNull();
    expect(forwarded.headers.get("x-forwarded-host")).toBeNull();
    expect(response.status).toBe(101);
    expect(response.webSocket).toBe(sockets[0]);
    expect(response.headers.get("connection")).toBe("Upgrade");
    expect(response.headers.get("upgrade")).toBe("websocket");
    expect(response.headers.get("sec-websocket-accept")).toBe("accepted");
    expect(response.headers.get("sec-websocket-protocol")).toBe("preview-v1");
    expect(response.headers.get("host")).toBeNull();
    expect(response.headers.get("set-cookie")).toBeNull();
    expect(response.headers.get("x-preview-private")).toBeNull();
  });

  it("does not manufacture a successful WebSocket upgrade from an upstream failure", async () => {
    const wsConnect = vi.fn(async () => new Response("not ready", {
      status: 503,
      headers: { "set-cookie": "nanocodex=overwritten" },
    }));
    sandboxSdk.getSandbox.mockReturnValue({
      containerFetch: vi.fn(),
      wsConnect,
    });
    const response = await proxyCloudflareSandboxPreview(
      fakeNamespace(),
      "preview-session",
      8_080,
      new Request("https://nanocodex.example/sandbox-preview/capability/socket", {
        headers: { upgrade: "websocket" },
      }),
      "/socket",
    );

    expect(response.status).toBe(503);
    expect(await response.text()).toBe("not ready");
    expect(response.webSocket).toBeNull();
    expect(response.headers.get("set-cookie")).toBeNull();
  });

  it("rejects expired and malformed authenticated preview payloads", async () => {
    const secret = "preview-expiry-test-secret";
    const sessionId = "018f25e8-7b51-7a32-8c4d-abcdef012345";
    const expired = await sealPreviewPayload(
      secret,
      `${sessionId}\n8080\n${Date.now() - 1}`,
    );
    const malformed = await sealPreviewPayload(
      secret,
      `${sessionId}\n8080\nnot-an-expiry`,
    );

    await expect(openSandboxPreviewCapability(secret, expired))
      .rejects.toThrow("invalid preview capability");
    await expect(openSandboxPreviewCapability(secret, malformed))
      .rejects.toThrow("invalid preview capability");
  });

  it("caches one derived preview key and replaces it when the secret rotates", async () => {
    const digest = vi.spyOn(crypto.subtle, "digest");
    const sessionId = "018f25e8-7b51-7a32-8c4d-fedcba987654";
    const firstSecret = "preview-key-cache-first";
    const secondSecret = "preview-key-cache-second";
    const firstUrl = await cloudflareSandboxPreviewUrl(
      "https://nanocodex.example",
      firstSecret,
      sessionId,
      8_080,
    );
    const firstCapability = new URL(firstUrl).pathname.split("/")[2]!;

    await openSandboxPreviewCapability(firstSecret, firstCapability);
    await cloudflareSandboxPreviewUrl(
      "https://nanocodex.example",
      secondSecret,
      sessionId,
      8_081,
    );
    await openSandboxPreviewCapability(firstSecret, firstCapability);

    expect(digest).toHaveBeenCalledTimes(3);
  });

  it("deletes every persisted object owned by a removed sandbox", async () => {
    const pages = [
      { objects: [{ key: "/sessions/session/a" }, { key: "/sessions/session/b" }] },
      { objects: [{ key: "/sessions/session/c" }] },
      { objects: [] },
    ];
    const bucket = {
      list: vi.fn(async () => pages.shift()!),
      delete: vi.fn(async () => {}),
    } as unknown as R2Bucket;

    await deleteCloudflareSandboxWorkspace(bucket, "session");

    expect(bucket.list).toHaveBeenCalledTimes(3);
    expect(bucket.list).toHaveBeenCalledWith({ prefix: "/sessions/session/", limit: 1_000 });
    expect(bucket.delete).toHaveBeenCalledWith([
      "/sessions/session/a",
      "/sessions/session/b",
    ]);
    expect(bucket.delete).toHaveBeenCalledWith(["/sessions/session/c"]);
  });
});

type FakeLookupProcess = {
  id: string;
  pid?: number;
  command: string;
  status: "starting" | "running" | "completed" | "failed" | "killed" | "error";
  exitCode?: number;
  kill(): Promise<void>;
  getStatus(): Promise<"starting" | "running" | "completed" | "failed" | "killed" | "error">;
  getLogs(): Promise<{ stdout: string; stderr: string }>;
  waitForPort(port: number, options?: { timeout?: number }): Promise<void>;
  waitForExit(timeout?: number): Promise<{ exitCode: number }>;
};

function fakeSandbox() {
  const process = fakeProcess();
  return {
    exec: vi.fn(async (_command: string, _options?: { cwd: string; timeout?: number }) => ({
      success: true,
      exitCode: 0,
      stdout: "",
      stderr: "",
      duration: 1,
    })),
    startProcess: vi.fn(async (command: string) => ({ ...process, command })),
    getProcess: vi.fn(async (id: string): Promise<FakeLookupProcess | null> => (
      id === process.id ? process : null
    )),
    readFile: vi.fn(async () => ({
      size: 0,
      content: new ReadableStream<Uint8Array>({ start(controller) { controller.close(); } }),
    })),
    writeFile: vi.fn(async () => {}),
    listFiles: vi.fn(async () => ({ files: [] })),
    tunnels: { get: vi.fn(async () => ({ url: "https://preview.example" })) },
  };
}

function fakeProcess(overrides: Partial<Pick<
  FakeLookupProcess,
  "id" | "pid" | "command" | "status" | "exitCode"
>> = {}) {
  return {
    id: "process",
    pid: 1,
    command: "",
    status: "running" as const,
    kill: vi.fn(async () => {}),
    getStatus: vi.fn(async (): Promise<FakeLookupProcess["status"]> => "running"),
    getLogs: vi.fn(async () => ({ stdout: "", stderr: "" })),
    waitForPort: vi.fn(async () => {}),
    waitForExit: vi.fn(async () => ({ exitCode: 0 })),
    ...overrides,
  };
}

function preparingSandbox(initialState: MountState) {
  let mountState = initialState;
  const sandbox = {
    ...fakeSandbox(),
    mountBucket: vi.fn(async () => { mountState = "mounted"; }),
    destroy: vi.fn(async () => {}),
    setMountState(state: MountState) { mountState = state; },
    getMountState() { return mountState; },
  };
  sandbox.exec.mockImplementation(async (command: string) => executionResult(
    isMountProbe(command) ? mountState : "",
  ));
  return sandbox;
}

type MountState = "absent" | "empty" | "occupied" | "mounted" | "mounted-unhealthy";

function isMountProbe(command: unknown): boolean {
  return typeof command === "string" && command.startsWith("if mountpoint -q /workspace");
}

function isWorkspaceFlush(command: unknown): boolean {
  return command === "sync -f /workspace";
}

function executionResult(stdout: string) {
  return {
    success: true,
    exitCode: 0,
    stdout,
    stderr: "",
    duration: 1,
  };
}

function fakeNamespace(): DurableObjectNamespace<Sandbox> {
  return {} as DurableObjectNamespace<Sandbox>;
}

async function sealPreviewPayload(secret: string, payload: string): Promise<string> {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(secret));
  const key = await crypto.subtle.importKey(
    "raw",
    digest,
    { name: "AES-GCM" },
    false,
    ["encrypt"],
  );
  const ciphertext = new Uint8Array(await crypto.subtle.encrypt(
    {
      name: "AES-GCM",
      iv,
      additionalData: new TextEncoder().encode("nanocodex-cloudflare-sandbox-preview-v1"),
    },
    key,
    new TextEncoder().encode(payload),
  ));
  const sealed = new Uint8Array(iv.byteLength + ciphertext.byteLength);
  sealed.set(iv);
  sealed.set(ciphertext, iv.byteLength);
  return btoa(String.fromCharCode(...sealed))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/, "");
}
