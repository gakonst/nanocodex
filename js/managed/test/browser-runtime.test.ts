import { jsonSchema, tool } from "ai";
import type { BrowserRuntime } from "agents/browser/ai";
import { describe, expect, it, vi } from "vitest";
import { BROWSER_VAULT_OTP_FUNCTION, PrivateBrowserCdp } from "../src/browser-vault";

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
      ctx: { storage: {} } as DurableObjectState,
      env: {
        BROWSER: binding,
        LOADER: {} as WorkerLoader,
        MANAGED_BROWSER_PROVIDER: "cloudflare",
      },
      sessionId: "agent-a",
      createRuntime,
    });

    expect(runtime.provider).toBe("cloudflare");
    expect(runtime.tools.map(({ name }) => name)).toEqual(["browser_execute"]);
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


describe("Vault browser isolation", () => {
  it("drops unsolicited provider events before they reach SDK logs during credential isolation", async () => {
    const pair = new WebSocketPair();
    pair[1].accept();
    let isolated = false;
    const binding = new CredentialSafeBrowserBinding({ fetch: async () => new Response(null, { status: 101, webSocket: pair[0] }) }, [], () => isolated);
    const response = await binding.fetch("https://localhost/v1/devtools/browser/session");
    const client = response.webSocket!;
    client.accept();
    const messages: string[] = [];
    client.addEventListener("message", event => { messages.push(String(event.data)); });
    pair[1].send(JSON.stringify({ method: "Page.frameNavigated", params: { url: "https://login.example" } }));
    await vi.waitFor(() => expect(messages).toHaveLength(1));
    isolated = true;
    pair[1].send(JSON.stringify({ method: "Page.frameNavigated", params: { url: "https://login.example/?echo=encoded-secret" } }));
    client.send(JSON.stringify({ id: 1, method: "Target.getTargets" }));
    await vi.waitFor(() => expect(messages).toHaveLength(2));
    expect(messages[1]).toContain("blocked by browser credential policy");
    expect(JSON.stringify(messages)).not.toContain("encoded-secret");
    client.close(); pair[1].close();
  });

  it("blocks ordinary page reads across navigation and rehydration, while allowing bounded status and explicit close", async () => {
    const stored = new Map<string, unknown>();
    const context = { callId: "vault", sessionId: "root", parentCallId: "root", model: "test", signal: new AbortController().signal };
    const ordinary = vi.fn(async () => ({ page: "fixture" }));
    let formResult: unknown = true;
    const send = vi.fn(async (method: string) => {
      if (method === "Target.getTargetInfo") return { targetInfo: { type: "page", url: "https://login.example/next" } };
      if (method === "Target.attachToTarget") return { sessionId: "attached" };
      if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "top", loaderId: "next-document", url: "https://login.example/next" } } };
      if (method === "Page.createIsolatedWorld") return { executionContextId: 7 };
      return { result: { value: formResult } };
    });
    const connect = vi.spyOn(PrivateBrowserCdp, "connect").mockResolvedValue({ send, close() {} } as unknown as PrivateBrowserCdp);
    const close = vi.fn(async () => {});
    const resolve = vi.fn(async () => ({ username: "fake-user", password: "fake-password" }));
    const create = () => createManagedBrowserRuntime({
      ctx: { storage: { get: async (key: string) => stored.get(key), put: async (key: string, value: unknown) => { stored.set(key, value); }, delete: async (key: string) => stored.delete(key) } } as unknown as DurableObjectState,
      env: { BROWSER: { fetch: vi.fn() }, LOADER: {} as WorkerLoader }, sessionId: "agent-vault",
      resolveVaultLogin: resolve, authorizeVaultAccess: () => {},
      createRuntime: () => ({ connector: { sessionInfo: async () => ({ sessionId: "browser-1" }), closeSession: close },
        tools: { browser_execute: tool({ inputSchema: jsonSchema({ type: "object" }), execute: ordinary }) }, runtime: {} }) as unknown as BrowserRuntime,
    });
    try {
      let runtime = await create();
      const call = (name: string, input: unknown) => runtime.tools.find(t => t.name === name)!.handler(input, context);
      const reference = { vault_id: "a".repeat(22), expected_origin: "https://login.example", target_id: "tab1" };
      expect(await call("browser_vault_fill", { ...reference, username_selector: "#user", submit: true })).toEqual({ status: "submitted" });
      formResult = "unsupported";
      expect(await call("browser_vault_fill", { ...reference, username_selector: "#user", submit: true }))
        .toEqual({ status: "filled", submission: "action_required" });
      formResult = true;
      expect(JSON.stringify([...stored.values()])).not.toContain("fake-password");
      await expect(call("browser_execute", { code: "await cdp.send({method:'DOM.getDocument'})" })).rejects.toThrow("isolated");
      runtime = await create();
      await expect(call("browser_execute", { code: "await cdp.send({method:'Target.getTargets'})" })).rejects.toThrow("isolated");
      await expect(call("browser_vault_fill", { ...reference, target_id: "other", password_selector: "#pass", submit: true })).rejects.toThrow("isolated");
      formResult = { status: "password_form", flags: [false, true, false] };
      expect(await call("browser_vault_status", reference)).toEqual({ status: "password_form", password_selector: 'input[type="password"]' });
      expect(ordinary).not.toHaveBeenCalled();
      expect(await call("browser_vault_close", {})).toEqual({ status: "closed" });
      expect(close).toHaveBeenCalledOnce();
      expect(await call("browser_execute", { code: "1" })).toEqual({ page: "fixture" });
    } finally { connect.mockRestore(); }
  });
});

describe("private browser verification lifecycle", () => {
  const identity = { vault_id: "a".repeat(22), expected_origin: "https://login.example", target_id: "tab1" };
  const context = { callId: "vault", sessionId: "root", parentCallId: "root", model: "test", signal: new AbortController().signal };
  const challengeKey = "browser-vault-challenge:cloudflare:agent-vault";
  const quarantineKey = "browser-vault-quarantine:cloudflare:agent-vault";
  async function fixture() {
    const stored = new Map<string, unknown>();
    const state = { session: "browser-1", loader: "loader-1", origin: identity.expected_origin, ambiguous: false, otp: true };
    const resolve = vi.fn(async () => ({ username: "fresh-account-user", password: "fresh-account-password" }));
    const injection = vi.fn(async () => {
      expect(stored.has(challengeKey)).toBe(false);
      if (state.ambiguous) throw new Error("socket closed after submission");
      return { result: { value: true } };
    });
    const send = vi.fn(async (method: string, params: Record<string, any> = {}) => {
      if (method === "Target.getTargetInfo") {
        expect(params.targetId).toBe(identity.target_id);
        return { targetInfo: { type: "page", url: state.origin + "/verify" } };
      }
      if (method === "Target.attachToTarget") return { sessionId: "attached" };
      if (method === "Page.getFrameTree") return { frameTree: { frame: { id: "top", loaderId: state.loader, url: state.origin + "/verify" } } };
      if (method === "Page.createIsolatedWorld") return { executionContextId: 7 };
      if (params.functionDeclaration === BROWSER_VAULT_OTP_FUNCTION) return injection();
      const status = { status: state.otp ? "otp_form" : "unknown", flags: [false, false, state.otp] };
      if (params.arguments?.[1]?.value === "snapshot") return { result: { value: { ...status,
        snapshot_id: params.arguments[2].value, title: "Account", text: "fresh-account-user fresh-account-password", elements: [] } } };
      return { result: { value: status } };
    });
    const connect = vi.spyOn(PrivateBrowserCdp, "connect").mockResolvedValue({ send, close() {} } as unknown as PrivateBrowserCdp);
    const create = () => createManagedBrowserRuntime({
      ctx: { storage: { get: async (key: string) => stored.get(key), put: async (key: string, value: unknown) => { stored.set(key, structuredClone(value)); }, delete: async (key: string) => stored.delete(key) } } as unknown as DurableObjectState,
      env: { BROWSER: { fetch: vi.fn() }, LOADER: {} as WorkerLoader }, sessionId: "agent-vault",
      resolveVaultLogin: resolve, authorizeVaultAccess: () => {},
      createRuntime: () => ({ connector: { sessionInfo: async () => ({ sessionId: state.session }), closeSession: async () => {} },
        tools: { browser_execute: tool({ inputSchema: jsonSchema({ type: "object" }), execute: async () => ({ page: "ordinary" }) }) }, runtime: {} }) as unknown as BrowserRuntime,
    });
    const runtime = await create();
    const call = (name: string, input: unknown = identity, current = runtime) => current.tools.find(t => t.name === name)!.handler(input, context);
    const request = async () => await call("browser_vault_request_challenge") as { challenge_id: string; expires_at: number };
    return { runtime, create, call, request, connect, stored, state, resolve, injection, send };
  }
  it.each(["1234", "1234567890"])("accepts %s through direct intake and keeps quarantine", async code => {
    const f = await fixture();
    try {
      const challenge = await f.request();
      expect(challenge).toMatchObject({ type: "browser_vault_challenge", status: "input_required", agent_id: "agent-vault", origin: identity.expected_origin });
      expect(challenge.expires_at - Date.now()).toBeGreaterThan(290_000);
      expect(f.runtime.tools.some(t => t.name.includes("submit"))).toBe(false);
      await expect(f.runtime.submitVaultChallenge({ challenge_id: challenge.challenge_id, code }, context.signal)).resolves.toEqual({ type: "browser_vault_challenge_receipt", status: "submitted", challenge_id: challenge.challenge_id });
      expect(f.injection).toHaveBeenCalledOnce();
      const injected = f.send.mock.calls.find(([, params]) => params?.functionDeclaration === BROWSER_VAULT_OTP_FUNCTION);
      expect(injected?.[1]?.arguments.map((arg: { value: unknown }) => arg.value)).toEqual([identity.expected_origin, expect.any(String), code, true]);
      expect(f.stored.has(quarantineKey)).toBe(true);
      await expect(f.call("browser_execute", { code: "1" })).rejects.toThrow("isolated");
      await expect(f.runtime.submitVaultChallenge({ challenge_id: challenge.challenge_id, code }, context.signal)).rejects.toThrow("unavailable");
      expect(JSON.stringify([...f.stored.values()])).not.toContain(code);
    } finally { f.connect.mockRestore(); }
  });
  it.each(["123", "12345678901", "123a", " 1234", "１２３４", 1234])("rejects invalid code %s without consuming the challenge", async code => {
    const f = await fixture();
    try {
      const challenge = await f.request();
      await expect(f.runtime.submitVaultChallenge({ challenge_id: challenge.challenge_id, code }, context.signal)).rejects.toThrow("Invalid");
      expect(f.stored.has(challengeKey)).toBe(true);
      expect(f.injection).not.toHaveBeenCalled();
    } finally { f.connect.mockRestore(); }
  });
  it.each(["session", "loader", "origin", "target"])("rejects changed %s binding before injection", async binding => {
    const f = await fixture();
    try {
      const challenge = await f.request();
      if (binding === "session") f.state.session = "browser-2";
      if (binding === "loader") f.state.loader = "loader-2";
      if (binding === "origin") f.state.origin = "https://other.example";
      if (binding === "target") f.stored.set(quarantineKey, { ...f.stored.get(quarantineKey) as object, targetId: "other-tab" });
      await expect(f.runtime.submitVaultChallenge({ challenge_id: challenge.challenge_id, code: "123456" }, context.signal)).rejects.toThrow();
      expect(f.injection).not.toHaveBeenCalled();
      if (binding !== "session") expect(f.stored.has(quarantineKey)).toBe(true);
    } finally { f.connect.mockRestore(); }
  });
  it("expires challenges and consumes an ambiguous submission before replay", async () => {
    const f = await fixture();
    try {
      const expired = await f.request();
      f.stored.set(challengeKey, { ...f.stored.get(challengeKey) as object, expiresAt: Date.now() - 1 });
      await expect(f.runtime.submitVaultChallenge({ challenge_id: expired.challenge_id, code: "123456" }, context.signal)).rejects.toThrow("expired");
      expect(f.injection).not.toHaveBeenCalled();
      const challenge = await f.request();
      f.state.ambiguous = true;
      await expect(f.runtime.submitVaultChallenge({ challenge_id: challenge.challenge_id, code: "123456" }, context.signal)).rejects.toThrow("could not be confirmed");
      expect(f.stored.has(challengeKey)).toBe(false);
      await expect(f.runtime.submitVaultChallenge({ challenge_id: challenge.challenge_id, code: "123456" }, context.signal)).rejects.toThrow("unavailable");
      expect(f.injection).toHaveBeenCalledOnce();
      expect(f.stored.has(quarantineKey)).toBe(true);
      await expect(f.call("browser_execute", { code: "1" })).rejects.toThrow("isolated");
    } finally { f.connect.mockRestore(); }
  });
  it("rehydrates metadata without secrets and resolves fresh snapshot redaction credentials", async () => {
    const f = await fixture();
    try {
      const challenge = await f.request();
      const serialized = JSON.stringify([...f.stored.values()]);
      expect(serialized).not.toContain("fresh-account");
      expect(serialized).not.toContain('"code"');
      const recreated = await f.create();
      await expect(recreated.submitVaultChallenge({ challenge_id: challenge.challenge_id, code: "123456" }, context.signal)).resolves.toMatchObject({ status: "submitted" });
      f.resolve.mockClear();
      const snapshot = await f.call("browser_vault_snapshot", identity, recreated);
      expect(f.resolve).toHaveBeenCalledOnce();
      expect(JSON.stringify(snapshot)).not.toContain("fresh-account");
      expect(JSON.stringify(snapshot)).toContain("[redacted]");
      await expect(f.call("browser_execute", { code: "1" }, recreated)).rejects.toThrow("isolated");
    } finally { f.connect.mockRestore(); }
  });
  it.each(["browser_vault_request_challenge", "browser_vault_snapshot", "browser_vault_action"])("rejects unauthorized resolver access for %s", async name => {
    const f = await fixture();
    try {
      f.resolve.mockRejectedValue(new Error("not authorized"));
      await expect(f.call(name, name === "browser_vault_action" ? { ...identity, action: "navigate", url: identity.expected_origin + "/account" } : identity)).rejects.toThrow();
      expect(f.resolve).toHaveBeenCalledOnce();
      expect(f.connect).not.toHaveBeenCalled();
      expect(f.stored.size).toBe(0);
    } finally { f.connect.mockRestore(); }
  });
  it("persists exclusive human control across recreation and resumes only private reads on finish", async () => {
    const f = await fixture();
    const takeoverKey = "browser-vault-takeover:cloudflare:agent-vault";
    try {
      await f.request();
      const lease = await f.call("browser_vault_request_takeover") as { challenge_id: string };
      expect(lease).toMatchObject({ type: "browser_vault_takeover", status: "input_required" });
      expect(f.stored.has(takeoverKey)).toBe(true);
      expect(f.stored.has(challengeKey)).toBe(false);
      const blocked = [
        ["browser_execute", { code: "1" }],
        ["browser_vault_snapshot", identity],
        ["browser_vault_action", { ...identity, action: "navigate", url: identity.expected_origin + "/account" }],
        ["browser_vault_status", identity],
      ] as const;
      for (const runtime of [f.runtime, await f.create()]) {
        f.send.mockClear();
        f.resolve.mockClear();
        for (const [name, input] of blocked) await expect(f.call(name, input, runtime)).rejects.toThrow();
        expect(f.send).not.toHaveBeenCalled();
        expect(f.resolve).not.toHaveBeenCalled();
      }
      const recreated = await f.create();
      await expect(recreated.submitVaultTakeover({ challenge_id: lease.challenge_id, action: "finish" }, context.signal)).resolves.toEqual({ status: "finished" });
      expect(f.stored.has(takeoverKey)).toBe(false);
      expect(f.stored.has(quarantineKey)).toBe(true);
      await expect(f.call("browser_vault_snapshot", identity, recreated)).resolves.toMatchObject({ title: "Account" });
      await expect(f.call("browser_execute", { code: "1" }, recreated)).rejects.toThrow("isolated");
    } finally { f.connect.mockRestore(); }
  });
  it("does not offer intake without a supported OTP form", async () => {
    const f = await fixture();
    try {
      f.state.otp = false;
      await expect(f.request()).rejects.toThrow("No supported");
      expect(f.stored.has(challengeKey)).toBe(false);
    } finally { f.connect.mockRestore(); }
  });
});
