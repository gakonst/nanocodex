import { afterEach, describe, expect, it, vi } from "vitest";
import type { ToolMap } from "nanocodex";

import {
  Sandbox,
  handleSandboxEgress,
  isCrossBindingR2Copy,
} from "../src/sandbox-runtime";
import { cloudflareSandboxPreviewUrl } from "../src/sandbox-tools";
import {
  createSharedBrainReadWorkspace,
  createManagedNamespaceTools,
  routeSandboxPreviewRequest,
  turnControlAuthorizationMatches,
  turnCanUseExecutionNamespace,
} from "../src/index";

afterEach(() => vi.unstubAllGlobals());

describe("sandbox runtime egress", () => {
  it("installs the managed policy as the catch-all outbound handler", () => {
    expect(Sandbox.outbound).toBe(handleSandboxEgress);
  });

  it("allows policy-validated public HTTPS without using account credentials", async () => {
    const upstream = vi.fn(async () => new Response("ok", {
      headers: { "content-type": "text/plain" },
    }));
    vi.stubGlobal("fetch", upstream);
    const broker = { fetch: vi.fn() } as unknown as Fetcher;

    const response = await handleSandboxEgress(
      new Request("https://github.com/dtolnay/anyhow.git/info/refs?service=git-upload-pack", {
        headers: { host: "github.com" },
      }),
      { NANOCODEX: broker },
    );

    expect(response.status).toBe(200);
    expect(await response.text()).toBe("ok");
    expect(upstream).toHaveBeenCalledTimes(1);
    expect(broker.fetch).not.toHaveBeenCalled();
  });

  it("rejects account connector destinations and private network targets", async () => {
    const upstream = vi.fn();
    vi.stubGlobal("fetch", upstream);
    const broker = { fetch: vi.fn() } as unknown as Fetcher;

    expect((await handleSandboxEgress(
      new Request("https://api.github.com/user"),
      { NANOCODEX: broker },
    )).status).toBe(403);
    expect((await handleSandboxEgress(
      new Request("http://127.0.0.1:8787/secret"),
      { NANOCODEX: broker },
    )).status).toBe(403);
    expect(upstream).not.toHaveBeenCalled();
    expect(broker.fetch).not.toHaveBeenCalled();
  });

  it("blocks the Sandbox SDK cross-binding copy prefix escape", () => {
    expect(isCrossBindingR2Copy(new Request(
      "http://r2.internal/NANOCODEX_WORKSPACES_0/authorized/destination",
      { method: "PUT", headers: {
        "x-amz-copy-source": "/NANOCODEX_WORKSPACES_1/sessions/another-agent/secret",
      } },
    ))).toBe(true);
    expect(isCrossBindingR2Copy(new Request(
      "http://r2.internal/NANOCODEX_WORKSPACES_0/authorized/destination",
      { method: "PUT", headers: {
        "x-amz-copy-source": "/NANOCODEX_WORKSPACES_0/authorized/source",
      } },
    ))).toBe(false);
  });
});

describe("managed sandbox preview wiring", () => {
  it("reads shared /brain files from the durable R2 prefix and preserves private fallback reads", async () => {
    const bucket = {
      get: vi.fn(async () => ({ arrayBuffer: async () => new Uint8Array([1, 2, 3]).buffer })),
    } as unknown as R2Bucket;
    const fallback = { readFile: vi.fn(async () => new Uint8Array([9])) };
    const workspace = createSharedBrainReadWorkspace(bucket, "durable-agent", fallback);

    await expect(workspace.readFile("/brain/output.png")).resolves.toEqual(new Uint8Array([1, 2, 3]));
    expect(bucket.get).toHaveBeenCalledWith("brains/durable-agent/output.png");
    await expect(workspace.readFile("/workspace/private.png")).resolves.toEqual(new Uint8Array([9]));
    expect(fallback.readFile).toHaveBeenCalledWith("/workspace/private.png");
    await expect(workspace.readFile("/brain/../secret")).rejects.toThrow("canonical file");
  });

  it("reserves retained execution hands for full account authority", () => {
    expect(turnCanUseExecutionNamespace({
      capabilities: ["agents:write", "tools:use"],
    })).toBe(true);
    expect(turnCanUseExecutionNamespace({
      capabilities: ["agents:write", "tools:use"],
      connectGrant: {
        grantId: `0x${"a".repeat(64)}`,
        connectors: ["chatgpt"],
        mcpIds: [],
      },
    })).toBe(false);
    expect(turnCanUseExecutionNamespace({ capabilities: ["agents:write"] })).toBe(false);
    expect(turnCanUseExecutionNamespace(undefined)).toBe(false);
  });

  it("prevents a Connect grant from steering a turn with different authority", () => {
    const account = { capabilities: ["agents:write", "tools:use"] as const };
    const connect = {
      capabilities: ["agents:write", "tools:use"] as const,
      connectGrant: {
        grantId: `0x${"a".repeat(64)}`,
        connectors: ["chatgpt"] as const,
        mcpIds: [],
      },
    };
    expect(turnControlAuthorizationMatches(account, account)).toBe(true);
    expect(turnControlAuthorizationMatches(connect, connect)).toBe(true);
    expect(turnControlAuthorizationMatches(account, connect)).toBe(false);
  });

  it("routes mounted hands only when the active turn has execution authority", async () => {
    const sourceHandler = vi.fn(async () => ({ ok: true }));
    const sourceTools: ToolMap = {
      exec_command: {
        description: "exec",
        parameters: { type: "object", additionalProperties: false },
        handler: sourceHandler,
      },
      write_stdin: {
        description: "poll",
        parameters: { type: "object", additionalProperties: false },
        handler: sourceHandler,
      },
      preview: {
        description: "preview",
        parameters: { type: "object", additionalProperties: false },
        handler: sourceHandler,
      },
    };
    let executionAuthorized = true;
    const tools = createManagedNamespaceTools(
      () => executionAuthorized,
      () => [{ id: "sandbox:test", root: "/test", workspace: "/workspace" }],
      (_machineId, name) => sourceTools[name],
    );

    expect(tools.map(({ name }) => name)).toEqual(["exec_command", "write_stdin", "preview"]);
    await expect(tools[0]!.handler(
      { cmd: "pwd", workdir: "/test" },
      toolContext(),
    )).resolves.toEqual({ ok: true });
    expect(sourceHandler).toHaveBeenCalledTimes(1);

    executionAuthorized = false;
    await expect(tools[0]!.handler({}, toolContext())).rejects.toMatchObject({
      status: 403,
      code: "namespace_forbidden",
      message: "the current authorization cannot use execution hands",
    });
    expect(sourceHandler).toHaveBeenCalledTimes(1);
  });

  it("refreshes an epoch-bound retained route once before a new subagent namespace snapshot", async () => {
    const oldRoute = "vm-host:33333333-3333-4333-8333-333333333333:1";
    const newRoute = "vm-host:33333333-3333-4333-8333-333333333333:2";
    let currentRoute = oldRoute;
    let retainedRoute = oldRoute;
    const oldExec = vi.fn(async () => ({ route: oldRoute }));
    const newExec = vi.fn(async () => ({ route: newRoute }));
    const refresh = vi.fn(async () => { retainedRoute = currentRoute; });
    const tools = createManagedNamespaceTools(
      () => true,
      () => [{ id: "sandbox:retained", root: "/repo", workspace: "/workspace" }],
      (_machineId, name) => name === "exec_command"
        ? { handler: retainedRoute === oldRoute ? oldExec : newExec }
        : undefined,
      refresh,
    );
    const exec = tools.find(({ name }) => name === "exec_command")!;
    const original = toolContext();

    await expect(exec.handler({ cmd: "pwd", workdir: "/repo" }, original))
      .resolves.toEqual({ route: oldRoute });
    currentRoute = newRoute;
    await expect(exec.handler({ cmd: "still pinned", workdir: "/repo" }, {
      ...original,
      callId: "same-cell",
    })).resolves.toEqual({ route: oldRoute });

    const subagent = {
      ...original,
      callId: "subagent-call",
      parentCallId: "subagent-cell",
      sessionId: "subagent-session",
      subagent: {
        agentId: "2",
        parentAgentId: null,
        sessionId: "subagent-session",
        role: "builder",
        task: "continue in retained cwd",
      },
    };
    await expect(Promise.all([
      exec.handler({ cmd: "one", workdir: "/repo" }, subagent),
      exec.handler({ cmd: "two", workdir: "/repo" }, { ...subagent, callId: "parallel" }),
    ])).resolves.toEqual([{ route: newRoute }, { route: newRoute }]);

    expect(refresh).toHaveBeenCalledTimes(2);
    expect(oldExec).toHaveBeenCalledTimes(2);
    expect(newExec).toHaveBeenCalledTimes(2);
  });

  it("rechecks child authorization before dispatching through a cached namespace binding", async () => {
    const sourceHandler = vi.fn(async () => ({ ok: true }));
    const authorized = new Set(["subagent-session"]);
    const tools = createManagedNamespaceTools(
      (context) => authorized.has(context.sessionId),
      () => [{ id: "sandbox:retained", root: "/repo", workspace: "/workspace" }],
      (_machineId, name) => name === "exec_command" ? { handler: sourceHandler } : undefined,
    );
    const exec = tools.find(({ name }) => name === "exec_command")!;
    const child = {
      ...toolContext(),
      sessionId: "subagent-session",
      parentCallId: "cached-cell",
      subagent: {
        agentId: "2",
        parentAgentId: null,
        sessionId: "subagent-session",
        role: "builder",
        task: "use retained cwd",
      },
    };

    await expect(exec.handler({ cmd: "pwd", workdir: "/repo" }, child))
      .resolves.toEqual({ ok: true });
    authorized.clear();
    await expect(exec.handler({ cmd: "pwd", workdir: "/repo" }, {
      ...child,
      callId: "later-call",
    })).rejects.toMatchObject({ status: 403, code: "namespace_forbidden" });
    expect(sourceHandler).toHaveBeenCalledTimes(1);
  });

  it("opens a bearer capability and strips only its route prefix before proxying", async () => {
    const namespace = {} as DurableObjectNamespace<Sandbox>;
    const secret = "server-only-preview-secret";
    const sessionId = "018f25e8-7b51-7a32-8c4d-0123456789ab";
    const publicUrl = await cloudflareSandboxPreviewUrl(
      "https://nanocodex.example",
      secret,
      sessionId,
      4_321,
    );
    const request = new Request(`${publicUrl}nested/resource?value=kept`, {
      method: "PUT",
      body: "payload",
    });
    const proxy = vi.fn(async () => new Response("proxied", { status: 202 }));

    const response = await routeSandboxPreviewRequest(request, {
      NANOCODEX_ADMIN_TOKEN: secret,
      NANOCODEX_SANDBOXES: namespace,
    }, new URL(request.url), undefined, proxy);

    expect(response?.status).toBe(202);
    expect(await response?.text()).toBe("proxied");
    expect(proxy).toHaveBeenCalledWith(
      namespace,
      sessionId,
      4_321,
      request,
      "/nested/resource",
    );
    expect(request.method).toBe("PUT");
    expect(new URL(request.url).search).toBe("?value=kept");
  });

  it("passes WebSocket upgrades through and rejects invalid or unconfigured capabilities", async () => {
    const namespace = {} as DurableObjectNamespace<Sandbox>;
    const secret = "server-only-preview-secret";
    const sessionId = "018f25e8-7b51-7a32-8c4d-abcdef012345";
    const publicUrl = await cloudflareSandboxPreviewUrl(
      "https://nanocodex.example",
      secret,
      sessionId,
      8_080,
    );
    const websocket = new Request(publicUrl, { headers: { upgrade: "websocket" } });
    const proxy = vi.fn(async () => new Response(null, { status: 200 }));

    await routeSandboxPreviewRequest(websocket, {
      NANOCODEX_ADMIN_TOKEN: secret,
      NANOCODEX_SANDBOXES: namespace,
    }, new URL(websocket.url), undefined, proxy);
    expect(proxy).toHaveBeenCalledWith(
      namespace,
      sessionId,
      8_080,
      websocket,
      "/",
    );

    proxy.mockClear();
    const invalid = await routeSandboxPreviewRequest(
      new Request("https://nanocodex.example/sandbox-preview/not-a-capability/private"),
      { NANOCODEX_ADMIN_TOKEN: secret, NANOCODEX_SANDBOXES: namespace },
      undefined,
      undefined,
      proxy,
    );
    expect(invalid?.status).toBe(404);
    const unconfigured = await routeSandboxPreviewRequest(
      new Request(publicUrl),
      { NANOCODEX_ADMIN_TOKEN: "", NANOCODEX_SANDBOXES: namespace },
      undefined,
      undefined,
      proxy,
    );
    expect(unconfigured?.status).toBe(404);
    expect(proxy).not.toHaveBeenCalled();
  });
});

function toolContext() {
  return {
    callId: "call",
    model: "gpt-5.6-sol",
    parentCallId: "parent",
    sessionId: "session",
    signal: new AbortController().signal,
  };
}
