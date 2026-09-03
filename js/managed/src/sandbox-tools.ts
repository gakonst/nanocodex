import { getSandbox } from "@cloudflare/sandbox";
import type { ToolMap } from "nanocodex";
import {
  EXEC_COMMAND_PARAMETERS,
  EXECUTION_OUTPUT_SCHEMA,
  MACHINE_PREVIEW_PARAMETERS,
  PREVIEW_OUTPUT_SCHEMA,
  WRITE_STDIN_PARAMETERS,
} from "nanocodex-tools/execution-contract";

import { isPrivateEgressHeader } from "./managed-egress";
import type { Sandbox } from "./sandbox-runtime";

const WORKSPACE = "/workspace";
const WORKSPACE_FLUSH_COMMAND = "sync -f /workspace";
const DEFAULT_MAX_OUTPUT_TOKENS = 10_000;
const APPROXIMATE_BYTES_PER_TOKEN = 4;
const OUTPUT_CURSOR_PREFIX = "sandbox-output-cursor:";
const WORKSPACE_MOUNT_PROBE_TIMEOUT_MS = 10_000;
const WORKSPACE_MOUNT_HEALTH_ATTEMPTS = 3;
const PREVIEW_CAPABILITY_TTL_MS = 60 * 60 * 1_000;
const PREVIEW_AAD = new TextEncoder().encode("nanocodex-cloudflare-sandbox-preview-v1");
const PREVIEW_WEBSOCKET_RESPONSE_HEADERS = new Set([
  "connection",
  "sec-websocket-accept",
  "sec-websocket-extensions",
  "sec-websocket-protocol",
  "upgrade",
]);
const PREVIEW_HTTP_RESPONSE_HEADERS_BLOCKED = new Set([
  "clear-site-data",
  "connection",
  "content-security-policy-report-only",
  "host",
  "keep-alive",
  "nel",
  "proxy-authenticate",
  "proxy-authorization",
  "refresh",
  "report-to",
  "set-cookie",
  "set-cookie2",
  "trailer",
  "transfer-encoding",
]);
const PREVIEW_HTML_URL_ATTRIBUTES = [
  "action",
  "cite",
  "data",
  "formaction",
  "href",
  "poster",
  "src",
] as const;
const PREVIEW_HTML_URL_SET_ATTRIBUTES = ["imagesrcset", "srcset"] as const;

let cachedPreviewKey:
  | { secret: string; promise: Promise<CryptoKey> }
  | undefined;

type SandboxPreparation = {
  sessionId: string;
  sandbox: Sandbox;
  promise: Promise<Sandbox>;
};

type SandboxProcessStatus = "starting" | "running" | "completed" | "failed" | "killed" | "error";

type SandboxProcess = {
  id: string;
  status: SandboxProcessStatus;
  exitCode?: number;
  kill(): Promise<void>;
  getStatus(): Promise<SandboxProcessStatus>;
  getLogs(): Promise<{ stdout: string; stderr: string }>;
  waitForExit(timeout?: number): Promise<{ exitCode: number }>;
};

const sandboxPreparations = new WeakMap<
  DurableObjectNamespace<Sandbox>,
  Map<string, SandboxPreparation>
>();

type SandboxToolClient = {
  exec(
    command: string,
    options: { cwd: string; timeout?: number },
  ): Promise<{
    success: boolean;
    exitCode: number;
    stdout: string;
    stderr: string;
    duration: number;
  }>;
  startProcess(
    command: string,
    options: { cwd: string; processId: string; autoCleanup: false },
  ): Promise<SandboxProcess>;
  getProcess(id: string): Promise<SandboxProcess | null>;
  tunnels: {
    get(port: number): Promise<{ url: string }>;
  };
};

type SandboxOutputCursorStorage = {
  delete(key: string): void;
  get(key: string): unknown;
  put(key: string, value: unknown): void;
};

export function cloudflareSandboxTools(
  namespace: DurableObjectNamespace<Sandbox>,
  sessionId: string,
  localBucket = false,
  publicOrigin?: string,
  previewSecret?: string,
  outputCursorStorage?: SandboxOutputCursorStorage,
): ToolMap {
  return createCloudflareSandboxTools(
    () => prepareSandbox(namespace, sessionId, localBucket),
    publicOrigin === undefined || previewSecret === undefined
      ? undefined
      : async (port) => ({
          port,
          url: await cloudflareSandboxPreviewUrl(
            publicOrigin,
            previewSecret,
            sessionId,
            port,
          ),
          persistent: false,
        }),
    outputCursorStorage,
  );
}

export async function destroyCloudflareSandbox(
  namespace: DurableObjectNamespace<Sandbox>,
  sessionId: string,
): Promise<void> {
  const cached = sandboxPreparation(namespace, sessionId);
  const sandbox = cached?.sandbox ?? sandboxHandle(namespace, sessionId);
  if (cached) await cached.promise.catch(() => {});
  try {
    await sandbox.destroy();
  } finally {
    clearSandboxPreparations(namespace, sessionId);
  }
}

export async function deleteCloudflareSandbox(
  namespace: DurableObjectNamespace<Sandbox>,
  bucket: R2Bucket,
  sessionId: string,
): Promise<void> {
  // Destruction unmounts the R2-backed workspace. Purge its objects only after
  // that completes so a final filesystem flush cannot recreate deleted keys.
  await destroyCloudflareSandbox(namespace, sessionId);
  await deleteCloudflareSandboxWorkspace(bucket, sessionId);
}

/** Deletes the persisted filesystem owned by one deleted agent sandbox. */
export async function deleteCloudflareSandboxWorkspace(
  bucket: R2Bucket,
  sessionId: string,
): Promise<void> {
  const prefix = `/sessions/${sessionId}/`;
  while (true) {
    const page = await bucket.list({ prefix, limit: 1_000 });
    const keys = page.objects.map(({ key }) => key);
    if (keys.length === 0) return;
    await bucket.delete(keys);
  }
}

export function createCloudflareSandboxTools(
  createSandbox: () => Promise<SandboxToolClient>,
  createPreview?: (port: number) => Promise<{ port: number; url: string; persistent: boolean }>,
  outputCursorStorage: SandboxOutputCursorStorage = memoryOutputCursorStorage(),
): ToolMap {
  return {
    exec_command: {
      description: "Run a shell command in the retained native Linux sandbox, returning a session when it remains live.",
      parameters: EXEC_COMMAND_PARAMETERS,
      outputSchema: EXECUTION_OUTPUT_SCHEMA,
      handler: async (input, context) => {
        const value = objectInput(input);
        if (value.sandbox_permissions === "require_escalated") {
          throw new Error("Cloudflare Sandbox exec_command does not support privilege escalation");
        }
        if (value.tty === true) throw new Error("Cloudflare Sandbox exec_command does not support TTY sessions");
        if (value.shell !== undefined || value.login !== undefined) {
          throw new Error("Cloudflare Sandbox exec_command does not support shell or login overrides");
        }
        const command = requiredString(value.cmd, "cmd");
        const cwd = workspacePath(optionalString(value.workdir, "workdir") ?? ".");
        const yieldTime = yieldMilliseconds(value.yield_time_ms, 10_000);
        const outputByteLimit = requestedOutputBytes(value.max_output_tokens);
        context?.signal.throwIfAborted();
        const sandbox = await createSandbox();
        context?.signal.throwIfAborted();
        const sessionId = await availableSessionId(sandbox);
        outputCursorStorage.put(`${OUTPUT_CURSOR_PREFIX}${sessionId}`, 0);
        let process: SandboxProcess | undefined;
        try {
          process = await sandbox.startProcess(mergedShellCommand(command), {
            cwd,
            processId: sandboxProcessId(sessionId),
            autoCleanup: false,
          });
          return await observeProcess(
            sandbox,
            process,
            sessionId,
            yieldTime,
            outputByteLimit,
            outputCursorStorage,
            context?.signal,
          );
        } catch (error) {
          if (process === undefined) {
            outputCursorStorage.delete(`${OUTPUT_CURSOR_PREFIX}${sessionId}`);
            try { await flushWorkspace(sandbox); } catch { /* Preserve the execution failure. */ }
          }
          throw error;
        }
      },
    },
    write_stdin: {
      description: "Poll a session returned by exec_command in the retained native Linux sandbox, or send Ctrl-C to terminate it.",
      parameters: WRITE_STDIN_PARAMETERS,
      outputSchema: EXECUTION_OUTPUT_SCHEMA,
      handler: async (input, context) => {
        const value = objectInput(input);
        if (value.chars !== undefined && value.chars !== "" && value.chars !== "\u0003") {
          throw new Error("Cloudflare Sandbox write_stdin supports only polling or Ctrl-C termination; stdin is unavailable");
        }
        const sessionId = requiredSessionId(value.session_id);
        const yieldTime = yieldMilliseconds(value.yield_time_ms, 5_000);
        const outputByteLimit = requestedOutputBytes(value.max_output_tokens);
        context?.signal.throwIfAborted();
        const sandbox = await createSandbox();
        const process = await sandbox.getProcess(sandboxProcessId(sessionId));
        if (process === null) throw new Error(`unknown sandbox session: ${sessionId}`);
        if (value.chars === "\u0003") await process.kill().catch(() => {});
        return observeProcess(
          sandbox,
          process,
          sessionId,
          yieldTime,
          outputByteLimit,
          outputCursorStorage,
          context?.signal,
        );
      },
    },
    preview: {
      description: "Expose an HTTP server from the retained native Linux sandbox.",
      parameters: MACHINE_PREVIEW_PARAMETERS,
      outputSchema: PREVIEW_OUTPUT_SCHEMA,
      handler: async (input) => {
        const value = objectInput(input);
        const port = requiredPort(value.port);
        const prepared = await createSandbox();
        if (createPreview) return createPreview(port);
        return withSandboxRpcResult(
          prepared.tunnels.get(port),
          (tunnel) => ({ port, url: tunnel.url, persistent: false }),
        );
      },
    },
  };
}

export async function cloudflareSandboxPreviewUrl(
  publicOrigin: string,
  previewSecret: string,
  sessionId: string,
  port: number,
): Promise<string> {
  requiredPort(port);
  const origin = new URL(publicOrigin);
  if (!["http:", "https:"].includes(origin.protocol)
    || origin.username
    || origin.password
    || origin.href !== `${origin.origin}/`) {
    throw new Error("public origin must be an HTTP(S) origin");
  }
  const capability = await sealSandboxPreview(previewSecret, sessionId, port);
  return new URL(`/sandbox-preview/${capability}/`, origin).href;
}

export async function openSandboxPreviewCapability(
  previewSecret: string,
  capability: string,
): Promise<{ sessionId: string; port: number }> {
  if (!/^[A-Za-z0-9_-]{64,256}$/.test(capability)) throw new Error("invalid preview capability");
  const sealed = decodeBase64Url(capability);
  if (sealed.byteLength <= 12) throw new Error("invalid preview capability");
  const iv = sealed.subarray(0, 12);
  const ciphertext = sealed.subarray(12);
  let plaintext: ArrayBuffer;
  try {
    plaintext = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv, additionalData: PREVIEW_AAD },
      await previewKey(previewSecret),
      ciphertext,
    );
  } catch {
    throw new Error("invalid preview capability");
  }
  const [sessionId, rawPort, rawExpiresAt, ...extra] = new TextDecoder()
    .decode(plaintext)
    .split("\n");
  let port: number;
  try {
    port = requiredPort(Number(rawPort));
  } catch {
    throw new Error("invalid preview capability");
  }
  const expiresAt = Number(rawExpiresAt);
  if (!sessionId
    || extra.length > 0
    || !Number.isSafeInteger(expiresAt)
    || expiresAt <= Date.now()) {
    throw new Error("invalid preview capability");
  }
  return { sessionId, port };
}

export async function proxyCloudflareSandboxPreview(
  namespace: DurableObjectNamespace<Sandbox>,
  sessionId: string,
  port: number,
  request: Request,
  path: string,
): Promise<Response> {
  requiredPort(port);
  const incoming = new URL(request.url);
  const targetPath = path.startsWith("/") ? path : `/${path}`;
  const capabilityPath = sandboxPreviewCapabilityPath(incoming.pathname, targetPath);
  const target = new URL(`http://sandbox.internal${targetPath}${incoming.search}`);
  const forwarded = new Request(target, request);
  const websocket = request.headers.get("Upgrade")?.toLowerCase() === "websocket";
  for (const name of [...forwarded.headers.keys()]) {
    if (isPrivatePreviewRequestHeader(name, websocket)) forwarded.headers.delete(name);
  }
  const sandbox = sandboxHandle(namespace, sessionId);
  if (websocket) {
    const response = await sandbox.wsConnect(forwarded, port);
    if (response.status !== 101 || !response.webSocket) {
      if (response.status === 101) return new Response("Sandbox WebSocket upgrade failed", { status: 502 });
      return hardenPreviewHttpResponse(response, capabilityPath);
    }
    const responseHeaders = new Headers();
    for (const [name, value] of response.headers) {
      if (PREVIEW_WEBSOCKET_RESPONSE_HEADERS.has(name.toLowerCase())) {
        responseHeaders.append(name, value);
      }
    }
    return new Response(null, {
      status: 101,
      headers: responseHeaders,
      webSocket: response.webSocket,
    });
  }
  return hardenPreviewHttpResponse(await sandbox.containerFetch(forwarded, port), capabilityPath);
}

function hardenPreviewHttpResponse(response: Response, capabilityPath?: string): Response {
  const responseHeaders = new Headers(response.headers);
  for (const name of [...responseHeaders.keys()]) {
    if (PREVIEW_HTTP_RESPONSE_HEADERS_BLOCKED.has(name.toLowerCase())
      || isPrivateEgressHeader(name)) responseHeaders.delete(name);
  }
  responseHeaders.set(
    "content-security-policy",
    "sandbox allow-downloads allow-forms allow-modals allow-pointer-lock allow-popups allow-popups-to-escape-sandbox allow-presentation allow-scripts",
  );
  responseHeaders.set("permissions-policy", "camera=(), geolocation=(), microphone=(), payment=(), usb=()");
  responseHeaders.set("referrer-policy", "no-referrer");
  const location = responseHeaders.get("location");
  if (capabilityPath && location && isRootRelativeUrl(location)) {
    responseHeaders.set("location", scopeRootRelativeUrl(location, capabilityPath));
  }
  const hardened = new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers: responseHeaders,
  });
  if (!capabilityPath || !responseHeaders.get("content-type")?.toLowerCase().startsWith("text/html")) {
    return hardened;
  }
  const browserScope = `<script type="importmap">${JSON.stringify({ imports: { "/": `${capabilityPath}/` } })}</script><script>(()=>{const p=${JSON.stringify(capabilityPath)},W=globalThis.WebSocket;globalThis.WebSocket=new Proxy(W,{construct(t,a,n){try{const u=new URL(a[0],location.href);if((u.protocol==="ws:"||u.protocol==="wss:")&&u.host===location.host&&u.pathname!==p&&!u.pathname.startsWith(p+"/")){u.pathname=p+(u.pathname.startsWith("/")?u.pathname:"/"+u.pathname);a[0]=u.href}}catch{}return Reflect.construct(t,a,n)}})})()</script>`;
  return new HTMLRewriter()
    .on("*", {
      element(element) {
        for (const attribute of PREVIEW_HTML_URL_ATTRIBUTES) {
          const value = element.getAttribute(attribute);
          if (value && isRootRelativeUrl(value)) {
            element.setAttribute(attribute, scopeRootRelativeUrl(value, capabilityPath));
          }
        }
        for (const attribute of PREVIEW_HTML_URL_SET_ATTRIBUTES) {
          const value = element.getAttribute(attribute);
          if (value) {
            element.setAttribute(attribute, value.replace(
              /(^|,\s*)(\/(?!\/)[^\s,]*)/g,
              (_match, separator: string, url: string) => `${separator}${scopeRootRelativeUrl(url, capabilityPath)}`,
            ));
          }
        }
      },
    })
    .on("head", {
      element(element) {
        element.prepend(browserScope, { html: true });
      },
    })
    .transform(hardened);
}

function sandboxPreviewCapabilityPath(incomingPath: string, targetPath: string): string | undefined {
  if (targetPath === "/" && /^\/sandbox-preview\/[^/]+$/.test(incomingPath)) return incomingPath;
  if (!incomingPath.endsWith(targetPath)) return undefined;
  const capabilityPath = incomingPath.slice(0, -targetPath.length);
  return /^\/sandbox-preview\/[^/]+$/.test(capabilityPath) ? capabilityPath : undefined;
}

function isRootRelativeUrl(value: string): boolean {
  return value.startsWith("/") && !value.startsWith("//");
}

function scopeRootRelativeUrl(value: string, capabilityPath: string): string {
  if (value === capabilityPath
    || value.startsWith(`${capabilityPath}/`)
    || value.startsWith(`${capabilityPath}?`)
    || value.startsWith(`${capabilityPath}#`)) return value;
  return `${capabilityPath}${value}`;
}

async function sealSandboxPreview(
  previewSecret: string,
  sessionId: string,
  port: number,
): Promise<string> {
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ciphertext = new Uint8Array(await crypto.subtle.encrypt(
    { name: "AES-GCM", iv, additionalData: PREVIEW_AAD },
    await previewKey(previewSecret),
    new TextEncoder().encode(`${sessionId}\n${port}\n${Date.now() + PREVIEW_CAPABILITY_TTL_MS}`),
  ));
  const sealed = new Uint8Array(iv.byteLength + ciphertext.byteLength);
  sealed.set(iv);
  sealed.set(ciphertext, iv.byteLength);
  return encodeBase64Url(sealed);
}

function previewKey(secret: string): Promise<CryptoKey> {
  if (!secret) throw new Error("preview secret is required");
  if (cachedPreviewKey?.secret === secret) return cachedPreviewKey.promise;
  const promise = derivePreviewKey(secret);
  cachedPreviewKey = { secret, promise };
  void promise.catch(() => {
    if (cachedPreviewKey?.promise === promise) cachedPreviewKey = undefined;
  });
  return promise;
}

async function derivePreviewKey(secret: string): Promise<CryptoKey> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(secret));
  return crypto.subtle.importKey("raw", digest, { name: "AES-GCM" }, false, ["encrypt", "decrypt"]);
}

function isPrivatePreviewRequestHeader(name: string, websocket: boolean): boolean {
  const lower = name.toLowerCase();
  return isPrivateEgressHeader(name)
    || lower === "host"
    || lower === "origin"
    || lower === "proxy-connection"
    || lower === "referer"
    || lower === "te"
    || lower === "trailer"
    || lower === "transfer-encoding"
    || (!websocket && (lower === "connection" || lower === "upgrade"))
    || lower.startsWith("cf-")
    || lower.startsWith("forwarded")
    || lower.startsWith("x-forwarded-")
    || lower.startsWith("x-nanocodex-");
}

function encodeBase64Url(value: Uint8Array): string {
  return btoa(String.fromCharCode(...value))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/, "");
}

function decodeBase64Url(value: string): Uint8Array {
  const standard = value.replaceAll("-", "+").replaceAll("_", "/");
  const padded = standard.padEnd(Math.ceil(standard.length / 4) * 4, "=");
  let decoded: string;
  try {
    decoded = atob(padded);
  } catch {
    throw new Error("invalid preview capability");
  }
  return Uint8Array.from(decoded, (character) => character.charCodeAt(0));
}

async function prepareSandbox(
  namespace: DurableObjectNamespace<Sandbox>,
  sessionId: string,
  localBucket: boolean,
): Promise<Sandbox> {
  let sessions = sandboxPreparations.get(namespace);
  if (!sessions) {
    sessions = new Map();
    sandboxPreparations.set(namespace, sessions);
  }
  const key = `${localBucket ? "local" : "remote"}:${sessionId}`;
  const existing = sessions.get(key);
  if (existing) return existing.promise;

  const sandbox = sandboxHandle(namespace, sessionId);
  const entry: SandboxPreparation = {
    sessionId,
    sandbox,
    promise: Promise.resolve(sandbox),
  };
  const prepared = prepareSandboxWorkspace(sandbox, sessionId, localBucket);
  entry.promise = prepared.then(
    (value) => {
      releaseSandboxPreparation(namespace, key, entry);
      return value;
    },
    (error: unknown) => {
      releaseSandboxPreparation(namespace, key, entry);
      throw error;
    },
  );
  sessions.set(key, entry);
  return entry.promise;
}

async function prepareSandboxWorkspace(
  sandbox: Sandbox,
  sessionId: string,
  localBucket: boolean,
): Promise<Sandbox> {
  if (!localBucket) {
    const state = await workspaceMountState(sandbox);
    if (state === "mounted") return sandbox;
    if (state === "mounted-unhealthy") {
      throw new Error(
        "the existing /workspace mount is unhealthy; refusing to unmount or remount a live workspace",
      );
    }
    if (state === "occupied") {
      throw new Error(
        "the unmounted /workspace directory is not empty; refusing to hide retained workspace files",
      );
    }
  }

  try {
    await sandbox.mountBucket("NANOCODEX_WORKSPACES", WORKSPACE, {
      prefix: `/sessions/${sessionId}/`,
      ...(localBucket ? { localBucket: true as const } : {}),
    });
  } catch (error) {
    if (localBucket && errorMessage(error).toLowerCase().includes("mount path already in use")) {
      return sandbox;
    }
    // Another preparation can win between the preflight probe and the SDK's
    // serialized mount operation. Reuse only a mount that is demonstrably live;
    // never suppress a mount error merely because /workspace contains files.
    if (!localBucket && await workspaceMountState(sandbox) === "mounted") return sandbox;
    throw error;
  }
  if (!localBucket && await workspaceMountState(sandbox) !== "mounted") {
    throw new Error("the R2 workspace mount did not become healthy");
  }
  return sandbox;
}

type WorkspaceMountState = "absent" | "empty" | "occupied" | "mounted" | "mounted-unhealthy";

async function workspaceMountState(sandbox: SandboxToolClient): Promise<WorkspaceMountState> {
  let state: WorkspaceMountState = "mounted-unhealthy";
  for (let attempt = 0; attempt < WORKSPACE_MOUNT_HEALTH_ATTEMPTS; attempt += 1) {
    state = await workspaceMountStateOnce(sandbox);
    if (state !== "mounted-unhealthy") return state;
  }
  return state;
}

async function workspaceMountStateOnce(sandbox: SandboxToolClient): Promise<WorkspaceMountState> {
  const probe = await sandbox.exec(
    "if mountpoint -q /workspace; then "
      + "if stat /workspace >/dev/null; then printf mounted; else printf mounted-unhealthy; fi; "
      + "elif [ ! -e /workspace ]; then printf absent; "
      + "elif [ -d /workspace ] && [ -z \"$(find /workspace -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null)\" ]; then printf empty; "
      + "else printf occupied; fi",
    { cwd: "/", timeout: WORKSPACE_MOUNT_PROBE_TIMEOUT_MS },
  );
  if (!probe.success || probe.exitCode !== 0) {
    throw new Error("could not inspect the existing /workspace mount");
  }
  const state = probe.stdout.trim();
  if (
    state === "absent"
    || state === "empty"
    || state === "occupied"
    || state === "mounted"
    || state === "mounted-unhealthy"
  ) return state;
  throw new Error(`unexpected /workspace mount probe result: ${state || "empty output"}`);
}

function sandboxPreparation(
  namespace: DurableObjectNamespace<Sandbox>,
  sessionId: string,
): SandboxPreparation | undefined {
  return [...(sandboxPreparations.get(namespace)?.values() ?? [])]
    .find((entry) => entry.sessionId === sessionId);
}

function clearSandboxPreparations(
  namespace: DurableObjectNamespace<Sandbox>,
  sessionId: string,
): void {
  const sessions = sandboxPreparations.get(namespace);
  if (!sessions) return;
  for (const [key, entry] of sessions) {
    if (entry.sessionId === sessionId) sessions.delete(key);
  }
  if (sessions.size === 0) sandboxPreparations.delete(namespace);
}

function releaseSandboxPreparation(
  namespace: DurableObjectNamespace<Sandbox>,
  key: string,
  entry: SandboxPreparation,
): void {
  const sessions = sandboxPreparations.get(namespace);
  if (sessions?.get(key) !== entry) return;
  sessions.delete(key);
  if (sessions.size === 0) sandboxPreparations.delete(namespace);
}

function sandboxHandle(
  namespace: DurableObjectNamespace<Sandbox>,
  sessionId: string,
): Sandbox {
  return getSandbox(namespace, `nanocodex-${sessionId}`, {
    normalizeId: true,
    sleepAfter: "10m",
    // Commands are independent tool calls. A command containing `exit` must
    // not terminate a shared SDK shell and poison later calls.
    enableDefaultSession: false,
    transport: "rpc",
    labels: { application: "nanocodex", session: sessionId },
  });
}

export function workspacePath(raw: string): string {
  if (!raw || raw.length > 1024 || raw.includes("\0")) throw new Error("path must be 1-1024 characters");
  let relative = raw;
  if (relative === WORKSPACE) relative = ".";
  else if (relative.startsWith(`${WORKSPACE}/`)) relative = relative.slice(WORKSPACE.length + 1);
  else if (relative.startsWith("/")) throw new Error("path must be relative to /workspace");
  const parts = relative.split("/").filter((part) => part !== "" && part !== ".");
  if (parts.includes("..")) throw new Error("path must not contain '..'");
  return parts.length === 0 ? WORKSPACE : `${WORKSPACE}/${parts.join("/")}`;
}

function objectInput(input: unknown): Record<string, unknown> {
  if (!input || typeof input !== "object" || Array.isArray(input)) throw new Error("tool input must be an object");
  return input as Record<string, unknown>;
}

function requiredString(
  value: unknown,
  name: string,
  maxChars = Number.MAX_SAFE_INTEGER,
): string {
  if (typeof value !== "string" || value.length === 0) throw new Error(`${name} must be a non-empty string`);
  if (value.length > maxChars) throw new Error(`${name} is too long`);
  return value;
}

function optionalString(value: unknown, name: string): string | undefined {
  if (value === undefined) return undefined;
  return requiredString(value, name, 1024);
}

function requiredSessionId(value: unknown): number {
  if (!Number.isSafeInteger(value) || Number(value) < 1) {
    throw new Error("session_id must be a positive safe integer");
  }
  return Number(value);
}

function yieldMilliseconds(value: unknown, defaultValue: number): number {
  if (value === undefined) return defaultValue;
  if (!Number.isSafeInteger(value) || Number(value) < 0) {
    throw new Error("yield_time_ms must be a non-negative safe integer");
  }
  return Number(value);
}

function sandboxProcessId(sessionId: number): string {
  return `nanocodex-${sessionId}`;
}

async function availableSessionId(sandbox: SandboxToolClient): Promise<number> {
  for (let attempt = 0; attempt < 32; attempt += 1) {
    const sessionId = crypto.getRandomValues(new Uint32Array(1))[0]! || 1;
    if (await sandbox.getProcess(sandboxProcessId(sessionId)) === null) return sessionId;
  }
  throw new Error("could not allocate a sandbox command session");
}

async function observeProcess(
  sandbox: SandboxToolClient,
  initial: SandboxProcess,
  sessionId: number,
  yieldTimeMs: number,
  outputByteLimit: number,
  outputCursorStorage: SandboxOutputCursorStorage,
  signal?: AbortSignal,
): Promise<Record<string, unknown>> {
  const startedAt = performance.now();
  let process = initial;
  let refreshed: SandboxProcess | undefined;
  try {
    let status = await process.getStatus();
    if (!isTerminalProcessStatus(status) && yieldTimeMs > 0) {
      await waitForProcessExit(process, yieldTimeMs, signal);
      status = await process.getStatus();
    }
    if (isTerminalProcessStatus(status)) {
      refreshed = await sandbox.getProcess(initial.id) ?? undefined;
      process = refreshed ?? process;
    }
    const logs = await process.getLogs();
    const accumulatedOutput = `${logs.stdout}${logs.stderr}`;
    const cursorKey = `${OUTPUT_CURSOR_PREFIX}${sessionId}`;
    const retainedCursor = outputCursorStorage.get(cursorKey);
    const delivered = Math.min(
      Number.isSafeInteger(retainedCursor) && Number(retainedCursor) >= 0
        ? Number(retainedCursor)
        : 0,
      accumulatedOutput.length,
    );
    const terminal = isTerminalProcessStatus(status);
    const chunk = boundedOutput(accumulatedOutput, delivered, outputByteLimit, terminal);
    const nextCursor = delivered + chunk.consumedCharacters;
    const result: Record<string, unknown> = {
      output: chunk.text,
      chunk_id: `${sessionId}:${nextCursor}`,
      wall_time_seconds: Math.max(0, (performance.now() - startedAt) / 1_000),
    };
    if (chunk.truncated) result.original_token_count = chunk.originalTokenCount;
    if (terminal) {
      await flushWorkspace(sandbox);
      outputCursorStorage.delete(cursorKey);
      if (typeof process.exitCode === "number") result.exit_code = process.exitCode;
    } else {
      outputCursorStorage.put(cursorKey, nextCursor);
      result.session_id = sessionId;
    }
    return result;
  } catch (error) {
    await process.kill().catch(() => {});
    outputCursorStorage.delete(`${OUTPUT_CURSOR_PREFIX}${sessionId}`);
    try { await flushWorkspace(sandbox); } catch { /* Preserve the process observation failure. */ }
    throw error;
  } finally {
    if (refreshed !== undefined && refreshed !== initial) disposeSandboxRpcValue(refreshed);
    disposeSandboxRpcValue(initial);
  }
}

async function waitForProcessExit(
  process: SandboxProcess,
  milliseconds: number,
  signal?: AbortSignal,
): Promise<void> {
  signal?.throwIfAborted();
  let removeAbort = () => {};
  const aborted = new Promise<never>((_resolve, reject) => {
    const abort = () => {
      reject(signal?.reason ?? new Error("sandbox command cancelled"));
    };
    signal?.addEventListener("abort", abort, { once: true });
    removeAbort = () => signal?.removeEventListener("abort", abort);
  });
  try {
    await Promise.race([process.waitForExit(milliseconds), aborted]);
  } catch (error) {
    if (!(error instanceof Error && error.name === "ProcessReadyTimeoutError")) throw error;
  } finally {
    removeAbort();
  }
}

function optionalInteger(
  value: unknown,
  name: string,
  minimum: number,
  maximum: number,
): number | undefined {
  if (value === undefined) return undefined;
  if (!Number.isInteger(value) || (value as number) < minimum || (value as number) > maximum) {
    throw new Error(`${name} must be an integer between ${minimum} and ${maximum}`);
  }
  return value as number;
}

function requiredPort(value: unknown): number {
  const port = optionalPort(value, "port");
  if (port === undefined) throw new Error("port is required");
  if (port === 3_000) throw new Error("port 3000 is reserved for the sandbox control plane");
  return port;
}

function optionalPort(value: unknown, name: string): number | undefined {
  return optionalInteger(value, name, 1024, 65_535);
}

async function withSandboxRpcResult<T, R>(
  result: Promise<T>,
  consume: (value: T) => R | Promise<R>,
): Promise<R> {
  const value = await result;
  try {
    return await consume(value);
  } finally {
    disposeSandboxRpcValue(value);
  }
}

async function flushWorkspace(sandbox: SandboxToolClient): Promise<void> {
  const result = await sandbox.exec(WORKSPACE_FLUSH_COMMAND, { cwd: "/" });
  if (!result.success || result.exitCode !== 0) {
    const detail = result.stderr.trim() || result.stdout.trim();
    throw new Error(`failed to flush the retained workspace${detail ? `: ${detail}` : ""}`);
  }
}

function disposeSandboxRpcValue(value: unknown): void {
  if (typeof value !== "object" || value === null) return;
  const dispose = (value as Partial<Disposable>)[Symbol.dispose];
  if (typeof dispose === "function") dispose.call(value);
}

function boundedOutput(
  accumulated: string,
  deliveredCharacters: number,
  outputByteLimit: number,
  terminal: boolean,
): {
  text: string;
  truncated: boolean;
  consumedCharacters: number;
  originalTokenCount: number;
} {
  const value = accumulated.slice(deliveredCharacters);
  const encoded = new TextEncoder().encode(value);
  const originalTokenCount = Math.ceil(encoded.byteLength / APPROXIMATE_BYTES_PER_TOKEN);
  if (encoded.byteLength <= outputByteLimit) {
    return {
      text: value,
      truncated: false,
      consumedCharacters: value.length,
      originalTokenCount,
    };
  }
  if (terminal) {
    const marker = new TextEncoder().encode("\n… output truncated …\n");
    if (outputByteLimit <= marker.byteLength) {
      return {
        text: decodeUtf8Prefix(encoded, outputByteLimit),
        truncated: true,
        consumedCharacters: value.length,
        originalTokenCount,
      };
    }
    const retainedBytes = outputByteLimit - marker.byteLength;
    const headBytes = Math.ceil(retainedBytes / 2);
    const head = decodeUtf8Prefix(encoded, headBytes);
    const tail = decodeUtf8Suffix(encoded, retainedBytes - new TextEncoder().encode(head).byteLength);
    return {
      text: `${head}\n… output truncated …\n${tail}`,
      truncated: true,
      consumedCharacters: value.length,
      originalTokenCount,
    };
  }
  const text = decodeUtf8Prefix(encoded, outputByteLimit);
  return {
    text,
    truncated: true,
    consumedCharacters: text.length,
    originalTokenCount,
  };
}

function decodeUtf8Prefix(encoded: Uint8Array, limit: number): string {
  let end = Math.min(encoded.byteLength, limit);
  while (end > 0) {
    try {
      return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true })
        .decode(encoded.subarray(0, end));
    } catch {
      end -= 1;
    }
  }
  return "";
}

function decodeUtf8Suffix(encoded: Uint8Array, limit: number): string {
  let start = Math.max(0, encoded.byteLength - limit);
  while (start < encoded.byteLength) {
    try {
      return new TextDecoder("utf-8", { fatal: true, ignoreBOM: true })
        .decode(encoded.subarray(start));
    } catch {
      start += 1;
    }
  }
  return "";
}

function requestedOutputBytes(value: unknown): number {
  if (value !== undefined && (!Number.isSafeInteger(value) || Number(value) < 0)) {
    throw new Error("max_output_tokens must be a non-negative safe integer");
  }
  return Math.min(
    Number.MAX_SAFE_INTEGER,
    Number(value ?? DEFAULT_MAX_OUTPUT_TOKENS) * APPROXIMATE_BYTES_PER_TOKEN,
  );
}

function memoryOutputCursorStorage(): SandboxOutputCursorStorage {
  const cursors = new Map<string, unknown>();
  return {
    delete: (key) => { cursors.delete(key); },
    get: (key) => cursors.get(key),
    put: (key, value) => { cursors.set(key, value); },
  };
}

function mergedShellCommand(command: string): string {
  return `exec 2>&1\n${command}`;
}

function isTerminalProcessStatus(status: SandboxProcessStatus): boolean {
  return status !== "starting" && status !== "running";
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
