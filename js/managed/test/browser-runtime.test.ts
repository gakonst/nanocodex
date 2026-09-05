import { jsonSchema, tool } from "ai";
import type { BrowserRuntime } from "agents/browser/ai";
import { describe, expect, it, vi } from "vitest";

import {
  adaptAiSdkTools,
  BrowserbaseBrowserBinding,
  BrowserbaseSessionFactory,
  browserCdpMethodAllowed,
  browserToolInputAllowed,
  createManagedBrowserRuntime,
  CredentialSafeBrowserBinding,
  managedBrowserProvider,
  sanitizeBrowserToolResult,
} from "../src/browser-runtime";

const API_KEY = "bb_live_do_not_project_this_value";
const SESSION_ID = "session_123";
const CONNECT_URL = "wss://connect.browserbase.com/devtools?token=signed-secret";

describe("managed browser deployment policy", () => {
  it("accepts only host-selected providers", () => {
    expect(managedBrowserProvider(undefined)).toBe("cloudflare");
    expect(managedBrowserProvider("browserbase")).toBe("browserbase");
    expect(() => managedBrowserProvider("model-choice")).toThrow(
      "MANAGED_BROWSER_PROVIDER must be cloudflare or browserbase",
    );
  });

  it("configures the official runtime for durable provider-scoped reuse", async () => {
    let received: Record<string, unknown> | undefined;
    const swept = vi.fn(async () => ({ swept: [] }));
    const closed = vi.fn(async () => undefined);
    const expired = vi.fn(async () => []);
    const createRuntime = vi.fn((options) => {
      received = options as unknown as Record<string, unknown>;
      return {
        runtime: { expirePaused: expired },
        connector: { sweep: swept, closeSession: closed },
        tools: {
          browser_execute: tool({
            description: "Run browser code.",
            inputSchema: jsonSchema({
              type: "object",
              properties: { code: { type: "string" } },
              required: ["code"],
              additionalProperties: false,
            }),
            execute: async () => ({ ok: true }),
          }),
        },
      } as unknown as BrowserRuntime;
    });
    const binding = { fetch: vi.fn(async () => new Response()) };
    const runtime = await createManagedBrowserRuntime({
      ctx: { storage: { get: vi.fn(async () => undefined), put: vi.fn(async () => undefined) } } as unknown as DurableObjectState,
      env: {
        BROWSER: binding,
        LOADER: {} as WorkerLoader,
        MANAGED_BROWSER_PROVIDER: "cloudflare",
      },
      sessionId: "agent-a",
      createRuntime,
    });

    expect(runtime.provider).toBe("cloudflare");
    expect(runtime.tools.map(({ name }) => name)).toEqual(["browser_execute", "browser_handoff"]);
    expect(received?.browser).toBeInstanceOf(CredentialSafeBrowserBinding);
    expect(received?.loader).toBeDefined();
    expect(received?.quickActions).toBe(false);
    expect(received?.session).toEqual({
      mode: "reuse",
      key: "primary",
      keepAliveMs: 600_000,
    });
    expect(received?.name).toBe("managed-browser-cloudflare");

    await runtime.expireAndSweep();
    await runtime.close();
    expect(expired).toHaveBeenCalledOnce();
    expect(swept).toHaveBeenCalledWith({ maxIdleMs: 600_000 });
    expect(closed).toHaveBeenCalledOnce();
  });
});

describe("Browserbase session factory", () => {
  it("stops reading chunked responses at the byte limit", async () => {
    let cancelled = false;
    const body = new ReadableStream<Uint8Array>({
      pull(controller) {
        controller.enqueue(new Uint8Array(64 * 1024));
      },
      cancel() {
        cancelled = true;
      },
    });
    const factory = new BrowserbaseSessionFactory({
      apiKey: API_KEY,
      projectId: "project-a",
      fetch: vi.fn(async () => new Response(body, {
        status: 201,
        headers: { "content-type": "application/json" },
      })) as typeof globalThis.fetch,
    });

    await expect(factory.create(60_000)).rejects.toThrow("response exceeded the size limit");
    expect(cancelled).toBe(true);
  });

  it("creates, checks, connects, and releases without projecting signed material", async () => {
    const calls: Array<{ url: string; init: RequestInit }> = [];
    const websocketResponse = new Response(null, { status: 200 });
    Object.defineProperty(websocketResponse, "webSocket", { value: { accept() {} } });
    const fetch = vi.fn(async (input: RequestInfo | URL, init: RequestInit = {}) => {
      const url = String(input);
      calls.push({ url, init });
      if (url === "https://api.browserbase.com/v1/sessions" && init.method === "POST") {
        return Response.json({
          id: SESSION_ID,
          status: "PENDING",
          connectUrl: CONNECT_URL,
        }, { status: 201 });
      }
      if (url === `https://api.browserbase.com/v1/sessions/${SESSION_ID}`
        && init.method === "GET") {
        return Response.json({
          id: SESSION_ID,
          status: "RUNNING",
          connectUrl: CONNECT_URL,
        });
      }
      if (url === `https://api.browserbase.com/v1/sessions/${SESSION_ID}`
        && init.method === "POST") {
        return Response.json({ id: SESSION_ID, status: "COMPLETED" });
      }
      if (url === "https://connect.browserbase.com/devtools?token=signed-secret") {
        return websocketResponse;
      }
      return new Response(null, { status: 404 });
    });
    const factory = new BrowserbaseSessionFactory({
      apiKey: API_KEY,
      projectId: "project-a",
      fetch: fetch as typeof globalThis.fetch,
    });

    await expect(factory.create(123_456)).resolves.toEqual({ sessionId: SESSION_ID });
    await expect(factory.isAlive(SESSION_ID)).resolves.toBe(true);
    await expect(factory.connect(SESSION_ID)).resolves.toBe(websocketResponse);
    await expect(factory.release(SESSION_ID)).resolves.toBeUndefined();

    const createCall = calls[0]!;
    expect(new Headers(createCall.init.headers).get("x-bb-api-key")).toBe(API_KEY);
    expect(JSON.parse(String(createCall.init.body))).toEqual({
      projectId: "project-a",
      keepAlive: true,
      timeout: 124,
      browserSettings: {
        advancedStealth: false,
        solveCaptchas: false,
        verified: false,
        recordSession: false,
      },
    });
    expect(calls.at(-2)?.url).toBe("https://connect.browserbase.com/devtools?token=signed-secret");
    expect(JSON.stringify(await factory.create(60_000))).not.toContain(API_KEY);
    expect(JSON.stringify(await factory.create(60_000))).not.toContain("connect.browserbase.com");
  });

  it("adapts the Browserbase lifecycle to the official Browser Run binding surface", async () => {
    const sessions = {
      create: vi.fn(async () => ({ sessionId: SESSION_ID })),
      isAlive: vi.fn(async () => true),
      release: vi.fn(async () => undefined),
      connect: vi.fn(async () => new Response()),
    } as unknown as BrowserbaseSessionFactory;
    const binding = new BrowserbaseBrowserBinding(sessions);

    await expect((await binding.fetch(
      "https://localhost/v1/devtools/browser?keep_alive=120000",
      { method: "POST" },
    )).json()).resolves.toEqual({ sessionId: SESSION_ID });
    await expect((await binding.fetch(
      `https://localhost/v1/devtools/browser/${SESSION_ID}/json/list`,
    )).json()).resolves.toEqual([]);
    const protocol = await (await binding.fetch(
      `https://localhost/v1/devtools/browser/${SESSION_ID}/json/protocol`,
    )).json() as { domains: Array<{ domain: string }> };
    expect(protocol.domains.map(({ domain }) => domain)).toContain("Page");
    expect(JSON.stringify(protocol)).not.toMatch(/cookie|captcha|stealth/iu);
    expect((await binding.fetch(
      `https://localhost/v1/devtools/browser/${SESSION_ID}`,
      { method: "DELETE" },
    )).status).toBe(204);
    expect(sessions.create).toHaveBeenCalledWith(120_000);
    expect(sessions.release).toHaveBeenCalledWith(SESSION_ID);
    expect((await binding.fetch("https://evil.example/v1/devtools/browser")).status).toBe(404);
  });
});

describe("AI SDK browser tool adapter", () => {
  it("blocks cookie and arbitrary JavaScript CDP commands before durable logging", () => {
    expect(browserCdpMethodAllowed("Page.navigate")).toBe(true);
    expect(browserCdpMethodAllowed("DOM.getDocument")).toBe(true);
    expect(browserCdpMethodAllowed("Network.getAllCookies")).toBe(false);
    expect(browserCdpMethodAllowed("Storage.setCookies")).toBe(false);
    expect(browserCdpMethodAllowed("Network.setExtraHTTPHeaders")).toBe(false);
    expect(browserCdpMethodAllowed("Fetch.enable")).toBe(false);
    expect(browserCdpMethodAllowed("Runtime.evaluate")).toBe(false);
    expect(browserCdpMethodAllowed("Runtime.callFunctionOn")).toBe(false);
    expect(browserToolInputAllowed({ code: "return cdp.send({ method: 'Page.navigate' })" })).toBe(true);
    expect(browserToolInputAllowed({
      code: "return cdp.send({ method: 'Network.setExtraHTTPHeaders', params: { Cookie: 'sid=x' } })",
    })).toBe(false);
    expect(browserToolInputAllowed({ code: "return cdp.getLiveViewUrl()" })).toBe(false);
  });

  it("converts official tools to NamedTool and redacts provider/cookie secrets", async () => {
    const execute = vi.fn(async () => ({
      page: "https://example.com/ok",
      connectUrl: CONNECT_URL,
      cookies: [{ name: "sid", value: "raw-cookie" }],
      authorization: API_KEY,
    }));
    const tools = await adaptAiSdkTools({
      browser_execute: tool({
        description: "Run browser code.",
        inputSchema: jsonSchema({
          type: "object",
          properties: { code: { type: "string" } },
          required: ["code"],
          additionalProperties: false,
        }),
        execute,
      }),
    }, { secrets: [API_KEY] });
    const adapted = tools[0]!;
    const result = await adapted.handler({ code: "return 1" }, {
      callId: "call-1",
      parentCallId: "root",
      sessionId: "session-1",
      model: "test",
      signal: new AbortController().signal,
    });

    expect(adapted.name).toBe("browser_execute");
    expect(adapted.parameters).toEqual({
      type: "object",
      properties: { code: { type: "string" } },
      required: ["code"],
      additionalProperties: false,
    });
    expect(result).toEqual({
      page: "https://example.com/ok",
      connectUrl: "[redacted]",
      cookies: "[redacted]",
      authorization: "[redacted]",
    });
    expect(JSON.stringify(adapted)).not.toContain(API_KEY);
    expect(JSON.stringify(result)).not.toContain("raw-cookie");
    expect(execute).toHaveBeenCalledOnce();
  });

  it("replaces the foreign codemode prompt with the Rust Code Mode tool contract", async () => {
    const upstreamDescription = [
      "Execute JavaScript in a sandbox with access to connector SDKs.",
      "## Workflow",
      "Call `codemode.search(query)` before using the `cdp` connector.",
    ].join("\n");
    const tools = await adaptAiSdkTools({
      browser_execute: tool({
        description: upstreamDescription,
        inputSchema: jsonSchema({
          type: "object",
          properties: { code: { type: "string" } },
          required: ["code"],
          additionalProperties: false,
        }),
        execute: async () => ({ ok: true }),
      }),
      browser_markdown: tool({
        description: "Read a page as Markdown.",
        inputSchema: jsonSchema({ type: "object", additionalProperties: false }),
        execute: async () => "page",
      }),
    });
    const adapted = tools.find(({ name }) => name === "browser_execute");
    const ordinary = tools.find(({ name }) => name === "browser_markdown");

    expect(adapted?.description).toContain(
      "Outer contract (Nanocodex Rust/WASM Code Mode)",
    );
    expect(adapted?.description).toContain(
      "nested tools exist only on `tools.*`; `cdp` and `codemode` are not globals",
    );
    expect(adapted?.description).toContain("`await tools.browser_execute({ code })`");
    expect(adapted?.description).toContain(
      "only host globals are `cdp` and `codemode`",
    );
    expect(adapted?.description).toContain(
      '`await codemode.search("short intent")`',
    );
    expect(adapted?.description).toContain(
      '`await codemode.describe("cdp.method")`',
    );
    expect(adapted?.description).toContain(
      '`await cdp.send({ method: "Target.getTargets" })`',
    );
    expect(adapted?.description).toContain("including `Runtime.evaluate`");
    expect(adapted?.description).toContain("`Target.getTargetInfo` is not available");
    expect(adapted?.description).toContain("use `tools.web__run(...)`");
    expect(adapted?.description).not.toContain(upstreamDescription);
    expect(adapted?.description).not.toContain("## Workflow");
    expect(ordinary?.description).toBe("Read a page as Markdown.");
  });

  it("redacts provider URLs and scalar cookie material", () => {
    expect(sanitizeBrowserToolResult({
      provider: "https://live.browser.run/session/signed",
      scalar: "session_id=private; theme=dark",
      ordinary: "https://example.com/page",
    })).toEqual({
      provider: "[redacted provider URL]",
      scalar: "[redacted cookie material]",
      ordinary: "https://example.com/page",
    });
  });
});
