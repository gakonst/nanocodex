import type { NamedTool, ToolContext } from "nanocodex";
import { justBash } from "nanocodex/tools/bash";
import type { JustBashDescriptor } from "nanocodex/tools/bash";
import type { Workspace } from "nanocodex/workspace";

import {
  createManagedGitCommand,
  createManagedGhCommand,
  createManagedShellFetch,
  type ManagedShellFetch,
} from "./computer-shell";
import {
  createComputerFilesystem,
  type ComputerWorkspaceClient,
} from "./computer-workspace";
import type { ManagedComputerProvider } from "./computer-provider";
import { createCloudflareSshCommand } from "./cloudflare-ssh";
import type { ManagedEgressConnectorId } from "./managed-egress";

const MANAGED_SHELL_MAX_ENTRIES = 20_000;
const MANAGED_SHELL_MAX_OUTPUT_TOKENS = 10_000;
const OUTPUT_TRUNCATION_NOTICE = "\n[output truncated by exec_command]";
const ROOT = "/workspace";
const COMMAND_SEPARATORS = new Set([";", "&&", "||", "|"]);
const META_COMMANDS = new Set([
  ".",
  "bash",
  "builtin",
  "command",
  "env",
  "eval",
  "exec",
  "sh",
  "source",
  "xargs",
]);
const READ_ONLY_RETRY_COMMANDS = new Set([
  "[", "basename", "cat", "cmp", "cut", "diff", "dirname", "echo",
  "false", "file", "find", "grep", "head", "ls", "printenv", "printf",
  "pwd", "readlink", "realpath", "stat", "tail", "test", "tr", "true",
  "uniq", "wc",
]);
const FIND_MUTATING_ACTIONS = new Set([
  "-delete", "-exec", "-execdir", "-fprint", "-fprint0", "-fprintf", "-ok", "-okdir",
]);

type DisposableComputerWorkspace = ComputerWorkspaceClient & Readonly<{
  [Symbol.dispose](): void;
}>;

type ComputerProviderOption = ManagedComputerProvider
  | ((workspace: Workspace) => ManagedComputerProvider);

type ExecCommandInput = Readonly<{
  cmd: string;
  max_output_tokens?: number;
  sandbox_permissions?: string;
  shell?: string;
  tty?: boolean;
  workdir?: string;
}>;

type ShellToken = Readonly<{
  kind: "operator" | "word";
  value: string;
}>;

export type ManagedComputerRuntime = Readonly<{
  commandNames: readonly string[];
  descriptor: JustBashDescriptor;
  dispose(): void;
  fetch: ManagedShellFetch;
  filesystem: Workspace;
  instructions: string;
  tool: NamedTool;
}>;

/**
 * Constructs the one managed shell runtime used by every managed agent profile.
 * Just Bash is the zero-machine fast path. When configured compute is available,
 * the first command that cannot be faithfully handled by the embedded command
 * set promotes the whole runtime to that machine. Promotion is one-way: every
 * later exec_command for this runtime goes to the retained machine while the
 * model continues to see the same tool.
 */
export async function createManagedComputerRuntime(options: Readonly<{
  computer: DisposableComputerWorkspace;
  computerProvider?: ComputerProviderOption;
  connectorAllowed?: (connector: ManagedEgressConnectorId, instance?: string) => boolean;
  egress: Fetcher;
  sshIdentityAllowed?: (reference: string) => boolean;
  subject?: string;
  sshPassword?: (reference: string) => Promise<string>;
}>): Promise<ManagedComputerRuntime> {
  let disposed = false;
  let provider: ManagedComputerProvider | undefined;
  const dispose = () => {
    if (disposed) return;
    disposed = true;
    options.computer[Symbol.dispose]();
    try {
      const closing = provider?.dispose?.();
      if (closing instanceof Promise) void closing.catch(() => {});
    } catch {
      // Workspace disposal remains best-effort and synchronous at this boundary.
    }
  };

  try {
    const sourceFilesystem = await createComputerFilesystem(options.computer);
    provider = typeof options.computerProvider === "function"
      ? options.computerProvider(sourceFilesystem)
      : options.computerProvider;
    const fetch = createManagedShellFetch(
      options.egress,
      options.subject,
      options.connectorAllowed,
    );
    let mountedFilesystem: Workspace | undefined;
    const gitCommand = createManagedGitCommand(fetch, () => {
      if (!mountedFilesystem) throw new Error("managed shell filesystem is not mounted");
      return mountedFilesystem;
    });
    const commands = Object.freeze([
      gitCommand,
      createManagedGhCommand(fetch, (args, context) =>
        gitCommand.execute(["clone", ...args], context)),
      {
        name: "ssh",
        load: async () => createCloudflareSshCommand({
          egress: options.egress,
          filesystem: () => {
            if (!mountedFilesystem) throw new Error("managed shell filesystem is not mounted");
            return mountedFilesystem;
          },
          ...(options.sshPassword === undefined ? {} : { resolvePassword: options.sshPassword }),
          ...(options.sshIdentityAllowed === undefined
            ? {}
            : { sshIdentityAllowed: options.sshIdentityAllowed }),
          ...(options.subject === undefined ? {} : { subject: options.subject }),
        }),
      },
    ]);
    const shell = await justBash({
      filesystem: sourceFilesystem,
      maxEntries: MANAGED_SHELL_MAX_ENTRIES,
      maxOutputTokens: MANAGED_SHELL_MAX_OUTPUT_TOKENS,
      fetch,
      networkMode: options.subject === undefined
        ? "public-http-only"
        : "connector-http-gateway",
      customCommands: commands,
    });
    mountedFilesystem = shell.filesystem;
    const tool = provider === undefined
      ? shell.tool
      : createStickyExecutionTool(
          shell.tool,
          provider,
          new Set([...shell.descriptor.commands, ...shell.descriptor.customCommands]),
        );

    return Object.freeze({
      commandNames: shell.descriptor.customCommands,
      descriptor: shell.descriptor,
      dispose,
      fetch,
      filesystem: shell.filesystem,
      instructions: provider === undefined
        ? shell.instructions
        : managedShellInstructions(shell.descriptor),
      tool: Object.freeze({ ...tool, dispose }),
    });
  } catch (error) {
    dispose();
    throw error;
  }
}

function createStickyExecutionTool(
  virtualTool: NamedTool,
  provider: ManagedComputerProvider,
  safeCommands: ReadonlySet<string>,
): NamedTool {
  let promoted = false;
  let executionTail = Promise.resolve();
  const execute = async (input: unknown, context: ToolContext) => {
    const parsed = execCommandInput(input);
    const brokeredSsh = brokeredSshRouting(parsed.cmd);
    if (brokeredSsh === "protected") {
      if (inputRequiresNative(parsed)) {
        return protectedSshError("brokered SSH cannot request native shell execution");
      }
      return virtualTool.handler(input, context);
    }
    if (brokeredSsh === "invalid") {
      return protectedSshError("brokered SSH must be one standalone literal ssh command");
    }
    if (promoted || inputRequiresNative(parsed) || !isVirtualSafeCommand(parsed.cmd, safeCommands)) {
      promoted = true;
      return executeNativeCommand(parsed, context, provider);
    }

    const retrySafe = isVirtualRetrySafeCommand(parsed.cmd);
  try {
    const result = await virtualTool.handler(input, context);
    if (!isVirtualLimitationResult(result)) return result;
    promoted = true;
    return retrySafe
      ? executeNativeCommand(parsed, context, provider)
      : result;
  } catch (error) {
    if (!isVirtualLimitationError(error)) throw error;
    promoted = true;
    if (!retrySafe) throw error;
    return executeNativeCommand(parsed, context, provider);
  }
  };

  return Object.freeze({
    ...virtualTool,
    handler(input, context) {
      const run = () => execute(input, context);
      const result = executionTail.then(run, run);
      executionTail = result.then(() => undefined, () => undefined);
      return result;
    },
  });
}

function brokeredSshRouting(command: string): "none" | "protected" | "invalid" {
  if (!command.includes("IdentityRef")) return "none";
  const tokens = lexShell(command);
  if (!tokens) return "invalid";
  const words = tokens.map(({ value }) => value);
  const references = words.filter((value, index) => (
    index > 0 && words[index - 1] === "-o" && value.startsWith("IdentityRef=")
  ));
  if (references.length === 0) return "none";
  if (tokens.some(({ kind }) => kind === "operator") || words[0] !== "ssh") return "invalid";
  return references.length === 1 ? "protected" : "invalid";
}

function protectedSshError(message: string) {
  return { output: `${message}\n`, wall_time_seconds: 0, exit_code: 2 };
}

async function executeNativeCommand(
  input: ExecCommandInput,
  context: ToolContext,
  provider: ManagedComputerProvider,
) {
  if (context.signal.aborted) throw context.signal.reason ?? new Error("exec_command cancelled");
  const startedAt = Date.now();
  const result = await provider.exec({
    command: nativeCommand(input),
    cwd: resolveWorkdir(input.workdir),
    requirements: { capabilities: ["native-process"] },
  });
  if (context.signal.aborted) throw context.signal.reason ?? new Error("exec_command cancelled");
  const combined = `${result.stdout}${result.stderr}`;
  const outputTokens = Math.min(
    MANAGED_SHELL_MAX_OUTPUT_TOKENS,
    positiveInteger(input.max_output_tokens, MANAGED_SHELL_MAX_OUTPUT_TOKENS, "max_output_tokens"),
  );
  const maxCharacters = outputTokens * 4;
  const truncated = combined.length > maxCharacters;
  const retainedCharacters = Math.max(0, maxCharacters - OUTPUT_TRUNCATION_NOTICE.length);
  return {
    output: truncated
      ? maxCharacters >= OUTPUT_TRUNCATION_NOTICE.length
        ? `${combined.slice(0, retainedCharacters)}${OUTPUT_TRUNCATION_NOTICE}`
        : combined.slice(0, maxCharacters)
      : combined,
    wall_time_seconds: (Date.now() - startedAt) / 1000,
    exit_code: result.exitCode,
    ...(truncated ? { original_token_count: Math.ceil(combined.length / 4) } : {}),
  };
}

function inputRequiresNative(input: ExecCommandInput): boolean {
  return input.tty === true
    || input.sandbox_permissions === "require_escalated"
    || input.shell !== undefined && input.shell !== "bash" && input.shell !== "/bin/bash";
}

/**
 * A conservative shell preflight. Literal commands implemented by Just Bash are
 * cheap. Anything dynamic or capable of hiding another executable promotes
 * before the command runs, so retry cannot duplicate preceding shell effects.
 */
export function isVirtualSafeCommand(command: string, safeCommands: ReadonlySet<string>): boolean {
  const tokens = lexShell(command);
  if (!tokens) return false;
  let expectingCommand = true;
  let skipRedirectionTarget = false;
  let currentCommand: string | undefined;
  let currentArguments: string[] = [];
  const finishInvocation = () => currentCommand === undefined
    || isManagedShimInvocationSafe(currentCommand, currentArguments);

  for (const token of tokens) {
    if (token.kind === "operator") {
      if (token.value === "(" || token.value === ")" || token.value === "<<" || token.value === "<<<") return false;
      if (token.value === ">" || token.value === ">>" || token.value === "<") {
        skipRedirectionTarget = true;
        continue;
      }
      if (COMMAND_SEPARATORS.has(token.value)) {
        if (!finishInvocation()) return false;
        expectingCommand = true;
        currentCommand = undefined;
        currentArguments = [];
        continue;
      }
      return false;
    }
    if (skipRedirectionTarget) {
      skipRedirectionTarget = false;
      continue;
    }
    if (expectingCommand) {
      if (isAssignment(token.value)) continue;
      if (token.value.includes("$") || token.value.includes("*")) return false;
      if (META_COMMANDS.has(token.value) || !safeCommands.has(token.value)) return false;
      currentCommand = token.value;
      currentArguments = [];
      expectingCommand = false;
      continue;
    }
    if (currentCommand === "find" && (token.value === "-exec" || token.value === "-execdir")) return false;
    currentArguments.push(token.value);
  }
  return !skipRedirectionTarget && finishInvocation();
}

function isManagedShimInvocationSafe(command: string, args: readonly string[]): boolean {
  if (command === "git") {
    switch (args[0]) {
      case "clone": return args.length >= 2;
      case "status": return args.slice(1).every((arg) => ["--short", "-s", "--porcelain"].includes(arg));
      case "log": return args.slice(1).every((arg, index, values) =>
        arg === "--oneline"
        || /^-\d+$/u.test(arg)
        || arg === "-n" && /^\d+$/u.test(values[index + 1] ?? "")
        || index > 0 && values[index - 1] === "-n" && /^\d+$/u.test(arg)
        || arg === "--max-count"
        || index > 0 && values[index - 1] === "--max-count" && /^\d+$/u.test(arg)
        || /^--max-count=\d+$/u.test(arg));
      case "rev-parse": return args.length === 2 && args[1] === "HEAD";
      case "branch": return args.length === 1 || args.length === 2 && args[1] === "--show-current";
      case "remote": return args.length === 1 || args.length === 2 && args[1] === "-v";
      case "ls-files": return args.length === 1;
      default: return false;
    }
  }
  if (command === "gh") {
    if (args[0] === "auth") return args.length === 2 && args[1] === "status";
    if (args[0] === "api") return args.length >= 2;
    if (args[0] === "repo") return args[1] === "view" || args[1] === "clone" || args[1] === "list";
    return args[0] === "pr" && args[1] === "list";
  }
  return true;
}

/** Retry an emulator-only failure only when replay cannot duplicate mutations. */
export function isVirtualRetrySafeCommand(command: string): boolean {
  const tokens = lexShell(command);
  if (!tokens) return false;
  let expectingCommand = true;
  let skipRedirectionTarget = false;
  let currentCommand: string | undefined;
  for (const token of tokens) {
    if (token.kind === "operator") {
      if (token.value === ">" || token.value === ">>") return false;
      if (token.value === "<") { skipRedirectionTarget = true; continue; }
      if (COMMAND_SEPARATORS.has(token.value)) {
        expectingCommand = true;
        currentCommand = undefined;
        continue;
      }
      return false;
    }
    if (skipRedirectionTarget) { skipRedirectionTarget = false; continue; }
    if (expectingCommand) {
      if (isAssignment(token.value)) continue;
      if (!READ_ONLY_RETRY_COMMANDS.has(token.value)) return false;
      currentCommand = token.value;
      expectingCommand = false;
      continue;
    }
    if (currentCommand === "find" && FIND_MUTATING_ACTIONS.has(token.value)) return false;
  }
  return !skipRedirectionTarget;
}

function lexShell(command: string): ShellToken[] | undefined {
  const tokens: ShellToken[] = [];
  let word = "";
  const pushWord = () => {
    if (!word) return;
    tokens.push({ kind: "word", value: word });
    word = "";
  };

  for (let index = 0; index < command.length;) {
    const character = command[index]!;
    if (character === "`" || (character === "$" && command[index + 1] === "(")) return undefined;
    if ((character === "<" || character === ">") && command[index + 1] === "(") return undefined;

    if (character === "'") {
      index += 1;
      const end = command.indexOf("'", index);
      if (end < 0) return undefined;
      word += command.slice(index, end);
      index = end + 1;
      continue;
    }
    if (character === '"') {
      index += 1;
      let closed = false;
      while (index < command.length) {
        const quoted = command[index]!;
        if (quoted === '"') {
          closed = true;
          index += 1;
          break;
        }
        if (quoted === "`" || (quoted === "$" && command[index + 1] === "(")) return undefined;
        if (quoted === "\\" && index + 1 < command.length) {
          word += command[index + 1]!;
          index += 2;
          continue;
        }
        word += quoted;
        index += 1;
      }
      if (!closed) return undefined;
      continue;
    }
    if (character === "\\") {
      if (index + 1 >= command.length) return undefined;
      word += command[index + 1]!;
      index += 2;
      continue;
    }
    if (character === "#" && word.length === 0) {
      while (index < command.length && command[index] !== "\n") index += 1;
      continue;
    }
    if (/\s/u.test(character)) {
      pushWord();
      if (character === "\n") tokens.push({ kind: "operator", value: ";" });
      index += 1;
      continue;
    }
    if (";&|()<> ".includes(character)) {
      pushWord();
      const next = command[index + 1];
      const third = command[index + 2];
      if (character === "&" && next === "&" || character === "|" && next === "|") {
        tokens.push({ kind: "operator", value: `${character}${next}` });
        index += 2;
        continue;
      }
      if (character === ">" && next === ">") {
        tokens.push({ kind: "operator", value: ">>" });
        index += 2;
        continue;
      }
      if (character === "<" && next === "<" && third === "<") {
        tokens.push({ kind: "operator", value: "<<<" });
        index += 3;
        continue;
      }
      if (character === "<" && next === "<") {
        tokens.push({ kind: "operator", value: "<<" });
        index += 2;
        continue;
      }
      tokens.push({ kind: "operator", value: character });
      index += 1;
      continue;
    }
    word += character;
    index += 1;
  }
  pushWord();
  return tokens;
}

function isAssignment(value: string): boolean {
  return /^[A-Za-z_][A-Za-z0-9_]*=/u.test(value);
}

function isVirtualLimitationResult(value: unknown): boolean {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const result = value as Record<string, unknown>;
  if (result.exit_code !== 126 && result.exit_code !== 127) return false;
  if (typeof result.output !== "string") return false;
  return /command not found|not supported|unsupported|not found/iu.test(result.output);
}

function isVirtualLimitationError(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  return /Just Bash|embedded Bash interpreter|not implemented by just-bash/iu.test(error.message);
}

function execCommandInput(value: unknown): ExecCommandInput {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("exec_command input must be an object");
  }
  const input = value as Record<string, unknown>;
  if (typeof input.cmd !== "string" || !input.cmd.trim()) {
    throw new TypeError("exec_command.cmd must be a non-empty string");
  }
  if (input.workdir !== undefined && typeof input.workdir !== "string") {
    throw new TypeError("exec_command.workdir must be a string");
  }
  if (input.shell !== undefined && typeof input.shell !== "string") {
    throw new TypeError("exec_command.shell must be a string");
  }
  return input as ExecCommandInput;
}

function nativeCommand(input: ExecCommandInput): string {
  if (input.shell === undefined || input.shell === "bash" || input.shell === "/bin/bash") {
    return input.cmd;
  }
  return `${shellQuote(input.shell)} -lc ${shellQuote(input.cmd)}`;
}

function resolveWorkdir(raw = ROOT): string {
  const parts = (raw.startsWith("/") ? raw : `${ROOT}/${raw}`).split("/");
  const normalized: string[] = [];
  for (const part of parts) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (normalized.length <= 1) throw new Error(`computer cwd must stay within ${ROOT}`);
      normalized.pop();
      continue;
    }
    normalized.push(part);
  }
  const path = `/${normalized.join("/")}`;
  if (path !== ROOT && !path.startsWith(`${ROOT}/`)) {
    throw new Error(`computer cwd must stay within ${ROOT}`);
  }
  return path;
}

function positiveInteger(value: number | undefined, fallback: number, name: string): number {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || value <= 0) throw new RangeError(`${name} must be a positive integer`);
  return value;
}

function managedShellInstructions(descriptor: JustBashDescriptor): string {
  const network = descriptor.network.enabled
    ? `HTTP is available through the host-owned ${descriptor.network.mode} boundary.`
    : "Network commands are unavailable.";
  return `You have a persistent shell workspace rooted at ${descriptor.cwd}.
Use exec_command for shell work. When the user requests a concrete shell operation, run it directly and once with
the complete command instead of probing the runtime first. For an ordinary clone request, use exactly gh repo clone
OWNER/REPO DESTINATION or git clone URL DESTINATION. Do not add depth, filter, branch, or other flags unless requested,
and do not inspect a successful clone. Only investigate after the direct command fails or when explicitly asked.
Files persist across calls and agent restarts. ${network} Model subscription credentials are never exposed to the shell.`;
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `'\\''`)}'`;
}
