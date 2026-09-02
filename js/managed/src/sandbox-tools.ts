import { getSandbox, type Sandbox } from "@cloudflare/sandbox";
import type { ToolMap } from "nanocodex";

const WORKSPACE = "/workspace";
const MAX_COMMAND_CHARS = 32 * 1024;
const MAX_FILE_BYTES = 1024 * 1024;
const MAX_OUTPUT_BYTES = 128 * 1024;
const MAX_LIST_ENTRIES = 512;
const MAX_TIMEOUT_MS = 120_000;
const PREVIEW_AAD = new TextEncoder().encode("nanocodex-cloudflare-sandbox-preview-v1");

type SandboxToolClient = {
  exec(
    command: string,
    options: { cwd: string; timeout: number },
  ): Promise<{
    success: boolean;
    exitCode: number;
    stdout: string;
    stderr: string;
    duration: number;
  }>;
  startProcess(
    command: string,
    options: { cwd: string; autoCleanup: true },
  ): Promise<{
    id: string;
    pid?: number;
    command: string;
    status: string;
    getStatus(): Promise<string>;
    waitForPort(port: number, options: { timeout: number }): Promise<void>;
  }>;
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
  await sandboxHandle(namespace, sessionId).destroy();
}

export async function deleteCloudflareSandbox(
  namespace: DurableObjectNamespace<Sandbox>,
  bucket: R2Bucket,
  sessionId: string,
): Promise<void> {
  await Promise.all([
    destroyCloudflareSandbox(namespace, sessionId),
    deleteCloudflareSandboxWorkspace(bucket, sessionId),
  ]);
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
  let sandboxPromise: Promise<SandboxToolClient> | undefined;
  const sandbox = () => sandboxPromise ??= createSandbox();

  return {
    sandbox_exec: {
      description: "Run a shell command in this session's isolated Cloudflare Sandbox workspace.",
      parameters: {
        type: "object",
        properties: {
          command: { type: "string", description: "Shell command to run." },
          cwd: { type: "string", description: "Workspace-relative working directory." },
          timeout_ms: { type: "integer", minimum: 1, maximum: MAX_TIMEOUT_MS },
        },
        required: ["command"],
        additionalProperties: false,
      },
      handler: async (input) => {
        const value = objectInput(input);
        const command = requiredString(value.command, "command", MAX_COMMAND_CHARS);
        const cwd = workspacePath(optionalString(value.cwd, "cwd") ?? ".");
        const timeout = optionalInteger(value.timeout_ms, "timeout_ms", 1, MAX_TIMEOUT_MS) ?? 60_000;
        return withSandboxRpcResult(
          (await sandbox()).exec(command, { cwd, timeout }),
          (result) => {
            const stdout = truncate(result.stdout);
            const stderr = truncate(result.stderr);
            return {
              success: result.success,
              exit_code: result.exitCode,
              stdout: stdout.text,
              stderr: stderr.text,
              stdout_truncated: stdout.truncated,
              stderr_truncated: stderr.truncated,
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
          (await sandbox()).readFile(path, { encoding: "none" }),
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
          ready_timeout_ms: { type: "integer", minimum: 1, maximum: MAX_TIMEOUT_MS },
        },
        required: ["command"],
        additionalProperties: false,
      },
      handler: async (input) => {
        const value = objectInput(input);
        const command = requiredString(value.command, "command", MAX_COMMAND_CHARS);
        const cwd = workspacePath(optionalString(value.cwd, "cwd") ?? ".");
        const readyPort = optionalPort(value.ready_port, "ready_port");
        const readyTimeout = optionalInteger(
          value.ready_timeout_ms,
          "ready_timeout_ms",
          1,
          MAX_TIMEOUT_MS,
        ) ?? 30_000;
        return withSandboxRpcResult(
          (await sandbox()).startProcess(command, { cwd, autoCleanup: true }),
          async (process) => {
            if (readyPort !== undefined) {
              await process.waitForPort(readyPort, { timeout: readyTimeout });
            }
            return {
              process_id: process.id,
              pid: process.pid,
              command: process.command,
              status: await process.getStatus(),
              ...(readyPort === undefined ? {} : { ready_port: readyPort }),
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
        await (await sandbox()).writeFile(path, content, { encoding: "utf-8" });
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
          (await sandbox()).listFiles(path, { includeHidden: true }),
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
        await sandbox();
        if (createPreview) return createPreview(port);
        return withSandboxRpcResult(
          (await sandbox()).tunnels.get(port),
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
  const [sessionId, rawPort, ...extra] = new TextDecoder().decode(plaintext).split("\n");
  const port = Number(rawPort);
  if (!sessionId || extra.length > 0 || !Number.isInteger(port) || port < 1024 || port > 65_535) {
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
  const target = new URL(`http://sandbox.internal${targetPath}${incoming.search}`);
  const forwarded = new Request(target, request);
  const sandbox = sandboxHandle(namespace, sessionId);
  if (request.headers.get("Upgrade")?.toLowerCase() === "websocket") {
    return sandbox.wsConnect(forwarded, port);
  }
  return sandbox.containerFetch(forwarded, port);
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
    new TextEncoder().encode(`${sessionId}\n${port}`),
  ));
  const sealed = new Uint8Array(iv.byteLength + ciphertext.byteLength);
  sealed.set(iv);
  sealed.set(ciphertext, iv.byteLength);
  return encodeBase64Url(sealed);
}

async function previewKey(secret: string): Promise<CryptoKey> {
  if (!secret) throw new Error("preview secret is required");
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(secret));
  return crypto.subtle.importKey("raw", digest, { name: "AES-GCM" }, false, ["encrypt", "decrypt"]);
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
  const sandbox = sandboxHandle(namespace, sessionId);
  try {
    await sandbox.mountBucket("NANOCODEX_WORKSPACES", WORKSPACE, {
      prefix: `/sessions/${sessionId}/`,
      ...(localBucket ? { localBucket: true as const } : {}),
    });
  } catch (error) {
    if (!errorMessage(error).toLowerCase().includes("mount path already in use")) throw error;
  }
  return sandbox;
}

function sandboxHandle(
  namespace: DurableObjectNamespace<Sandbox>,
  sessionId: string,
): Sandbox {
  return getSandbox(namespace, `nanocodex-${sessionId}`, {
    normalizeId: true,
    sleepAfter: "10m",
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

async function withSandboxRpcResult<T extends object, R>(
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

function disposeSandboxRpcValue(value: object): void {
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

function requiredContent(value: unknown): string {
  if (typeof value !== "string") throw new Error("content must be a string");
  if (value.length > MAX_FILE_BYTES) throw new Error("content exceeds 1 MiB");
  return value;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
