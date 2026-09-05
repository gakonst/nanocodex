import type { Sandbox } from "@vercel/sandbox";
import type { ToolMap } from "nanocodex";

import {
  PHYSICAL_WORKSPACE as WORKSPACE,
  PREVIEW_PORTS,
  VIRTUAL_WORKSPACE,
  prepareSessionSandbox,
} from "./session-sandbox";
const MAX_COMMAND_CHARS = 32 * 1024;
const MAX_FILE_BYTES = 1024 * 1024;
const MAX_OUTPUT_BYTES = 128 * 1024;
const MAX_LIST_ENTRIES = 512;
const MAX_TIMEOUT_MS = 120_000;

export function vercelSandboxTools(sessionId: string): ToolMap {
  return createVercelSandboxTools(() => prepareSessionSandbox(sessionId));
}

export function createVercelSandboxTools(
  createSandbox: () => Promise<Pick<Sandbox, "domain" | "fs" | "runCommand">>,
): ToolMap {
  let sandboxPromise: ReturnType<typeof createSandbox> | undefined;
  const sandbox = () => sandboxPromise ??= createSandbox();

  return {
    sandbox_exec: {
      description: "Run a shell command in this session's isolated persistent Vercel Sandbox workspace.",
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
        const timeoutMs = optionalInteger(value.timeout_ms, "timeout_ms", 1, MAX_TIMEOUT_MS) ?? 60_000;
        const result = await (await sandbox()).runCommand({
          cmd: "bash",
          args: ["-lc", command],
          cwd,
          timeoutMs,
        });
        const [stdout, stderr] = await Promise.all([result.stdout(), result.stderr()]);
        const boundedStdout = truncate(stdout);
        const boundedStderr = truncate(stderr);
        return {
          success: result.exitCode === 0,
          exit_code: result.exitCode,
          stdout: boundedStdout.text,
          stderr: boundedStderr.text,
          stdout_truncated: boundedStdout.truncated,
          stderr_truncated: boundedStderr.truncated,
          duration_ms: result.durationMs,
        };
      },
    },
    sandbox_read_file: {
      description: "Read a UTF-8 text file from this session's isolated workspace (maximum 1 MiB).",
      parameters: pathParameters(),
      handler: async (input) => {
        const path = workspacePath(requiredString(objectInput(input).path, "path", 1024));
        const sandboxHandle = await sandbox();
        const stat = await sandboxHandle.fs.stat(path);
        if (!stat.isFile()) throw new Error("path is not a file");
        if (stat.size > MAX_FILE_BYTES) throw new Error("file exceeds 1 MiB");
        const content = await sandboxHandle.fs.readFile(path);
        if (content.byteLength > MAX_FILE_BYTES) throw new Error("file exceeds 1 MiB");
        let text: string;
        try {
          text = new TextDecoder("utf-8", { fatal: true }).decode(content);
        } catch {
          throw new Error("file is not valid UTF-8");
        }
        return { path: virtualPath(path), content: text };
      },
    },
    sandbox_start_process: {
      description: "Start a detached process in this session's isolated Vercel Sandbox, optionally waiting for a TCP port to become ready.",
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
        const readyPort = optionalInteger(value.ready_port, "ready_port", 1024, 65_535);
        const readyTimeout = optionalInteger(
          value.ready_timeout_ms,
          "ready_timeout_ms",
          1,
          MAX_TIMEOUT_MS,
        ) ?? 30_000;
        const sandboxHandle = await sandbox();
        const process = await sandboxHandle.runCommand({
          cmd: "bash",
          args: ["-lc", command],
          cwd,
          detached: true,
        });
        if (readyPort !== undefined) {
          await waitForPort(sandboxHandle, readyPort, readyTimeout);
        }
        return {
          process_id: process.cmdId,
          command,
          status: "running",
          ...(readyPort === undefined ? {} : { ready_port: readyPort }),
        };
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
        const bytes = Buffer.byteLength(content, "utf8");
        if (bytes > MAX_FILE_BYTES) throw new Error("content exceeds 1 MiB");
        const sandboxHandle = await sandbox();
        const parent = path.slice(0, path.lastIndexOf("/")) || WORKSPACE;
        await sandboxHandle.fs.mkdir(parent, { recursive: true });
        await sandboxHandle.fs.writeFile(path, content, "utf8");
        return { path: virtualPath(path), bytes_written: bytes };
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
        const path = workspacePath(optionalString(objectInput(input).path, "path") ?? ".");
        const entries = await (await sandbox()).fs.readdir(path, { withFileTypes: true });
        return {
          path: virtualPath(path),
          entries: entries.slice(0, MAX_LIST_ENTRIES).map((entry) => ({
            name: entry.name,
            type: entry.isDirectory() ? "directory" : entry.isFile() ? "file" : entry.isSymbolicLink() ? "symlink" : "other",
          })),
          truncated: entries.length > MAX_LIST_ENTRIES,
        };
      },
    },
    sandbox_preview: {
      description: "Return the public Vercel Sandbox URL for a server listening on a supported port.",
      parameters: {
        type: "object",
        properties: { port: { type: "integer", enum: [...PREVIEW_PORTS] } },
        required: ["port"],
        additionalProperties: false,
      },
      handler: async (input) => {
        const port = requiredPreviewPort(objectInput(input).port);
        return { port, url: (await sandbox()).domain(port), persistent: false };
      },
    },
  };
}

async function waitForPort(
  sandbox: Pick<Sandbox, "runCommand">,
  port: number,
  timeoutMs: number,
): Promise<void> {
  const result = await sandbox.runCommand({
    cmd: "bash",
    args: [
      "-lc",
      `until (exec 3<>/dev/tcp/127.0.0.1/${port}) 2>/dev/null; do sleep 0.1; done`,
    ],
    timeoutMs,
  });
  if (result.exitCode !== 0) throw new Error(`port ${port} did not become ready`);
}

export function workspacePath(raw: string): string {
  if (!raw || raw.length > 1024 || raw.includes("\0")) throw new Error("path must be 1-1024 characters");
  let relative = raw;
  if (relative === VIRTUAL_WORKSPACE || relative === WORKSPACE) relative = ".";
  else if (relative.startsWith(`${VIRTUAL_WORKSPACE}/`)) relative = relative.slice(VIRTUAL_WORKSPACE.length + 1);
  else if (relative.startsWith(`${WORKSPACE}/`)) relative = relative.slice(WORKSPACE.length + 1);
  else if (relative.startsWith("/")) throw new Error("path must be relative to /workspace");
  const parts = relative.split("/").filter((part) => part !== "" && part !== ".");
  if (parts.includes("..")) throw new Error("path must not contain '..'");
  return parts.length === 0 ? WORKSPACE : `${WORKSPACE}/${parts.join("/")}`;
}

function virtualPath(path: string): string {
  return path === WORKSPACE ? VIRTUAL_WORKSPACE : `${VIRTUAL_WORKSPACE}${path.slice(WORKSPACE.length)}`;
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

function requiredContent(value: unknown): string {
  if (typeof value !== "string") throw new Error("content must be a string");
  if (value.length > MAX_FILE_BYTES) throw new Error("content exceeds 1 MiB");
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

function requiredPreviewPort(value: unknown): (typeof PREVIEW_PORTS)[number] {
  if (typeof value !== "number" || !PREVIEW_PORTS.includes(value as (typeof PREVIEW_PORTS)[number])) {
    throw new Error(`port must be one of ${PREVIEW_PORTS.join(", ")}`);
  }
  return value as (typeof PREVIEW_PORTS)[number];
}

function pathParameters(): Record<string, unknown> {
  return {
    type: "object",
    properties: { path: { type: "string", description: "Workspace-relative file path." } },
    required: ["path"],
    additionalProperties: false,
  };
}

function truncate(value: string): { text: string; truncated: boolean } {
  const encoded = new TextEncoder().encode(value);
  if (encoded.byteLength <= MAX_OUTPUT_BYTES) return { text: value, truncated: false };
  let end = MAX_OUTPUT_BYTES;
  while (end > 0) {
    try {
      return {
        text: new TextDecoder("utf-8", { fatal: true }).decode(encoded.subarray(0, end)),
        truncated: true,
      };
    } catch {
      end -= 1;
    }
  }
  return { text: "", truncated: true };
}
