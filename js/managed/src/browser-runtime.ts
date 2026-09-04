import { asSchema, type Tool as AiSdkTool, type ToolSet as AiSdkToolSet } from "ai";
import {
  DurableBrowserSessionStore,
  type BrowserBinding,
  type BrowserSessionStore,
  type StoredBrowserSession,
} from "agents/browser";
import {
  createBrowserRuntime,
  type BrowserRuntime,
  type CreateBrowserToolsOptions,
} from "agents/browser/ai";
import type { NamedTool, ToolContext } from "nanocodex";

export type ManagedBrowserProvider = "cloudflare" | "browserbase";

export interface ManagedBrowserEnv {
  BROWSER?: BrowserBinding;
  LOADER?: WorkerLoader;
  MANAGED_BROWSER_PROVIDER?: string;
  MANAGED_BROWSER_KEEP_ALIVE_MS?: string;
  MANAGED_BROWSER_TOOL_TIMEOUT_MS?: string;
  BROWSERBASE_API_KEY?: string;
  BROWSERBASE_PROJECT_ID?: string;
}

export type ManagedBrowserRuntime = Readonly<{
  provider: ManagedBrowserProvider;
  tools: readonly NamedTool[];
  expireAndSweep(): Promise<void>;
  close(): Promise<void>;
}>;

type BrowserRuntimeFactory = (options: CreateBrowserToolsOptions) => BrowserRuntime;
type FetchImplementation = typeof globalThis.fetch;

const BROWSERBASE_API_ORIGIN = "https://api.browserbase.com";
const DEFAULT_KEEP_ALIVE_MS = 10 * 60_000;
const DEFAULT_TOOL_TIMEOUT_MS = 30_000;
const MAX_BROWSERBASE_RESPONSE_BYTES = 256 * 1024;
const MANAGED_BROWSER_EXECUTE_DESCRIPTION = [
  "Run browser automation in the retained managed browser session.",
  "Outer contract (Nanocodex Rust/WASM Code Mode): nested tools exist only on `tools.*`; `cdp` and `codemode` are not globals. Invoke this tool as the final expression: `await tools.browser_execute({ code })`.",
  "Inner contract (`code` only): this is a separate Cloudflare Code Mode sandbox whose only host globals are `cdp` and `codemode` (plus standard JavaScript). There is no `tools` or `text`; make the value to return the final expression.",
  "Inner discovery signatures take strings: `await codemode.search(\"short intent\")`, then `await codemode.describe(\"cdp.method\")`. Search indexes connector methods, not raw Chrome protocol commands; use `await cdp.spec({})` for those. Never guess method names or argument shapes.",
  "Inner `cdp` methods take one object argument, for example `await cdp.send({ method: \"Target.getTargets\" })`, `await cdp.attachToTarget({ targetId })`, and `await cdp.send({ method: \"Page.navigate\", params: { url }, sessionId })`.",
  "This managed surface rejects credential-bearing or unrestricted capabilities, including `Runtime.evaluate` and `Runtime.callFunctionOn`; use allowed Target, Page, and DOM commands instead.",
  "To read a title safely after `Page.navigate`, wait for loading and call `Target.getTargets` again, then select the matching `targetId`; each result contains its URL and title. `Target.getTargetInfo` is not available.",
  "For ordinary public-web search, use `tools.web__run(...)` in the surrounding Nanocodex Code Mode cell.",
].join("\n");
const MODEL_SAFE_CDP_METHODS = new Set([
  "Target.getTargets",
  "Target.createTarget",
  "Target.closeTarget",
  "Target.attachToTarget",
  "Page.enable",
  "Page.navigate",
  "Page.reload",
  "Page.stopLoading",
  "Page.captureScreenshot",
  "Page.getLayoutMetrics",
  "DOM.enable",
  "DOM.getDocument",
  "DOM.querySelector",
  "DOM.querySelectorAll",
  "DOM.getOuterHTML",
  "DOM.getAttributes",
  "DOM.getBoxModel",
  "DOM.focus",
  "DOM.scrollIntoView",
  "Input.dispatchMouseEvent",
  "Input.dispatchKeyEvent",
  "Input.insertText",
]);

const BROWSERBASE_CDP_PROTOCOL = Object.freeze({
  version: { major: "1", minor: "3" },
  domains: [
    {
      domain: "Target",
      description: "Inspect and manage browser targets (tabs).",
      commands: [
        { name: "getTargets", description: "List browser targets." },
        { name: "createTarget", description: "Open a new target at a URL." },
        { name: "closeTarget", description: "Close a target." },
        { name: "attachToTarget", description: "Attach to a target." },
      ],
      events: [],
      types: [],
    },
    {
      domain: "Page",
      description: "Navigate and inspect a page.",
      commands: [
        { name: "enable" },
        { name: "navigate" },
        { name: "captureScreenshot" },
        { name: "getLayoutMetrics" },
      ],
      events: [{ name: "loadEventFired" }],
      types: [],
    },
    {
      domain: "Runtime",
      description: "Evaluate JavaScript and inspect runtime values.",
      commands: [{ name: "enable" }, { name: "evaluate" }, { name: "callFunctionOn" }],
      events: [],
      types: [],
    },
    {
      domain: "DOM",
      description: "Inspect and interact with the document tree.",
      commands: [
        { name: "enable" },
        { name: "getDocument" },
        { name: "querySelector" },
        { name: "getOuterHTML" },
        { name: "focus" },
      ],
      events: [],
      types: [],
    },
    {
      domain: "Input",
      description: "Send ordinary user input to a page.",
      commands: [
        { name: "dispatchMouseEvent" },
        { name: "dispatchKeyEvent" },
        { name: "insertText" },
      ],
      events: [],
      types: [],
    },
  ],
});

class BrowserbaseApiError extends Error {
  constructor(readonly status: number, operation: string) {
    super(`Browserbase ${operation} failed with HTTP ${status}`);
    this.name = "BrowserbaseApiError";
  }
}

type BrowserbaseSession = Readonly<{
  id: string;
  status: "PENDING" | "RUNNING" | "ERROR" | "TIMED_OUT" | "COMPLETED";
  connectUrl?: string;
}>;

/**
 * The Browserbase REST/session boundary. Signed CDP and Live View URLs stay
 * inside this object and are never returned by its public lifecycle methods.
 */
export class BrowserbaseSessionFactory {
  readonly #apiKey: string;
  readonly #projectId?: string;
  readonly #fetch: FetchImplementation;

  constructor(options: Readonly<{
    apiKey: string;
    projectId?: string;
    fetch?: FetchImplementation;
  }>) {
    if (!options.apiKey.trim()) throw new TypeError("Browserbase API key is required");
    this.#apiKey = options.apiKey;
    this.#projectId = options.projectId?.trim() || undefined;
    this.#fetch = options.fetch ?? globalThis.fetch;
  }

  async create(timeoutMs: number): Promise<{ sessionId: string }> {
    const timeoutSeconds = Math.max(60, Math.min(21_600, Math.ceil(timeoutMs / 1_000)));
    const body = {
      ...(this.#projectId === undefined ? {} : { projectId: this.#projectId }),
      keepAlive: true,
      timeout: timeoutSeconds,
      browserSettings: {
        advancedStealth: false,
        solveCaptchas: false,
        verified: false,
        recordSession: false,
      },
    };
    const payload = await this.#request("/v1/sessions", {
      method: "POST",
      body: JSON.stringify(body),
    }, "session creation");
    const session = parseBrowserbaseSession(payload, true);
    return { sessionId: session.id };
  }

  async isAlive(sessionId: string): Promise<boolean> {
    try {
      const session = await this.#retrieve(sessionId);
      return session.status === "PENDING" || session.status === "RUNNING";
    } catch (error) {
      if (error instanceof BrowserbaseApiError && error.status === 404) return false;
      throw error;
    }
  }

  async release(sessionId: string): Promise<void> {
    validateBrowserbaseSessionId(sessionId);
    await this.#request(`/v1/sessions/${encodeURIComponent(sessionId)}`, {
      method: "POST",
      body: JSON.stringify({
        status: "REQUEST_RELEASE",
        ...(this.#projectId === undefined ? {} : { projectId: this.#projectId }),
      }),
    }, "session release");
  }

  async connect(sessionId: string): Promise<Response> {
    const session = await this.#retrieve(sessionId);
    if (session.status !== "PENDING" && session.status !== "RUNNING") {
      throw new BrowserbaseApiError(404, "CDP connection");
    }
    const endpoint = validateBrowserbaseConnectUrl(session.connectUrl);
    endpoint.protocol = "https:";
    return this.#fetch(endpoint, { headers: { Upgrade: "websocket" } });
  }

  async #retrieve(sessionId: string): Promise<BrowserbaseSession> {
    validateBrowserbaseSessionId(sessionId);
    const payload = await this.#request(
      `/v1/sessions/${encodeURIComponent(sessionId)}`,
      { method: "GET" },
      "session lookup",
    );
    const session = parseBrowserbaseSession(payload, false);
    if (session.id !== sessionId) throw new Error("Browserbase returned a mismatched session");
    return session;
  }

  async #request(path: string, init: RequestInit, operation: string): Promise<unknown> {
    const response = await this.#fetch(`${BROWSERBASE_API_ORIGIN}${path}`, {
      ...init,
      redirect: "error",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "X-BB-API-Key": this.#apiKey,
      },
    });
    if (!response.ok) {
      try { await response.body?.cancel(); } catch { /* The status is the complete safe error. */ }
      throw new BrowserbaseApiError(response.status, operation);
    }
    return readBoundedJson(response);
  }
}

/**
 * Presents Browserbase's REST + signed CDP websocket lifecycle as the
 * structural Browser Run binding consumed by the official Agents SDK.
 */
export class BrowserbaseBrowserBinding implements BrowserBinding {
  constructor(
    readonly sessions: BrowserbaseSessionFactory,
    readonly keepAliveMs = DEFAULT_KEEP_ALIVE_MS,
  ) {}

  async fetch(input: RequestInfo | URL, init: RequestInit = {}): Promise<Response> {
    const url = new URL(typeof input === "string" ? input : input instanceof URL ? input : input.url);
    if (url.origin !== "https://localhost") return new Response(null, { status: 404 });
    const method = (init.method ?? (input instanceof Request ? input.method : "GET")).toUpperCase();
    const match = /^\/v1\/devtools\/browser(?:\/([^/]+)(?:\/json\/(list|protocol))?)?$/.exec(url.pathname);
    if (!match) return new Response(null, { status: 404 });
    const sessionId = match[1];
    const metadata = match[2];

    try {
      if (method === "POST" && sessionId === undefined && metadata === undefined) {
        const requested = Number(url.searchParams.get("keep_alive"));
        const timeoutMs = Number.isFinite(requested) && requested > 0 ? requested : this.keepAliveMs;
        const created = await this.sessions.create(timeoutMs);
        return Response.json(created, { status: 201 });
      }
      if (method === "GET" && sessionId !== undefined && metadata === "list") {
        return await this.sessions.isAlive(sessionId)
          ? Response.json([])
          : new Response(null, { status: 404 });
      }
      if (method === "GET" && sessionId !== undefined && metadata === "protocol") {
        return Response.json(BROWSERBASE_CDP_PROTOCOL);
      }
      if (method === "DELETE" && sessionId !== undefined && metadata === undefined) {
        await this.sessions.release(sessionId);
        return new Response(null, { status: 204 });
      }
      if (method === "GET" && sessionId !== undefined && metadata === undefined
        && new Headers(init.headers).get("upgrade")?.toLowerCase() === "websocket") {
        return this.sessions.connect(sessionId);
      }
      return new Response(null, { status: 405 });
    } catch (error) {
      if (error instanceof BrowserbaseApiError) {
        return new Response(null, { status: error.status });
      }
      throw error;
    }
  }
}

/**
 * Keeps credential-bearing CDP commands and responses outside Code Mode's
 * durable execution log. The Agents SDK remains the session/runtime owner;
 * this binding is only a narrow policy proxy around its WebSocket.
 */
export class CredentialSafeBrowserBinding implements BrowserBinding {
  constructor(
    readonly browser: BrowserBinding,
    readonly secrets: readonly string[] = [],
  ) {}

  async fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
    const response = await this.browser.fetch(input, init);
    if (response.webSocket) return credentialSafeWebSocketResponse(response, this.secrets);
    const requestUrl = new URL(
      typeof input === "string" ? input : input instanceof URL ? input : input.url,
    );
    if (requestUrl.pathname.endsWith("/json/protocol")) return response;
    const contentType = response.headers.get("content-type")?.toLowerCase() ?? "";
    if (!contentType.includes("json") || response.body === null) return response;
    const value = await readBoundedJson(response);
    return Response.json(sanitizeBrowserToolResult(value, this.secrets), {
      status: response.status,
      headers: safeResponseHeaders(response.headers),
    });
  }
}

export function browserCdpMethodAllowed(method: string): boolean {
  return MODEL_SAFE_CDP_METHODS.has(method);
}

function browserCdpCommandAllowed(method: string, params: unknown): boolean {
  if (!browserCdpMethodAllowed(method)) return false;
  if (method !== "Page.navigate" && method !== "Target.createTarget") return true;
  if (!params || typeof params !== "object" || Array.isArray(params)) return false;
  const url = (params as Record<string, unknown>).url;
  if (typeof url !== "string") return false;
  try {
    const parsed = new URL(url);
    return parsed.protocol === "https:" || parsed.protocol === "http:";
  } catch {
    return false;
  }
}

export function managedBrowserProvider(value: string | undefined): ManagedBrowserProvider {
  const provider = value?.trim().toLowerCase() || "cloudflare";
  if (provider === "cloudflare" || provider === "browserbase") return provider;
  throw new TypeError("MANAGED_BROWSER_PROVIDER must be cloudflare or browserbase");
}

export async function createManagedBrowserRuntime(
  options: Readonly<{
    ctx: DurableObjectState;
    env: ManagedBrowserEnv;
    sessionId: string;
    createRuntime?: BrowserRuntimeFactory;
    fetch?: FetchImplementation;
  }>,
): Promise<ManagedBrowserRuntime> {
  const provider = managedBrowserProvider(options.env.MANAGED_BROWSER_PROVIDER);
  const loader = options.env.LOADER;
  if (!loader) throw new Error("Managed browser runtime requires the LOADER binding");
  const keepAliveMs = boundedInteger(
    options.env.MANAGED_BROWSER_KEEP_ALIVE_MS,
    DEFAULT_KEEP_ALIVE_MS,
    60_000,
    21_600_000,
    "MANAGED_BROWSER_KEEP_ALIVE_MS",
  );
  const timeout = boundedInteger(
    options.env.MANAGED_BROWSER_TOOL_TIMEOUT_MS,
    DEFAULT_TOOL_TIMEOUT_MS,
    1_000,
    120_000,
    "MANAGED_BROWSER_TOOL_TIMEOUT_MS",
  );
  let browser: BrowserBinding;
  let secret: string | undefined;
  if (provider === "cloudflare") {
    if (!options.env.BROWSER) {
      throw new Error("Cloudflare browser provider requires the BROWSER binding");
    }
    browser = options.env.BROWSER;
  } else {
    secret = options.env.BROWSERBASE_API_KEY;
    if (!secret) throw new Error("Browserbase provider requires the BROWSERBASE_API_KEY secret");
    browser = new BrowserbaseBrowserBinding(new BrowserbaseSessionFactory({
      apiKey: secret,
      projectId: options.env.BROWSERBASE_PROJECT_ID,
      fetch: options.fetch,
    }), keepAliveMs);
  }
  browser = new CredentialSafeBrowserBinding(browser, secret ? [secret] : []);
  const baseStore = new DurableBrowserSessionStore(options.ctx.storage);
  const store = new ScopedBrowserSessionStore(baseStore, `${provider}:${options.sessionId}:`);
  const runtime = (options.createRuntime ?? createBrowserRuntime)({
    ctx: options.ctx,
    browser,
    loader,
    store,
    session: { mode: "reuse", key: "primary", keepAliveMs },
    quickActions: false,
    timeout,
    name: `managed-browser-${provider}`,
  });
  const tools = await adaptAiSdkTools(runtime.tools, { secrets: secret ? [secret] : [] });
  return Object.freeze({
    provider,
    tools,
    async expireAndSweep() {
      await runtime.runtime.expirePaused();
      await runtime.connector.sweep({ maxIdleMs: keepAliveMs });
    },
    async close() {
      await runtime.connector.closeSession();
    },
  });
}

export async function adaptAiSdkTools(
  tools: AiSdkToolSet,
  options: Readonly<{ secrets?: readonly string[] }> = {},
): Promise<readonly NamedTool[]> {
  return Promise.all(Object.entries(tools).map(async ([name, tool]) => {
    if (typeof tool.execute !== "function") {
      throw new TypeError(`AI SDK browser tool ${name} is not executable`);
    }
    const parameters = await asSchema(tool.inputSchema).jsonSchema;
    if (!parameters || typeof parameters !== "object" || Array.isArray(parameters)) {
      throw new TypeError(`AI SDK browser tool ${name} has a non-object input schema`);
    }
    const description = name === "browser_execute"
      ? MANAGED_BROWSER_EXECUTE_DESCRIPTION
      : typeof tool.description === "string"
        ? tool.description
        : "Use the managed browser runtime.";
    return Object.freeze({
      name,
      description,
      supportsParallelToolCalls: false,
      parameters: parameters as Record<string, unknown>,
      handler: (input: unknown, context: ToolContext) => executeAiSdkTool(
        name,
        tool,
        input,
        context,
        options.secrets ?? [],
      ),
    } satisfies NamedTool);
  }));
}

async function executeAiSdkTool(
  name: string,
  tool: AiSdkTool,
  input: unknown,
  context: ToolContext,
  secrets: readonly string[],
): Promise<unknown> {
  try {
    if (name === "browser_execute" && !browserToolInputAllowed(input)) {
      throw new Error("Browser code requested a credential-bearing or unrestricted runtime capability");
    }
    const execution = tool.execute!(input, {
      toolCallId: context.callId,
      messages: [],
      abortSignal: context.signal,
      context: {},
    });
    const output = isAsyncIterable(execution)
      ? await collectAsyncIterable(execution)
      : await execution;
    const modelOutput = tool.toModelOutput
      ? await tool.toModelOutput({ toolCallId: context.callId, input, output })
      : output;
    return unwrapAiSdkModelOutput(sanitizeBrowserToolResult(modelOutput, secrets));
  } catch (error) {
    throw new Error(sanitizeBrowserError(error, secrets));
  }
}

export function browserToolInputAllowed(input: unknown): boolean {
  if (!input || typeof input !== "object" || Array.isArray(input)) return false;
  const code = (input as Record<string, unknown>).code;
  if (typeof code !== "string") return false;
  return !/(?:cookie|authorization|credential|password|token|secret|live\s*view|getLiveViewUrl|connectUrl|webSocketDebuggerUrl|Runtime\.evaluate|Runtime\.callFunctionOn|setExtraHTTPHeaders)/iu.test(code);
}

export function sanitizeBrowserToolResult(value: unknown, secrets: readonly string[] = []): unknown {
  return sanitizeValue(value, secrets, undefined, new WeakSet<object>(), 0);
}

function sanitizeValue(
  value: unknown,
  secrets: readonly string[],
  key: string | undefined,
  seen: WeakSet<object>,
  depth: number,
): unknown {
  if (sensitiveKey(key)) return "[redacted]";
  if (typeof value === "string") return sanitizeString(value, secrets);
  if (value === null || typeof value !== "object") return value;
  if (depth >= 24) return "[truncated]";
  if (seen.has(value)) return "[circular]";
  seen.add(value);
  if (Array.isArray(value)) {
    return value.map((entry) => sanitizeValue(entry, secrets, key, seen, depth + 1));
  }
  return Object.fromEntries(Object.entries(value).map(([entryKey, entry]) => [
    entryKey,
    sanitizeValue(entry, secrets, entryKey, seen, depth + 1),
  ]));
}

function sensitiveKey(key: string | undefined): boolean {
  if (key === undefined) return false;
  const normalized = key.toLowerCase().replaceAll(/[^a-z0-9]/g, "");
  return normalized.includes("cookie")
    || normalized.includes("authorization")
    || normalized.includes("credential")
    || normalized.includes("password")
    || normalized.includes("apikey")
    || normalized.includes("accesstoken")
    || normalized.includes("refreshtoken")
    || normalized === "token"
    || normalized.includes("secret")
    || normalized.includes("connecturl")
    || normalized.includes("websocketdebuggerurl")
    || normalized.includes("signingkey")
    || normalized.includes("liveview");
}

function sanitizeString(value: string, secrets: readonly string[]): string {
  let sanitized = value;
  for (const secret of secrets) {
    if (secret) sanitized = sanitized.replaceAll(secret, "[redacted]");
  }
  sanitized = sanitized.replaceAll(/(?:https?|wss?):\/\/[^\s"'<>]+/giu, (candidate) => {
    try {
      const hostname = new URL(candidate).hostname.toLowerCase();
      return hostname === "browserbase.com"
        || hostname.endsWith(".browserbase.com")
        || hostname === "browser.run"
        || hostname.endsWith(".browser.run")
        ? "[redacted provider URL]"
        : candidate;
    } catch {
      return "[redacted malformed URL]";
    }
  });
  if (/\b(?:set-cookie|document\.cookie|cookie)\s*:/iu.test(sanitized)
    || /(?:^|;\s*)(?:session|sid|token|auth)[a-z0-9_-]*=[^;\s]+/iu.test(sanitized)) {
    return "[redacted cookie material]";
  }
  return sanitized;
}

function sanitizeBrowserError(error: unknown, secrets: readonly string[]): string {
  const message = error instanceof Error ? error.message : "Managed browser tool failed";
  return sanitizeString(message, secrets);
}

function credentialSafeWebSocketResponse(
  response: Response,
  secrets: readonly string[],
): Response {
  const upstream = response.webSocket;
  if (!upstream) return response;
  const pair = new WebSocketPair();
  const [client, server] = Object.values(pair);
  server.accept();
  upstream.accept();
  server.addEventListener("message", (event) => {
    if (typeof event.data !== "string") {
      server.close(1003, "CDP text frames are required");
      return;
    }
    let command: unknown;
    try { command = JSON.parse(event.data) as unknown; } catch {
      server.close(1007, "Invalid CDP message");
      return;
    }
    const record = command && typeof command === "object" && !Array.isArray(command)
      ? command as Record<string, unknown>
      : undefined;
    if (!record || typeof record.id !== "number" || typeof record.method !== "string") {
      server.close(1008, "Invalid CDP command");
      return;
    }
    if (!browserCdpCommandAllowed(record.method, record.params)) {
      server.send(JSON.stringify({
        id: record.id,
        error: { code: -32_000, message: "CDP method blocked by browser credential policy" },
      }));
      return;
    }
    upstream.send(event.data);
  });
  upstream.addEventListener("message", (event) => {
    if (typeof event.data !== "string") {
      server.close(1003, "CDP text frames are required");
      return;
    }
    try {
      const value = JSON.parse(event.data) as unknown;
      server.send(JSON.stringify(sanitizeBrowserToolResult(value, secrets)));
    } catch {
      server.close(1007, "Invalid CDP response");
    }
  });
  server.addEventListener("close", () => {
    try { upstream.close(1000, "CDP client closed"); } catch { /* Already closed. */ }
  });
  upstream.addEventListener("close", (event) => {
    try { server.close(event.code, event.reason); } catch { /* Already closed. */ }
  });
  server.addEventListener("error", () => {
    try { upstream.close(1011, "CDP proxy failed"); } catch { /* Already closed. */ }
  });
  upstream.addEventListener("error", () => {
    try { server.close(1011, "CDP upstream failed"); } catch { /* Already closed. */ }
  });
  return new Response(null, {
    status: 101,
    headers: safeResponseHeaders(response.headers),
    webSocket: client,
  });
}

function safeResponseHeaders(headers: Headers): Headers {
  const safe = new Headers();
  for (const name of ["content-type", "cf-browser-session-id"]) {
    const value = headers.get(name);
    if (value !== null) safe.set(name, value);
  }
  return safe;
}

function unwrapAiSdkModelOutput(value: unknown): unknown {
  if (!value || typeof value !== "object" || Array.isArray(value)) return value;
  const output = value as Record<string, unknown>;
  if ((output.type === "json" || output.type === "text") && "value" in output) {
    return output.value;
  }
  return value;
}

class ScopedBrowserSessionStore implements BrowserSessionStore {
  constructor(
    readonly base: BrowserSessionStore,
    readonly prefix: string,
  ) {}

  acquireLock(key: string) { return this.base.acquireLock(this.prefix + key); }
  get(key: string) { return this.base.get(this.prefix + key); }
  set(key: string, session: StoredBrowserSession) {
    return this.base.set(this.prefix + key, session);
  }
  delete(key: string) { return this.base.delete(this.prefix + key); }
  async list(prefix: string): Promise<Map<string, StoredBrowserSession>> {
    if (!this.base.list) return new Map();
    const entries = await this.base.list(this.prefix + prefix);
    return new Map([...entries].map(([key, value]) => [key.slice(this.prefix.length), value]));
  }
}

async function readBoundedJson(response: Response): Promise<unknown> {
  const advertised = Number(response.headers.get("content-length"));
  if (Number.isFinite(advertised) && advertised > MAX_BROWSERBASE_RESPONSE_BYTES) {
    try { await response.body?.cancel(); } catch { /* Ignore cleanup failure. */ }
    throw new Error("Browserbase response exceeded the size limit");
  }
  const text = await readBoundedResponseText(response, MAX_BROWSERBASE_RESPONSE_BYTES);
  try {
    return JSON.parse(text) as unknown;
  } catch {
    throw new Error("Browserbase returned invalid JSON");
  }
}

async function readBoundedResponseText(response: Response, limit: number): Promise<string> {
  if (response.body === null) return "";
  const reader = response.body.getReader();
  const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false });
  const parts: string[] = [];
  let bytes = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      bytes += value.byteLength;
      if (bytes > limit) {
        await reader.cancel("response size limit exceeded");
        throw new Error("Browserbase response exceeded the size limit");
      }
      parts.push(decoder.decode(value, { stream: true }));
    }
    parts.push(decoder.decode());
    return parts.join("");
  } catch (error) {
    if (error instanceof Error && error.message === "Browserbase response exceeded the size limit") {
      throw error;
    }
    throw new Error("Browserbase returned invalid UTF-8");
  } finally {
    reader.releaseLock();
  }
}

function parseBrowserbaseSession(value: unknown, requireConnectUrl: boolean): BrowserbaseSession {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Browserbase returned an invalid session");
  }
  const session = value as Record<string, unknown>;
  const id = typeof session.id === "string" ? session.id : "";
  validateBrowserbaseSessionId(id);
  const statuses = new Set(["PENDING", "RUNNING", "ERROR", "TIMED_OUT", "COMPLETED"]);
  if (typeof session.status !== "string" || !statuses.has(session.status)) {
    throw new Error("Browserbase returned an invalid session status");
  }
  const connectUrl = typeof session.connectUrl === "string" ? session.connectUrl : undefined;
  if (requireConnectUrl) validateBrowserbaseConnectUrl(connectUrl);
  return { id, status: session.status as BrowserbaseSession["status"], connectUrl };
}

function validateBrowserbaseSessionId(value: string): void {
  if (!/^[A-Za-z0-9_-]{1,128}$/.test(value)) {
    throw new TypeError("Browserbase returned an invalid session identifier");
  }
}

function validateBrowserbaseConnectUrl(value: string | undefined): URL {
  if (!value) throw new Error("Browserbase did not return a CDP connection URL");
  let url: URL;
  try { url = new URL(value); } catch { throw new Error("Browserbase returned an invalid CDP URL"); }
  const host = url.hostname.toLowerCase();
  if (url.protocol !== "wss:"
    || (host !== "browserbase.com" && !host.endsWith(".browserbase.com"))) {
    throw new Error("Browserbase returned an untrusted CDP URL");
  }
  return url;
}

function boundedInteger(
  raw: string | undefined,
  fallback: number,
  minimum: number,
  maximum: number,
  name: string,
): number {
  if (raw === undefined || raw.trim() === "") return fallback;
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new TypeError(`${name} must be an integer from ${minimum} through ${maximum}`);
  }
  return value;
}

function isAsyncIterable(value: unknown): value is AsyncIterable<unknown> {
  return value !== null
    && typeof value === "object"
    && Symbol.asyncIterator in value
    && typeof (value as AsyncIterable<unknown>)[Symbol.asyncIterator] === "function";
}

async function collectAsyncIterable(iterable: AsyncIterable<unknown>): Promise<unknown> {
  const values: unknown[] = [];
  for await (const value of iterable) values.push(value);
  return values.length === 1 ? values[0] : values;
}
