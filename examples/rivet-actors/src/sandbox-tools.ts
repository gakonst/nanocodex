import {
  createAgentOsActions,
  type AgentOsActorExtras,
  type AgentOsOptions,
} from "@rivet-dev/agentos";
import type { ToolMap } from "nanocodex";
import type { RawAccess } from "rivetkit/db";

const WORKSPACE = "/workspace";
const MAX_COMMAND_CHARS = 32 * 1024;
const MAX_FILE_BYTES = 1024 * 1024;
const MAX_OUTPUT_BYTES = 128 * 1024;
const MAX_LIST_ENTRIES = 512;
const MAX_TIMEOUT_MS = 120_000;
export const MAX_PREVIEW_TTL_SECONDS = 15 * 60;
const PREVIEW_PROCESS_LIMIT_MS = (MAX_PREVIEW_TTL_SECONDS + 60) * 1_000;

export const agentOsRuntimeOptions = {
  defaultSoftware: true,
  limits: {
    jsRuntime: {
      cpuTimeLimitMs: PREVIEW_PROCESS_LIMIT_MS,
      wallClockLimitMs: PREVIEW_PROCESS_LIMIT_MS,
    },
    resources: {
      maxFilesystemBytes: 512 * 1024 * 1024,
      maxProcesses: 64,
      maxReaddirEntries: 4_096,
    },
  },
} satisfies AgentOsOptions;

export const agentOsPreviewOptions = {
  defaultExpiresInSeconds: 15 * 60,
  maxExpiresInSeconds: MAX_PREVIEW_TTL_SECONDS,
  maxActiveTokens: 32,
} satisfies NonNullable<AgentOsActorExtras["preview"]>;

const agentOsActions = createAgentOsActions(
  agentOsRuntimeOptions,
  undefined,
  agentOsPreviewOptions,
);

export type AgentOsActionContext = Parameters<typeof agentOsActions.exec>[0];
type SandboxAgentOsActions = Pick<
  typeof agentOsActions,
  | "createPreviewUrl"
  | "exec"
  | "killProcess"
  | "mkdir"
  | "readFile"
  | "readdirEntries"
  | "vmFetch"
  | "writeFile"
>;
type SpawnedProcess = { pid: number };
type SelfActorClient = {
  nanocodex: {
    getForId(actorId: string): {
      process: {
        spawn(
          command: string,
          args: string[],
          options: { cwd: string; output: { retainEvents: true } },
        ): Promise<SpawnedProcess>;
      };
    };
  };
};
type AgentOsVmStart = NonNullable<AgentOsActorExtras["onVmStart"]>;
type AgentOsVmStartContext = Parameters<AgentOsVmStart>[0];
type AgentOsVm = Parameters<AgentOsVmStart>[1];
type PreviewServerRow = {
  port: number;
  command: string;
  args_json: string;
  cwd: string;
};

export async function migrateRivetSandboxDatabase(database: RawAccess): Promise<void> {
  await database.execute(`
    CREATE TABLE IF NOT EXISTS nanocodex_preview_servers (
      port INTEGER PRIMARY KEY,
      command TEXT NOT NULL,
      args_json TEXT NOT NULL,
      cwd TEXT NOT NULL,
      expires_at_ms INTEGER NOT NULL DEFAULT 0
    )
  `);
}

export async function restoreRivetPreviewServers(
  context: AgentOsVmStartContext,
  vm: AgentOsVm,
): Promise<void> {
  const now = Date.now();
  await context.db.execute(
    "DELETE FROM nanocodex_preview_servers WHERE expires_at_ms <= ?",
    now,
  );
  const rows = await context.db.execute<PreviewServerRow>(
    `SELECT port, command, args_json, cwd
     FROM nanocodex_preview_servers
     WHERE expires_at_ms > ?
     ORDER BY port`,
    now,
  );
  for (const row of rows) {
    try {
      const args = parseStoredArgs(row.args_json);
      await vm.process.spawn(row.command, args, {
        cwd: row.cwd,
        output: { retainEvents: true },
      });
      await waitForVmPort(vm, row.port, 30_000);
    } catch (error) {
      context.log.error({
        msg: "failed to restore Rivet sandbox preview server",
        port: row.port,
        error,
      });
      await context.db.execute(
        "DELETE FROM nanocodex_preview_servers WHERE port = ?",
        row.port,
      );
    }
  }
}

export function rivetSandboxTools(
  context: AgentOsActionContext,
  sessionId: string,
): ToolMap {
  void sessionId;
  return createRivetSandboxTools(context);
}

export function createRivetSandboxTools(
  context: AgentOsActionContext,
  actions: SandboxAgentOsActions = agentOsActions,
): ToolMap {
  return {
    sandbox_exec: {
      description: "Run a shell command in this actor's isolated persistent Rivet AgentOS workspace.",
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
        const result = await actions.exec(context, command, {
          cwd,
          timeout,
          captureStdio: true,
        });
        const stdout = truncate(result.stdout);
        const stderr = truncate(result.stderr);
        return {
          success: result.exitCode === 0,
          exit_code: result.exitCode,
          stdout: stdout.text,
          stderr: stderr.text,
          stdout_truncated: stdout.truncated,
          stderr_truncated: stderr.truncated,
        };
      },
    },
    sandbox_read_file: {
      description: "Read a UTF-8 text file from this actor's isolated workspace (maximum 1 MiB).",
      parameters: pathParameters(),
      handler: async (input) => {
        const path = workspacePath(requiredString(objectInput(input).path, "path", 1024));
        const content = await actions.readFile(context, path);
        if (content.byteLength > MAX_FILE_BYTES) throw new Error("file exceeds 1 MiB");
        let text: string;
        try {
          text = new TextDecoder("utf-8", { fatal: true }).decode(content);
        } catch {
          throw new Error("file is not valid UTF-8");
        }
        return { path, content: text };
      },
    },
    sandbox_start_process: {
      description: "Start a native executable in this actor's isolated Rivet AgentOS VM, optionally waiting for an HTTP port to become ready.",
      parameters: {
        type: "object",
        properties: {
          command: { type: "string", description: "Executable to start, such as node." },
          args: {
            type: "array",
            items: { type: "string" },
            description: "Arguments passed directly to the executable, without a shell.",
          },
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
        const args = optionalStringArray(value.args, "args", 128, 8_192) ?? [];
        const cwd = workspacePath(optionalString(value.cwd, "cwd") ?? ".");
        const readyPort = optionalInteger(value.ready_port, "ready_port", 1024, 65_535);
        const readyTimeout = optionalInteger(
          value.ready_timeout_ms,
          "ready_timeout_ms",
          1,
          MAX_TIMEOUT_MS,
        ) ?? 30_000;
        // Invoke AgentOS's generated process action through the actor handle.
        // A process started directly inside the long Nanocodex turn is scoped to
        // that action and is reaped when the turn context closes.
        const self = (context.client() as SelfActorClient).nanocodex.getForId(context.actorId);
        const process = await self.process.spawn(command, args, {
          cwd,
          output: { retainEvents: true },
        });
        try {
          if (readyPort !== undefined) {
            await waitForPort(actions, context, readyPort, readyTimeout);
            await context.db.execute(
              `INSERT INTO nanocodex_preview_servers
                 (port, command, args_json, cwd, expires_at_ms)
               VALUES (?, ?, ?, ?, 0)
               ON CONFLICT(port) DO UPDATE SET
                 command = excluded.command,
                 args_json = excluded.args_json,
                 cwd = excluded.cwd,
                 expires_at_ms = 0`,
              readyPort,
              command,
              JSON.stringify(args),
              cwd,
            );
          }
        } catch (error) {
          await actions.killProcess(context, process.pid).catch(() => {});
          throw error;
        }
        return {
          process_id: process.pid,
          command,
          args,
          status: "running",
          ...(readyPort === undefined ? {} : { ready_port: readyPort }),
        };
      },
    },
    sandbox_write_file: {
      description: "Write a UTF-8 text file inside this actor's isolated workspace (maximum 1 MiB).",
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
        const parent = path.slice(0, path.lastIndexOf("/")) || WORKSPACE;
        await actions.mkdir(context, parent, { recursive: true });
        await actions.writeFile(context, path, content);
        return { path, bytes_written: bytes };
      },
    },
    sandbox_list_files: {
      description: "List files in a directory inside this actor's isolated workspace.",
      parameters: {
        type: "object",
        properties: {
          path: { type: "string", description: "Workspace-relative directory; defaults to the workspace root." },
        },
        additionalProperties: false,
      },
      handler: async (input) => {
        const path = workspacePath(optionalString(objectInput(input).path, "path") ?? ".");
        const entries = await actions.readdirEntries(context, path);
        return {
          path,
          entries: entries.slice(0, MAX_LIST_ENTRIES).map((entry) => ({
            name: entry.name,
            type: entry.isDirectory ? "directory" : entry.isSymbolicLink ? "symlink" : "file",
          })),
          truncated: entries.length > MAX_LIST_ENTRIES,
        };
      },
    },
    sandbox_preview: {
      description: "Expose a server running in AgentOS through a temporary Rivet Actor preview URL.",
      parameters: {
        type: "object",
        properties: {
          port: { type: "integer", minimum: 1024, maximum: 65_535 },
          ttl_seconds: { type: "integer", minimum: 60, maximum: MAX_PREVIEW_TTL_SECONDS },
        },
        required: ["port"],
        additionalProperties: false,
      },
      handler: async (input) => {
        const value = objectInput(input);
        const port = requiredInteger(value.port, "port", 1024, 65_535);
        const ttl = optionalInteger(
          value.ttl_seconds,
          "ttl_seconds",
          60,
          MAX_PREVIEW_TTL_SECONDS,
        ) ?? MAX_PREVIEW_TTL_SECONDS;
        try {
          await actions.vmFetch(context, port, "http://127.0.0.1/");
        } catch {
          throw new Error(`port ${port} is not reachable; start a listening server before creating a preview`);
        }
        const registered = await context.db.execute<{ port: number }>(
          "SELECT port FROM nanocodex_preview_servers WHERE port = ?",
          port,
        );
        if (!registered[0]) {
          throw new Error(
            `port ${port} is not durable; start it with sandbox_start_process and ready_port ${port}`,
          );
        }
        const preview = await actions.createPreviewUrl(context, port, ttl);
        await context.db.execute(
          "UPDATE nanocodex_preview_servers SET expires_at_ms = ? WHERE port = ?",
          preview.expiresAt,
          port,
        );
        const url = publicPreviewUrl(context, preview.path);
        return {
          port,
          path: preview.path,
          url,
          expires_at: new Date(preview.expiresAt).toISOString(),
          persistent: false,
        };
      },
    },
  };
}

function publicPreviewUrl(context: AgentOsActionContext, path: string): string {
  const configured = process.env.NANOCODEX_PUBLIC_URL?.trim();
  if (!configured) return path;
  const url = new URL(configured);
  const namespace = decodeURIComponent(url.username);
  const token = decodeURIComponent(url.password);
  url.username = "";
  url.password = "";
  if (namespace || token) {
    if (!namespace || !token.startsWith("pk_")) {
      throw new Error("NANOCODEX_PUBLIC_URL must contain a namespace and client-safe pk_ token");
    }
    url.pathname = `${url.pathname.replace(/\/$/, "")}/gateway/${encodeURIComponent(context.actorId)}@${encodeURIComponent(token)}/request${path}`;
    return url.toString();
  }
  url.pathname = `${url.pathname.replace(/\/$/, "")}${path}`;
  return url.toString();
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

function requiredContent(value: unknown): string {
  if (typeof value !== "string") throw new Error("content must be a string");
  if (value.length > MAX_FILE_BYTES) throw new Error("content exceeds 1 MiB");
  return value;
}

function optionalString(value: unknown, name: string): string | undefined {
  if (value === undefined) return undefined;
  return requiredString(value, name, 1024);
}

function optionalStringArray(
  value: unknown,
  name: string,
  maxItems: number,
  maxChars: number,
): string[] | undefined {
  if (value === undefined) return undefined;
  if (!Array.isArray(value) || value.length > maxItems) {
    throw new Error(`${name} must be an array with at most ${maxItems} strings`);
  }
  const parsed = value.map((item) => {
    if (typeof item !== "string") throw new Error(`${name} must contain only strings`);
    if (item.length > maxChars) throw new Error(`${name} contains an argument that is too long`);
    return item;
  });
  if (parsed.reduce((total, item) => total + item.length, 0) > MAX_COMMAND_CHARS) {
    throw new Error(`${name} is too long`);
  }
  return parsed;
}

function requiredInteger(value: unknown, name: string, minimum: number, maximum: number): number {
  const parsed = optionalInteger(value, name, minimum, maximum);
  if (parsed === undefined) throw new Error(`${name} is required`);
  return parsed;
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

function pathParameters(): Record<string, unknown> {
  return {
    type: "object",
    properties: { path: { type: "string", description: "Workspace-relative file path." } },
    required: ["path"],
    additionalProperties: false,
  };
}

async function waitForPort(
  actions: Pick<SandboxAgentOsActions, "vmFetch">,
  context: AgentOsActionContext,
  port: number,
  timeoutMs: number,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (true) {
    try {
      await actions.vmFetch(context, port, "http://127.0.0.1/");
      return;
    } catch {
      if (Date.now() >= deadline) throw new Error(`port ${port} did not become ready`);
      await new Promise((resolve) => setTimeout(resolve, Math.min(100, deadline - Date.now())));
    }
  }
}

async function waitForVmPort(vm: AgentOsVm, port: number, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (true) {
    try {
      await vm.network.httpRequest({
        port,
        path: "/",
        method: "GET",
        headers: {},
      });
      return;
    } catch {
      if (Date.now() >= deadline) throw new Error(`port ${port} did not become ready`);
      await new Promise((resolve) => setTimeout(resolve, Math.min(100, deadline - Date.now())));
    }
  }
}

function parseStoredArgs(encoded: string): string[] {
  const value: unknown = JSON.parse(encoded);
  if (!Array.isArray(value) || value.some((entry) => typeof entry !== "string")) {
    throw new Error("stored preview server arguments are invalid");
  }
  return value;
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
