import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const sandboxSdk = vi.hoisted(() => ({ getSandbox: vi.fn() }));
vi.mock("@cloudflare/sandbox", () => ({ getSandbox: sandboxSdk.getSandbox }));

import {
  cloudflareSandboxPreviewUrl,
  cloudflareSandboxTools,
  createCloudflareSandboxTools,
  deleteCloudflareBrainWorkspace,
  deleteCloudflareSandboxWorkspace,
  openSandboxPreviewCapability,
  prepareCloudflareSandbox,
  prepareCloudflareSandboxHand,
  proxyCloudflareSandboxPreview,
} from "../src/sandbox-tools";
import type { Sandbox } from "../src/sandbox-runtime";

const SHARED_BRAIN = { resourceId: "durable-agent" } as const;

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

  it("exposes only the canonical shell and preview tools without a host selector", () => {
    const tools = createCloudflareSandboxTools(async () => fakeSandbox());

    expect(Object.keys(tools)).toEqual(["exec_command", "write_stdin", "preview"]);
    expect(tools.exec_command!.parameters).toMatchObject({
      required: ["cmd"],
    });
    const parameters = tools.exec_command!.parameters as {
      properties: Record<string, Record<string, unknown>>;
    };
    expect(parameters.properties.yield_time_ms!.maximum).toBeUndefined();
    expect(parameters.properties.max_output_tokens!.maximum).toBeUndefined();
    expect(parameters.properties.environment).toBeUndefined();
    expect(parameters.properties.host).toBeUndefined();
    expect(tools.exec_command!.outputSchema).toMatchObject({
      required: ["wall_time_seconds", "output"],
    });
    expect(tools.write_stdin!.parameters).toMatchObject({ required: ["session_id"] });
    expect(tools.preview!.parameters).toMatchObject({ required: ["port"] });
  });

  it("does not impose command, wait, or output ceilings below the platform", async () => {
    const sandbox = fakeSandbox();
    const process = fakeProcess({ status: "completed", exitCode: 0 });
    const output = "x".repeat(160 * 1024);
    const command = "x".repeat(33 * 1024);
    process.getLogs.mockResolvedValue({ stdout: output, stderr: "" });
    sandbox.startProcess.mockResolvedValue(process);
    sandbox.getProcess.mockImplementation(async (id: string) => id === process.id ? process : null);
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.exec_command!.handler({
      cmd: command,
      yield_time_ms: 120_001,
      max_output_tokens: 100_001,
    }, context)).resolves.toMatchObject({
      output,
      exit_code: 0,
    });
  });

  it("runs canonical exec_command in the retained workspace and flushes mutations", async () => {
    const sandbox = fakeSandbox();
    const process = fakeProcess({ status: "failed", exitCode: 7 });
    process.getLogs.mockResolvedValue({ stdout: "hello", stderr: "warning" });
    sandbox.startProcess.mockResolvedValue(process);
    sandbox.getProcess.mockImplementation(async (id: string) => (
      id === process.id ? process : null
    ));
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.exec_command!.handler({
      cmd: "task",
      workdir: "repo",
    }, context)).resolves.toEqual({
      output: "hellowarning",
      chunk_id: expect.stringMatching(/^[1-9][0-9]*:12$/),
      exit_code: 7,
      wall_time_seconds: expect.any(Number),
    });
    expect(sandbox.startProcess).toHaveBeenCalledWith("exec 2>&1\ntask", {
      cwd: "/workspace/repo",
      envVars: { GH_TOKEN: "NANOCODEX_PROVIDER_CREDENTIAL", GH_PROMPT_DISABLED: "1", GIT_TERMINAL_PROMPT: "0" },
      processId: expect.stringMatching(/^nanocodex-[1-9][0-9]*$/),
      autoCleanup: false,
    });
    expect(sandbox.exec).toHaveBeenCalledWith("sync", { cwd: "/" });
  });

  it("rejects an unavailable workdir before starting a retained process", async () => {
    const sandbox = fakeSandbox();
    sandbox.exec.mockResolvedValueOnce(executionResult(
      "",
      "Failed to change directory to '/workspace/workspace/dtolnay/itoa'",
      1,
    ));
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.exec_command!.handler({
      cmd: "cargo test",
      workdir: "workspace/dtolnay/itoa",
    }, context)).rejects.toThrow(
      "exec_command.workdir is unavailable: /workspace/workspace/dtolnay/itoa: Failed to change directory",
    );
    expect(sandbox.startProcess).not.toHaveBeenCalled();
  });

  it("persists the output cursor across tool reconstruction and preserves a terminal tail", async () => {
    const sandbox = fakeSandbox();
    const process = fakeProcess({ status: "running" });
    let status: "running" | "completed" = "running";
    let stdout = "first";
    process.getStatus.mockImplementation(async () => status);
    process.getLogs.mockImplementation(async () => ({ stdout, stderr: "" }));
    sandbox.startProcess.mockImplementation(async (_command: string, options: { processId: string }) => {
      process.id = options.processId;
      return process;
    });
    sandbox.getProcess.mockImplementation(async (id: string) => id === process.id ? process : null);
    const cursors = new Map<string, unknown>();
    const cursorStorage = {
      delete: (key: string) => { cursors.delete(key); },
      get: (key: string) => cursors.get(key),
      put: (key: string, value: unknown) => { cursors.set(key, value); },
    };
    let tools = createCloudflareSandboxTools(async () => sandbox, undefined, cursorStorage);

    const started = await tools.exec_command!.handler({
      cmd: "task",
      yield_time_ms: 0,
    }, context) as { session_id: number; output: string };
    expect(started.output).toBe("first");
    stdout = "firstsecond";
    tools = createCloudflareSandboxTools(async () => sandbox, undefined, cursorStorage);
    await expect(tools.write_stdin!.handler({
      session_id: started.session_id,
      yield_time_ms: 0,
    }, context)).resolves.toMatchObject({
      output: "second",
      session_id: started.session_id,
    });

    stdout += `${"x".repeat(100)}TAIL`;
    status = "completed";
    tools = createCloudflareSandboxTools(async () => sandbox, undefined, cursorStorage);
    await expect(tools.write_stdin!.handler({
      session_id: started.session_id,
      max_output_tokens: 9,
    }, context)).resolves.toMatchObject({
      output: expect.stringMatching(/^x+\n… output truncated …\n.*TAIL$/),
      original_token_count: 26,
      exit_code: 0,
    });
    expect(cursors.size).toBe(0);
  });

  it("uses Ctrl-C as the canonical termination path for a yielded session", async () => {
    const sandbox = fakeSandbox();
    const process = fakeProcess({ id: "nanocodex-7", status: "killed", exitCode: 137 });
    sandbox.getProcess.mockResolvedValue(process);
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.write_stdin!.handler({
      session_id: 7,
      chars: "\u0003",
    }, context)).resolves.toMatchObject({ exit_code: 137 });
    expect(process.kill).toHaveBeenCalledTimes(1);
  });

  it("uses the sandbox exit stream once for an unrestricted yield", async () => {
    const sandbox = fakeSandbox();
    const process = fakeProcess({ status: "running" });
    const timeout = Object.assign(new Error("still running"), {
      name: "ProcessReadyTimeoutError",
    });
    process.waitForExit.mockRejectedValue(timeout);
    sandbox.startProcess.mockResolvedValue(process);
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.exec_command!.handler({
      cmd: "long-job",
      yield_time_ms: 120_001,
    }, context)).resolves.toMatchObject({ session_id: expect.any(Number) });
    expect(process.waitForExit).toHaveBeenCalledOnce();
    expect(process.waitForExit).toHaveBeenCalledWith(120_001);
    expect(process.getStatus).toHaveBeenCalledTimes(2);
  });

  it("rejects escalation, PTYs, stdin, shell overrides, and escaped workdirs", async () => {
    const tools = createCloudflareSandboxTools(async () => fakeSandbox());

    await expect(tools.exec_command!.handler({
      cmd: "pwd", sandbox_permissions: "require_escalated",
    }, context)).rejects.toThrow("does not support privilege escalation");
    await expect(tools.exec_command!.handler({ cmd: "pwd", tty: true }, context))
      .rejects.toThrow("does not support TTY");
    await expect(tools.exec_command!.handler({
      cmd: "pwd", shell: "/bin/zsh",
    }, context)).rejects.toThrow("does not support shell or login overrides");
    await expect(tools.exec_command!.handler({
      cmd: "pwd", workdir: "../outside",
    }, context)).rejects.toThrow("must not contain '..'");
    await expect(tools.write_stdin!.handler({
      session_id: 7, chars: "input",
    }, context)).rejects.toThrow("stdin is unavailable");
  });

  it("attempts a retained-workspace flush when command transport fails", async () => {
    const sandbox = fakeSandbox();
    const failure = new Error("transport stopped");
    sandbox.startProcess.mockRejectedValueOnce(failure);
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.exec_command!.handler({ cmd: "task" }, context))
      .rejects.toBe(failure);
    expect(sandbox.exec).toHaveBeenLastCalledWith("sync", { cwd: "/" });
  });

  it("kills and flushes a started process when observation fails", async () => {
    const sandbox = fakeSandbox();
    const process = fakeProcess({ status: "running" });
    const failure = new Error("status transport stopped");
    process.getStatus.mockRejectedValueOnce(failure);
    sandbox.startProcess.mockResolvedValue(process);
    const tools = createCloudflareSandboxTools(async () => sandbox);

    await expect(tools.exec_command!.handler({ cmd: "task" }, context))
      .rejects.toBe(failure);
    expect(process.kill).toHaveBeenCalledTimes(1);
    expect(sandbox.exec).toHaveBeenLastCalledWith("sync", { cwd: "/" });
  });

  it("mounts one retained workspace and reuses it across tool reconstruction", async () => {
    const sandbox = preparingSandbox("empty");
    sandboxSdk.getSandbox.mockReturnValue(sandbox);
    const namespace = fakeNamespace();

    await cloudflareSandboxTools(namespace, "retained").exec_command!.handler(
      { cmd: "printf first" }, context,
    );
    await cloudflareSandboxTools(namespace, "retained").exec_command!.handler(
      { cmd: "printf second" }, context,
    );

    expect(sandboxSdk.getSandbox).toHaveBeenCalledTimes(2);
    expect(sandbox.mountBucket).toHaveBeenCalledTimes(1);
    expect(sandbox.mountBucket).toHaveBeenCalledWith(
      "NANOCODEX_WORKSPACES", "/workspace", {
        prefix: "/sessions/retained/",
        s3fsOptions: ["stat_cache_expire=0"],
      },
    );
  });

  it("provisions and mounts a retained workspace without running a command", async () => {
    const sandbox = preparingSandbox("empty");
    sandboxSdk.getSandbox.mockReturnValue(sandbox);

    await prepareCloudflareSandbox(fakeNamespace(), "mounted-hand");

    expect(sandbox.mountBucket).toHaveBeenCalledWith(
      "NANOCODEX_WORKSPACES", "/workspace", {
        prefix: "/sessions/mounted-hand/",
        s3fsOptions: ["stat_cache_expire=0"],
      },
    );
    expect(sandbox.startProcess).not.toHaveBeenCalled();
  });

  it("mounts every Cloudflare workspace into every peer native namespace", async () => {
    const one = namespaceSandbox();
    const two = namespaceSandbox();
    sandboxSdk.getSandbox.mockReturnValueOnce(one).mockReturnValueOnce(two);
    const mounts = [
      { resourceId: "resource-one", root: "/mnt-one-11111111", slot: 7 },
      { resourceId: "resource-two", root: "/mnt-two-22222222", slot: 3 },
    ];

    const namespace = fakeNamespace();
    await prepareCloudflareSandboxHand(
      namespace, "resource-one", mounts, false, SHARED_BRAIN,
    );
    await prepareCloudflareSandboxHand(
      namespace, "resource-two", mounts, false, SHARED_BRAIN,
    );

    expect(one.mountBucket).toHaveBeenCalledWith(
      "NANOCODEX_WORKSPACES_3",
      "/mnt-two-22222222",
      {
        prefix: "/sessions/resource-two/",
        readOnly: true,
        s3fsOptions: ["stat_cache_expire=0"],
      },
    );
    expect(two.mountBucket).toHaveBeenCalledWith(
      "NANOCODEX_WORKSPACES_7",
      "/mnt-one-11111111",
      {
        prefix: "/sessions/resource-one/",
        readOnly: true,
        s3fsOptions: ["stat_cache_expire=0"],
      },
    );
    for (const sandbox of [one, two]) {
      expect(sandbox.mountBucket).toHaveBeenCalledWith(
        "NANOCODEX_BRAIN",
        "/brain",
        {
          prefix: "/brains/durable-agent/",
          s3fsOptions: ["stat_cache_expire=0"],
        },
      );
    }
    expect(one.exec).toHaveBeenCalledWith(
      expect.stringContaining("ln -s /workspace '/mnt-one-11111111'"),
      { cwd: "/", timeout: 10_000 },
    );
    expect(two.exec).toHaveBeenCalledWith(
      expect.stringContaining("ln -s /workspace '/mnt-two-22222222'"),
      { cwd: "/", timeout: 10_000 },
    );
  });

  it("rejects ambiguous Cloudflare peer binding slots before provisioning", async () => {
    await expect(prepareCloudflareSandboxHand(fakeNamespace(), "resource-one", [
      { resourceId: "resource-one", root: "/mnt-one-11111111", slot: 0 },
      { resourceId: "resource-two", root: "/mnt-two-22222222", slot: 0 },
    ], false, SHARED_BRAIN)).rejects.toThrow("unique resources, roots, and slots");
    expect(sandboxSdk.getSandbox).not.toHaveBeenCalled();
  });

  it("accepts local synchronized peer directories without requiring FUSE mountpoints", async () => {
    const sandbox = namespaceSandbox();
    sandboxSdk.getSandbox.mockReturnValue(sandbox);

    await prepareCloudflareSandboxHand(fakeNamespace(), "resource-one", [
      { resourceId: "resource-one", root: "/mnt-one-11111111", slot: 0 },
      { resourceId: "resource-two", root: "/mnt-two-22222222", slot: 1 },
    ], true, SHARED_BRAIN);

    expect(sandbox.mountBucket).toHaveBeenCalledWith(
      "NANOCODEX_WORKSPACES_1",
      "/run/nanocodex/peer-sources/1",
      { prefix: "/sessions/resource-two/", localBucket: true, readOnly: true },
    );
    expect(sandbox.exec).toHaveBeenCalledWith(
      expect.stringContaining(
        "mount --bind '/run/nanocodex/peer-sources/1' '/mnt-two-22222222'",
      ),
      { cwd: "/", timeout: 10_000 },
    );
    expect(sandbox.mountBucket).toHaveBeenCalledWith(
      "NANOCODEX_BRAIN",
      "/brain",
      { prefix: "/brains/durable-agent/", localBucket: true },
    );
  });

  it("deduplicates local SDK mounts across hand reconstruction", async () => {
    const sandbox = namespaceSandbox();
    sandboxSdk.getSandbox.mockReturnValue(sandbox);
    const namespace = fakeNamespace();
    const mounts = [
      { resourceId: "resource-one", root: "/mnt-one-11111111", slot: 0 },
      { resourceId: "resource-two", root: "/mnt-two-22222222", slot: 1 },
    ];

    await prepareCloudflareSandboxHand(
      namespace, "resource-one", mounts, true, SHARED_BRAIN,
    );
    await prepareCloudflareSandboxHand(
      namespace, "resource-one", mounts, true, SHARED_BRAIN,
    );

    expect(sandboxSdk.getSandbox).toHaveBeenCalledTimes(2);
    expect(sandbox.mountBucket).toHaveBeenCalledTimes(3);
    expect(sandbox.mountBucket).toHaveBeenCalledWith(
      "NANOCODEX_WORKSPACES", "/workspace",
      { prefix: "/sessions/resource-one/", localBucket: true },
    );
    expect(sandbox.mountBucket).toHaveBeenCalledWith(
      "NANOCODEX_BRAIN", "/brain",
      { prefix: "/brains/durable-agent/", localBucket: true },
    );
    expect(sandbox.mountBucket).toHaveBeenCalledWith(
      "NANOCODEX_WORKSPACES_1", "/run/nanocodex/peer-sources/1",
      { prefix: "/sessions/resource-two/", localBucket: true, readOnly: true },
    );
  });

  it("requires a valid shared brain workspace before provisioning hands", async () => {
    const mount = [{
      resourceId: "resource-one",
      root: "/mnt-one-11111111",
      slot: 0,
    }];
    await expect(prepareCloudflareSandboxHand(
      fakeNamespace(), "resource-one", mount,
    )).rejects.toThrow("requires a shared brain workspace");
    await expect(prepareCloudflareSandboxHand(
      fakeNamespace(), "resource-one", mount, false, { resourceId: "../agent" },
    )).rejects.toThrow("invalid resource id");
    expect(sandboxSdk.getSandbox).not.toHaveBeenCalled();
  });

  it("preserves local R2 mount options and refuses unsafe remote remounts", async () => {
    const local = preparingSandbox("empty");
    sandboxSdk.getSandbox.mockReturnValueOnce(local);
    await cloudflareSandboxTools(fakeNamespace(), "local", true).exec_command!.handler(
      { cmd: "printf local" }, context,
    );
    expect(local.mountBucket).toHaveBeenCalledWith(
      "NANOCODEX_WORKSPACES",
      "/workspace",
      { prefix: "/sessions/local/", localBucket: true },
    );

    const occupied = preparingSandbox("occupied");
    sandboxSdk.getSandbox.mockReturnValueOnce(occupied);
    await expect(cloudflareSandboxTools(fakeNamespace(), "occupied").exec_command!.handler(
      { cmd: "printf unsafe" }, context,
    )).rejects.toThrow("unmounted /workspace directory is not empty");
    expect(occupied.mountBucket).not.toHaveBeenCalled();
  });

  it("creates a server-fronted preview without exposing its secret", async () => {
    const tools = createCloudflareSandboxTools(
      async () => fakeSandbox(),
      async (port) => ({
        port,
        url: "https://nanocodex.example/sandbox-preview/sealed/",
        persistent: false,
      }),
    );

    await expect(tools.preview!.handler({ port: 8080 }, context))
      .resolves.toEqual({
        port: 8080,
        url: "https://nanocodex.example/sandbox-preview/sealed/",
        persistent: false,
      });
  });

  it("never exposes the sandbox control-plane port", async () => {
    const tools = createCloudflareSandboxTools(async () => fakeSandbox());

    await expect(tools.preview!.handler({ port: 3_000 }, context))
      .rejects.toThrow("reserved for the sandbox control plane");
    await expect(cloudflareSandboxPreviewUrl(
      "https://nanocodex.example", "secret", "preview-session", 3_000,
    )).rejects.toThrow("reserved for the sandbox control plane");
    await expect(proxyCloudflareSandboxPreview(
      fakeNamespace(), "preview-session", 3_000,
      new Request("https://nanocodex.example/sandbox-preview/capability/"),
      "/api/execute",
    )).rejects.toThrow("reserved for the sandbox control plane");
  });

  it("keeps account credentials and origin authority out of sandbox previews", async () => {
    const containerFetch = vi.fn(async (_request: Request) => new Response("preview", {
      status: 201,
      headers: {
        "clear-site-data": '"cookies"',
        "content-security-policy-report-only": "default-src 'none'",
        host: "sandbox.internal",
        "set-cookie": "nanocodex=overwritten",
      },
    }));
    sandboxSdk.getSandbox.mockReturnValue({ containerFetch, wsConnect: vi.fn() });
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
      fakeNamespace(), "preview-session", 8_080, request, "/app",
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
    const expired = await sealPreviewPayload(secret, `${sessionId}\n8080\n${Date.now() - 1}`);
    const malformed = await sealPreviewPayload(secret, `${sessionId}\n8080\nnot-an-expiry`);

    await expect(openSandboxPreviewCapability(secret, expired))
      .rejects.toThrow("invalid preview capability");
    await expect(openSandboxPreviewCapability(secret, malformed))
      .rejects.toThrow("invalid preview capability");
  });

  it("round-trips valid preview capabilities", async () => {
    const secret = "preview-round-trip-secret";
    const sessionId = "018f25e8-7b51-7a32-8c4d-fedcba987654";
    const url = await cloudflareSandboxPreviewUrl(
      "https://nanocodex.example", secret, sessionId, 8_080,
    );
    const capability = new URL(url).pathname.split("/")[2]!;

    await expect(openSandboxPreviewCapability(secret, capability))
      .resolves.toEqual({ sessionId, port: 8_080 });
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
      { objects: [{ key: "sessions/session/a" }, { key: "sessions/session/b" }] },
      { objects: [{ key: "sessions/session/c" }] },
      { objects: [] },
    ];
    const bucket = {
      list: vi.fn(async () => pages.shift()!),
      delete: vi.fn(async () => {}),
    } as unknown as R2Bucket;

    await deleteCloudflareSandboxWorkspace(bucket, "session");

    expect(bucket.list).toHaveBeenCalledTimes(3);
    expect(bucket.list).toHaveBeenCalledWith({ prefix: "sessions/session/", limit: 1_000 });
    expect(bucket.delete).toHaveBeenCalledWith(["sessions/session/a", "sessions/session/b"]);
    expect(bucket.delete).toHaveBeenCalledWith(["sessions/session/c"]);
  });

  it("deletes the durable agent's shared brain workspace", async () => {
    const bucket = {
      list: vi.fn()
        .mockResolvedValueOnce({ objects: [{ key: "brains/agent/artifact" }] })
        .mockResolvedValueOnce({ objects: [] }),
      delete: vi.fn(async () => {}),
    } as unknown as R2Bucket;

    await deleteCloudflareBrainWorkspace(bucket, "agent");

    expect(bucket.list).toHaveBeenCalledWith({ prefix: "brains/agent/", limit: 1_000 });
    expect(bucket.delete).toHaveBeenCalledWith(["brains/agent/artifact"]);
  });
});

function fakeSandbox() {
  const process = fakeProcess();
  return {
    exec: vi.fn(async (_command: string, _options?: { cwd: string }) => executionResult("")),
    startProcess: vi.fn(async (_command: string, options: { processId: string }) => ({
      ...process,
      id: options.processId,
    })),
    getProcess: vi.fn(async (_id: string) => null as ReturnType<typeof fakeProcess> | null),
    tunnels: { get: vi.fn(async () => ({ url: "https://preview.example" })) },
  };
}

function fakeProcess(overrides: {
  id?: string;
  status?: "starting" | "running" | "completed" | "failed" | "killed" | "error";
  exitCode?: number;
} = {}) {
  const status = overrides.status ?? "completed";
  return {
    id: overrides.id ?? "process",
    status,
    exitCode: overrides.exitCode ?? 0,
    kill: vi.fn(async () => {}),
    getStatus: vi.fn(async () => status),
    getLogs: vi.fn(async () => ({ stdout: "", stderr: "" })),
    waitForExit: vi.fn(async () => ({ exitCode: overrides.exitCode ?? 0 })),
  };
}

function preparingSandbox(initialState: "empty" | "mounted" | "occupied") {
  let mountState = initialState;
  const sandbox = {
    ...fakeSandbox(),
    mountBucket: vi.fn(async () => { mountState = "mounted"; }),
    destroy: vi.fn(async () => {}),
  };
  sandbox.exec.mockImplementation(async (command: string) => executionResult(
    command.startsWith("if mountpoint -q /workspace") ? mountState : "",
  ));
  return sandbox;
}

function namespaceSandbox() {
  let workspaceMounted = false;
  const peerMounts = new Set<string>();
  const sandbox = {
    ...fakeSandbox(),
    mountBucket: vi.fn(async (_bucket: string, path: string) => {
      if (path === "/workspace") workspaceMounted = true;
      else peerMounts.add(path);
    }),
    destroy: vi.fn(async () => {}),
  };
  sandbox.exec.mockImplementation(async (command: string) => {
    if (command.startsWith("if mountpoint -q /workspace")) {
      return executionResult(workspaceMounted ? "mounted" : "empty");
    }
    if (command.startsWith("if [ -L '/mnt-")) return executionResult("linked");
    const peer = [...peerMounts].find((root) => command.startsWith(`if mountpoint -q '${root}'`));
    if (peer !== undefined) return executionResult("mounted");
    if (command.startsWith("if mountpoint -q '/mnt-")) return executionResult("absent");
    if (command.startsWith("if mountpoint -q '/brain'")) return executionResult("absent");
    return executionResult("");
  });
  return sandbox;
}

function executionResult(stdout: string, stderr = "", exitCode = 0, duration = 1) {
  return { success: exitCode === 0, exitCode, stdout, stderr, duration };
}

function fakeNamespace(): DurableObjectNamespace<Sandbox> {
  return {} as DurableObjectNamespace<Sandbox>;
}

async function sealPreviewPayload(secret: string, payload: string): Promise<string> {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(secret));
  const key = await crypto.subtle.importKey("raw", digest, { name: "AES-GCM" }, false, ["encrypt"]);
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
