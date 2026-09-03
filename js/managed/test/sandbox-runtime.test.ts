import { afterEach, describe, expect, it, vi } from "vitest";
import type { ToolMap } from "nanocodex";

import {
  Sandbox,
  handleSandboxEgress,
} from "../src/sandbox-runtime";
import { cloudflareSandboxPreviewUrl } from "../src/sandbox-tools";
import {
  createManagedNamespaceTools,
  routeSandboxPreviewRequest,
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
});

describe("managed sandbox preview wiring", () => {
  it("constructs account sandbox tools with the session origin and server secret", async () => {
    const namespace = {} as DurableObjectNamespace<Sandbox>;
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
    const factory = vi.fn(() => sourceTools) as unknown as typeof import("../src/sandbox-tools").cloudflareSandboxTools;
    let accountOwned = true;
    const tools = createManagedNamespaceTools({
      NANOCODEX_ADMIN_TOKEN: "server-only-preview-secret",
      NANOCODEX_SANDBOXES: namespace,
      NANOCODEX_SANDBOX_LOCAL: "true",
    }, {
      session_id: "session-id",
      public_origin: "https://nanocodex.example",
    }, () => accountOwned, factory);

    expect(factory).toHaveBeenCalledWith(
      namespace,
      "session-id",
      true,
      "https://nanocodex.example",
      "server-only-preview-secret",
    );
    expect(tools.map(({ name }) => name)).toEqual(["exec_command", "write_stdin", "preview"]);
    await expect(tools[0]!.handler({ cmd: "pwd" }, toolContext())).resolves.toEqual({ ok: true });
    expect(sourceHandler).toHaveBeenCalledTimes(1);

    accountOwned = false;
    await expect(tools[0]!.handler({}, toolContext())).rejects.toMatchObject({
      status: 403,
      code: "namespace_forbidden",
    });
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
