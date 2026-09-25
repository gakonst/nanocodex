import { afterEach, describe, expect, it, vi } from "vitest";
import type { ToolMap } from "nanocodex";

import {
  Sandbox,
  handleSandboxEgress,
  isCrossBindingR2Copy,
} from "../src/sandbox-runtime";
import { cloudflareSandboxPreviewUrl } from "../src/sandbox-tools";
import {
  createManagedNamespaceTools,
  routeSandboxPreviewRequest,
} from "../src/index";

afterEach(() => vi.unstubAllGlobals());

describe("sandbox runtime egress", () => {

  it("routes policy-validated public HTTPS through the broker", async () => {
    const upstream = vi.fn(async () => new Response("ok", {
      headers: { "content-type": "text/plain" },
    }));
    vi.stubGlobal("fetch", upstream);
    const broker = { fetch: vi.fn(async () => new Response("ok")) } as unknown as Fetcher;

    const response = await handleSandboxEgress(
      new Request("https://github.com/dtolnay/anyhow.git/info/refs?service=git-upload-pack", {
        headers: { host: "github.com" },
      }),
      { NANOCODEX: broker },
    );

    expect(response.status).toBe(200);
    expect(await response.text()).toBe("ok");
    expect(upstream).not.toHaveBeenCalled();
    expect(broker.fetch).toHaveBeenCalledTimes(1);
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

  it("authenticates native gh and git using trusted outbound params, not caller identity", async () => {
    const requests: Request[] = [];
    const broker = { async fetch(request: Request) { requests.push(request); return new Response("ok"); } } as unknown as Fetcher;
    const subject = "s".repeat(43);
    for (const [url, authorization] of [
      ["https://api.github.com/user", "token NANOCODEX_PROVIDER_CREDENTIAL"],
      ["https://github.com/owner/private.git/info/refs?service=git-upload-pack", undefined],
      ["https://github.com/owner/private.git/git-upload-pack", `Basic ${btoa("x-access-token:NANOCODEX_PROVIDER_CREDENTIAL")}`],
    ]) {
      const response = await handleSandboxEgress(new Request(url!, {
        headers: { host: new URL(url!).hostname, ...(authorization ? { authorization } : {}) },
      }), { NANOCODEX: broker }, { params: { subject } });
      expect(response.status).toBe(200);
      expect(requests.at(-1)!.headers.get("x-nanocodex-subject")).toBe(subject);
      expect(requests.at(-1)!.headers.get("authorization")).toBe("Bearer NANOCODEX_PROVIDER_CREDENTIAL");
    }
    const forgedHeaders: Record<string, string>[] = [
      { "x-nanocodex-subject": "a".repeat(43) },
      { authorization: "Bearer real-user-token" },
    ];
    for (const headers of forgedHeaders) {
      const denied = await handleSandboxEgress(new Request("https://api.github.com/user", { headers }),
        { NANOCODEX: broker }, { params: { subject } });
      expect(denied.status).toBe(403);
    }
    expect(requests).toHaveLength(3);
  });

  it("binds grant sandboxes to public-only egress and cannot rebind their owner", async () => {
    const values = new Map<string, unknown>();
    const runtime = Object.create(Sandbox.prototype) as Sandbox;
    const setOutboundHandler = vi.fn();
    Object.assign(runtime, { ctx: {
      blockConcurrencyWhile: (run: () => Promise<void>) => run(),
      storage: { get: async (key: string) => values.get(key), put: async (key: string, value: unknown) => { values.set(key, value); } },
    }, setOutboundHandler });
    const subject = "s".repeat(43), grantId = `0x${"a".repeat(64)}`;
    await runtime.bindAccountEgress(subject, grantId);
    await runtime.bindAccountEgress(subject, grantId);
    await expect(runtime.bindAccountEgress(subject)).rejects.toThrow();
    await expect(runtime.bindAccountEgress(subject, `0x${"b".repeat(64)}`)).rejects.toThrow();
    const params = setOutboundHandler.mock.calls[0]![1];
    const broker = { fetch: vi.fn(async () => new Response("public")) } as unknown as Fetcher;
    expect((await handleSandboxEgress(new Request("https://pypi.org/simple/"), { NANOCODEX: broker }, { params })).status).toBe(200);
    for (const [url, headers] of [
      ["https://api.github.com/user", {}],
      ["https://pypi.org/simple/", { "x-nanocodex-vault-id": "v".repeat(32) }],
      ["https://api.github.com/user", { "x-nanocodex-subject": subject }],
      ["https://nanocodex-hand.internal/v1/hand-hosts/owner/id/hands/host", {}],
    ] as const) {
      expect((await handleSandboxEgress(new Request(url, { headers }), { NANOCODEX: broker }, { params })).status).toBe(403);
    }
    expect(broker.fetch).toHaveBeenCalledTimes(1);
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

    expect(tools.map(({ name }) => name)).toEqual(["mcp__cua_repl__js", "mcp__cua_repl__js_reset", "exec_command", "write_stdin", "preview"]);
    const exec = tools.find(({ name }) => name === "exec_command")!;
    await expect(exec.handler(
      { cmd: "pwd", workdir: "/test" },
      toolContext(),
    )).resolves.toEqual({ ok: true });
    expect(sourceHandler).toHaveBeenCalledTimes(1);

    executionAuthorized = false;
    await expect(exec.handler({}, toolContext())).rejects.toMatchObject({
      status: 403,
      code: "namespace_forbidden",
      message: "the current authorization cannot use execution hands",
    });
    expect(sourceHandler).toHaveBeenCalledTimes(1);
  });

  it("rechecks execution authority before invoking a captured CUA provider", async () => {
    let allowed = true;
    const invoke = vi.fn(async () => ({ content: [] }));
    const tools = createManagedNamespaceTools(() => allowed,
      () => [{ id: "desktop", workspace: "/" }],
      (_id, name) => name.startsWith("mcp__cua_repl__") ? { handler: invoke, definition: { description: "Fixture CUA provider", parameters: { type: "object", additionalProperties: true } } } : undefined);
    await tools.find(tool => tool.name === "mcp__cua_repl__js")!.handler({ workdir: "/desktop" }, toolContext());
    allowed = false;
    await expect(tools.find(tool => tool.name === "mcp__cua_repl__js")!.handler({ workdir: "/desktop", code: "1" }, toolContext()))
      .rejects.toMatchObject({ status: 403, code: "namespace_forbidden" });
    expect(invoke).not.toHaveBeenCalled();
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
});

function toolContext() {
  return {
    callId: "call",
    model: "gpt-6-sol",
    parentCallId: "parent",
    sessionId: "session",
    signal: new AbortController().signal,
  };
}
