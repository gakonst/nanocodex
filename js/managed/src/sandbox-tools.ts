import { getSandbox } from "@cloudflare/sandbox";
import type { ToolMap } from "nanocodex";

import { isPrivateEgressHeader } from "./managed-egress";
import type { Sandbox } from "./sandbox-runtime";

const WORKSPACE = "/workspace";
const WORKSPACE_FLUSH_COMMAND = "sync -f /workspace";
const MAX_COMMAND_CHARS = 32 * 1024;
const MAX_FILE_BYTES = 1024 * 1024;
const MAX_OUTPUT_BYTES = 128 * 1024;
const MAX_LIST_ENTRIES = 512;
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

type SandboxProcessStatus =
  | "starting"
  | "running"
  | "completed"
  | "failed"
  | "killed"
  | "error";

type SandboxProcess = {
  id: string;
  pid?: number;
  command: string;
  status: SandboxProcessStatus;
  exitCode?: number;
  kill(): Promise<void>;
  getStatus(): Promise<SandboxProcessStatus>;
  getLogs(): Promise<{ stdout: string; stderr: string }>;
  waitForPort(port: number, options?: { timeout?: number }): Promise<void>;
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
    options: { cwd: string; autoCleanup: false },
  ): Promise<SandboxProcess>;
  getProcess(id: string): Promise<SandboxProcess | null>;
  readFile(
    path: string,
    options: { encoding: "none" },
  ): Promise<{ size: number; content: ReadableStream<Uint8Array> }>;
  writeFile(
    path: string,
    content: string,
    options: { encoding: "utf-8" },
  ): Promise<unknown>;
  listFiles(
    path: string,
    options: { includeHidden: true },
  ): Promise<{
    files: Array<{ name: string; type: string; size: number }>;
  }>;
  tunnels: {
    get(port: number): Promise<{ url: string }>;
  };
};

export function cloudflareSandboxTools(
  namespace: DurableObjectNamespace<Sandbox>,
  sessionId: string,
  localBucket = false,
  publicOrigin?: string,
  previewSecret?: string,
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
): ToolMap {
  return {
    sandbox_exec: {
      description: "Run a foreground shell command in this session's isolated Cloudflare Sandbox workspace.",
      parameters: {
        type: "object",
        properties: {
          command: { type: "string", description: "Shell command to run." },
          cwd: { type: "string", description: "Workspace-relative working directory." },
          timeout_ms: {
            type: "integer",
            minimum: 1,
            description: "Optional Sandbox SDK execution timeout in milliseconds.",
          },
        },
        required: ["command"],
        additionalProperties: false,
      },
      handler: async (input) => {
        const value = objectInput(input);
        const command = requiredString(value.command, "command", MAX_COMMAND_CHARS);
        const cwd = workspacePath(optionalString(value.cwd, "cwd") ?? ".");
        const timeout = optionalPositiveInteger(value.timeout_ms, "timeout_ms");
        const sandbox = await createSandbox();
        return withSandboxRpcResult(
          sandbox.exec(command, {
            cwd,
            ...(timeout === undefined ? {} : { timeout }),
          }).catch(async (error: unknown) => {
            try { await flushWorkspace(sandbox); } catch { /* Preserve the execution failure. */ }
            throw error;
          }),
          async (result) => {
            await flushWorkspace(sandbox);
            return {
              success: result.success,
              exit_code: result.exitCode,
              ...boundedOutput(result),
              duration_ms: result.duration,
            };
          },
        );
      },
    },
    sandbox_read_file: {
      description: "Read a UTF-8 text file from this session's isolated workspace (maximum 1 MiB).",
      parameters: pathParameters(),
      handler: async (input) => {
        const path = workspacePath(requiredString(objectInput(input).path, "path", 1024));
        return withSandboxRpcResult(
          (await createSandbox()).readFile(path, { encoding: "none" }),
          async (result) => {
            if (result.size > MAX_FILE_BYTES) {
              await cancelReadableStream(result.content, "file exceeds 1 MiB");
              throw new Error("file exceeds 1 MiB");
            }
            return { path, content: await readBounded(result.content) };
          },
        );
      },
    },
    sandbox_start_process: {
      description: "Start a managed background process in this session's Cloudflare Sandbox, optionally waiting for an HTTP port to become ready.",
      parameters: {
        type: "object",
        properties: {
          command: { type: "string", description: "Command to start." },
          cwd: { type: "string", description: "Workspace-relative working directory." },
          ready_port: { type: "integer", minimum: 1024, maximum: 65_535 },
          ready_timeout_ms: {
            type: "integer",
            minimum: 1,
            description: "Optional Sandbox SDK port-readiness timeout in milliseconds.",
          },
        },
        required: ["command"],
        additionalProperties: false,
      },
      handler: async (input) => {
        const value = objectInput(input);
        const command = requiredString(value.command, "command", MAX_COMMAND_CHARS);
        const cwd = workspacePath(optionalString(value.cwd, "cwd") ?? ".");
        const readyPort = optionalPort(value.ready_port, "ready_port");
        const readyTimeout = optionalPositiveInteger(
          value.ready_timeout_ms,
          "ready_timeout_ms",
        );
        const sandbox = await createSandbox();
        return withSandboxRpcResult(
          sandbox.startProcess(command, {
            cwd,
            autoCleanup: false,
          }),
          async (process) => {
            if (readyPort !== undefined) {
              try {
                await process.waitForPort(
                  readyPort,
                  readyTimeout === undefined ? undefined : { timeout: readyTimeout },
                );
              } catch (error) {
                let status = await process.getStatus().catch(() => process.status);
                let killError: string | undefined;
                if (!isTerminalProcessStatus(status)) {
                  try {
                    await process.kill();
                    status = await process.getStatus().catch(() => status);
                  } catch (killFailure) {
                    killError = errorMessage(killFailure);
                  }
                }
                let flushError: string | undefined;
                try {
                  await flushWorkspace(sandbox);
                } catch (flushFailure) {
                  flushError = errorMessage(flushFailure);
                }
                return {
                  process_id: process.id,
                  pid: process.pid,
                  command,
                  status,
                  terminal: isTerminalProcessStatus(status),
                  ready_port: readyPort,
                  ready: false,
                  ready_error: errorMessage(error),
                  ...(killError === undefined ? {} : { kill_error: killError }),
                  ...(flushError === undefined ? {} : { flush_error: flushError }),
                };
              }
            }
            const status = await process.getStatus();
            if (isTerminalProcessStatus(status)) await flushWorkspace(sandbox);
            return {
              process_id: process.id,
              pid: process.pid,
              command,
              status,
              ...(readyPort === undefined ? {} : { ready_port: readyPort, ready: true }),
            };
          },
        );
      },
    },
    sandbox_get_process: {
      description: "Get authoritative status for a background process in this session's Cloudflare Sandbox. Terminal results include accumulated output; set include_output only when partial running output is needed.",
      parameters: processIdParameters(true),
      handler: async (input) => {
        const value = objectInput(input);
        const processId = requiredProcessId(value.process_id);
        const sandbox = await createSandbox();
        return withSandboxRpcResult(
          sandbox.getProcess(processId),
          async (process) => {
            if (process === null) return { found: false, process_id: processId };
            const terminal = isTerminalProcessStatus(process.status);
            let output;
            if (terminal || value.include_output === true) {
              const logs = process.getLogs();
              output = boundedOutput(await (terminal
                ? Promise.all([logs, flushWorkspace(sandbox)]).then(([result]) => result)
                : logs));
            }
            return {
              found: true,
              process_id: process.id,
              pid: process.pid,
              command: process.command,
              status: process.status,
              terminal,
              exit_code: process.exitCode ?? null,
              ...output,
            };
          },
        );
      },
    },
    sandbox_kill_process: {
      description: "Terminate a running background process in this session's Cloudflare Sandbox.",
      parameters: processIdParameters(),
      handler: async (input) => {
        const processId = requiredProcessId(objectInput(input).process_id);
        const sandbox = await createSandbox();
        return withSandboxRpcResult(
          sandbox.getProcess(processId),
          async (process) => {
            if (process === null) return { found: false, process_id: processId };
            let status = process.status;
            const killRequested = !isTerminalProcessStatus(status);
            if (killRequested) {
              await process.kill();
              status = await process.getStatus();
            }
            await flushWorkspace(sandbox);
            return {
              found: true,
              process_id: process.id,
              status,
              terminal: isTerminalProcessStatus(status),
              kill_requested: killRequested,
            };
          },
        );
      },
    },
    sandbox_write_file: {
      description: "Write a UTF-8 text file inside this session's isolated workspace (maximum 1 MiB).",
      parameters: {
        type: "object",
        properties: {
          path: { type: "string", description: "Workspace-relative file path." },
          content: { type: "string", description: "Complete UTF-8 file content." },
        },
        required: ["path", "content"],
        additionalProperties: false,
      },
      handler: async (input) => {
        const value = objectInput(input);
        const path = workspacePath(requiredString(value.path, "path", 1024));
        const content = requiredContent(value.content);
        const bytes = new TextEncoder().encode(content).byteLength;
        if (bytes > MAX_FILE_BYTES) throw new Error("content exceeds 1 MiB");
        const sandbox = await createSandbox();
        await sandbox.writeFile(path, content, { encoding: "utf-8" });
        await flushWorkspace(sandbox);
        return { path, bytes_written: bytes };
      },
    },
    sandbox_list_files: {
      description: "List files in a directory inside this session's isolated workspace.",
      parameters: {
        type: "object",
        properties: {
          path: { type: "string", description: "Workspace-relative directory; defaults to the workspace root." },
        },
        additionalProperties: false,
      },
      handler: async (input) => {
        const value = objectInput(input);
        const path = workspacePath(optionalString(value.path, "path") ?? ".");
        return withSandboxRpcResult(
          (await createSandbox()).listFiles(path, { includeHidden: true }),
          (result) => ({
            path,
            entries: result.files.slice(0, MAX_LIST_ENTRIES).map((entry) => ({
              name: entry.name,
              type: entry.type,
              size: entry.size,
            })),
            truncated: result.files.length > MAX_LIST_ENTRIES,
          }),
        );
      },
    },
    sandbox_preview: {
      // Keep this definition byte-stable so snapshots created before the
      // Worker-fronted preview implementation can resume safely.
      description: "Expose a server running in the sandbox through a temporary public Cloudflare Tunnel URL.",
      parameters: portParameters(),
      handler: async (input) => {
        const port = requiredPort(objectInput(input).port);
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
  const port = Number(rawPort);
  const expiresAt = Number(rawExpiresAt);
  if (!sessionId
    || extra.length > 0
    || !Number.isInteger(port)
    || port < 1024
    || port > 65_535
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

function requiredString(value: unknown, name: string, maxChars: number): string {
  if (typeof value !== "string" || value.length === 0) throw new Error(`${name} must be a non-empty string`);
  if (value.length > maxChars) throw new Error(`${name} is too long`);
  return value;
}

function optionalString(value: unknown, name: string): string | undefined {
  if (value === undefined) return undefined;
  return requiredString(value, name, 1024);
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

function optionalPositiveInteger(value: unknown, name: string): number | undefined {
  if (value === undefined) return undefined;
  if (!Number.isSafeInteger(value) || Number(value) < 1) {
    throw new Error(`${name} must be a positive integer`);
  }
  return Number(value);
}

function requiredPort(value: unknown): number {
  const port = optionalPort(value, "port");
  if (port === undefined) throw new Error("port is required");
  return port;
}

function optionalPort(value: unknown, name: string): number | undefined {
  return optionalInteger(value, name, 1024, 65_535);
}

function pathParameters(): Record<string, unknown> {
  return {
    type: "object",
    properties: { path: { type: "string", description: "Workspace-relative file path." } },
    required: ["path"],
    additionalProperties: false,
  };
}

function portParameters(): Record<string, unknown> {
  return {
    type: "object",
    properties: { port: { type: "integer", minimum: 1024, maximum: 65_535 } },
    required: ["port"],
    additionalProperties: false,
  };
}

function processIdParameters(includeOutput = false): Record<string, unknown> {
  return {
    type: "object",
    properties: {
      process_id: {
        type: "string",
        description: "Process ID returned by sandbox_start_process.",
      },
      ...(includeOutput ? {
        include_output: {
          type: "boolean",
          description: "Include accumulated output before the process reaches a terminal state.",
        },
      } : {}),
    },
    required: ["process_id"],
    additionalProperties: false,
  };
}

async function readBounded(stream: ReadableStream<Uint8Array>): Promise<string> {
  const reader = stream.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  let completed = false;
  let cancelled = false;
  try {
    while (true) {
      const next = await reader.read();
      if (next.done) {
        completed = true;
        break;
      }
      size += next.value.byteLength;
      if (size > MAX_FILE_BYTES) {
        cancelled = true;
        await cancelReader(reader, "file exceeds 1 MiB");
        throw new Error("file exceeds 1 MiB");
      }
      chunks.push(next.value);
    }
  } catch (error) {
    if (!completed && !cancelled) await cancelReader(reader, error);
    throw error;
  } finally {
    reader.releaseLock();
  }
  const content = new Uint8Array(size);
  let offset = 0;
  for (const chunk of chunks) {
    content.set(chunk, offset);
    offset += chunk.byteLength;
  }
  try {
    return new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(content);
  } catch {
    throw new Error("file is not valid UTF-8");
  }
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

async function cancelReadableStream(stream: ReadableStream<Uint8Array>, reason: unknown): Promise<void> {
  try {
    await stream.cancel(reason);
  } catch {
    // Preserve the result or decoding failure when cancellation also fails.
  }
}

async function cancelReader(reader: ReadableStreamDefaultReader<Uint8Array>, reason: unknown): Promise<void> {
  try {
    await reader.cancel(reason);
  } catch {
    // Preserve the result or decoding failure when cancellation also fails.
  }
}

function truncate(value: string): { text: string; truncated: boolean } {
  const encoded = new TextEncoder().encode(value);
  if (encoded.byteLength <= MAX_OUTPUT_BYTES) return { text: value, truncated: false };
  let end = MAX_OUTPUT_BYTES;
  while (end > 0) {
    try {
      return {
        text: new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(encoded.subarray(0, end)),
        truncated: true,
      };
    } catch {
      end -= 1;
    }
  }
  return { text: "", truncated: true };
}

function boundedOutput(output: { stdout: string; stderr: string }): {
  stdout: string;
  stderr: string;
  stdout_truncated: boolean;
  stderr_truncated: boolean;
} {
  const stdout = truncate(output.stdout);
  const stderr = truncate(output.stderr);
  return {
    stdout: stdout.text,
    stderr: stderr.text,
    stdout_truncated: stdout.truncated,
    stderr_truncated: stderr.truncated,
  };
}

function isTerminalProcessStatus(status: SandboxProcessStatus): boolean {
  return status !== "starting" && status !== "running";
}

function requiredContent(value: unknown): string {
  if (typeof value !== "string") throw new Error("content must be a string");
  if (value.length > MAX_FILE_BYTES) throw new Error("content exceeds 1 MiB");
  return value;
}

function requiredProcessId(value: unknown): string {
  if (typeof value !== "string" || !/^[A-Za-z0-9][A-Za-z0-9_-]*$/.test(value)) {
    throw new Error("process_id must be a safe Sandbox process ID");
  }
  return value;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
