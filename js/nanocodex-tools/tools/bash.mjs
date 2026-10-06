import { tryExecuteBinaryCommand } from "./shell-binary.mjs";
import { createSearchCommands } from "./shell-search.mjs";
import { namedTool } from "./namedTool.mjs";
import {
  EXEC_COMMAND_PARAMETERS,
  EXECUTION_OUTPUT_SCHEMA,
} from "./execution-contract.mjs";

const DEFAULT_MAX_OUTPUT_TOKENS = 10_000;
const MAX_OUTPUT_TOKENS = 100_000;
const OUTPUT_TRUNCATION_NOTICE = "\n[output truncated by exec_command]";
const encoder = new TextEncoder();
const decoder = new TextDecoder();

// Resource ceilings are defense in depth, not a timer-based CPU sandbox.
// Optimized commands yield inside their loops; synchronous fallback work is
// admitted before it enters the bundled interpreter's regex engine.
const DEFAULT_EXECUTION_LIMITS = Object.freeze({
  maxSourceBytes: 1024 * 1024, maxExecDepth: 64, maxCallDepth: 64,
  maxCommandCount: 10_000, maxLoopIterations: 100_000,
  maxAwkIterations: 100_000, maxSedIterations: 100_000, maxJqIterations: 1_000_000,
  maxQueryTokens: 100_000, maxQueryDepth: 256, maxQueryElements: 100_000,
  maxAwkParserTokens: 100_000, maxAwkParserDepth: 128, maxAwkParserOperations: 1_000_000,
  maxCsvRows: 100_000, maxCsvCells: 1_000_000, maxWorkUnits: 1_000_000,
  maxTraversalEntries: 100_000, maxTraversalDepth: 256, maxTraversalWork: 1_000_000,
  maxLiveBytes: 48 * 1024 * 1024, maxInputBytes: 32 * 1024 * 1024,
  maxFileSystemBytes: 256 * 1024 * 1024, maxDatabaseBytes: 16 * 1024 * 1024,
  maxDatabaseResultBytes: 16 * 1024 * 1024, maxArchiveBytes: 32 * 1024 * 1024,
  maxArchiveCompressedBytes: 32 * 1024 * 1024, maxArchiveEntryBytes: 32 * 1024 * 1024,
  maxArchiveEntries: 100_000, maxWorkerMessageBytes: 16 * 1024 * 1024,
  maxExecutionTimeMs: 30_000, maxSqliteTimeoutMs: 10_000,
  maxPythonTimeoutMs: 10_000, maxJsTimeoutMs: 10_000, maxGlobOperations: 100_000,
  maxStringLength: 16 * 1024 * 1024, maxArrayElements: 100_000,
  maxHeredocSize: 1024 * 1024, maxSubstitutionDepth: 64,
  maxBraceExpansionResults: 10_000, maxOutputSize: 4 * 1024 * 1024,
  maxFileDescriptors: 128, maxSourceDepth: 32,
});

const DEFAULT_INTERPRETER_SPECIFIER = "just-bash/browser";

const DEVICES = new Set(["/dev/full", "/dev/null", "/dev/stderr", "/dev/stdout"]);

// just-bash@3.4.0's built-in command registry (without the conditional curl
// command). The parity test compares this manifest with Bash.commands so an
// upstream dependency change cannot silently alter the advertised tool surface.
const BUILTIN_COMMANDS = Object.freeze(
  "alias awk base64 basename bash cat chmod clear column comm cp cut date diff dirname du echo egrep env expand expr false fgrep file find fold grep gunzip gzip head help history hostname html-to-markdown join jq ln ls md5sum mkdir mv nl od paste printenv printf pwd readlink rev rg rm rmdir sed seq sh sha1sum sha256sum sleep sort split stat strings tac tail tee time timeout touch tr tree true unalias unexpand uniq wc which whoami xargs zcat".split(" "),
);

export async function justBash(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("Just Bash options must be an object");
  }
  validateWorkspace(options.filesystem);
  const executionTimeoutMs = positiveInteger(
    options.executionTimeoutMs,
    DEFAULT_EXECUTION_LIMITS.maxExecutionTimeMs,
    "executionTimeoutMs",
  );
  const maxEntries = options.maxEntries === undefined
    ? undefined : positiveInteger(options.maxEntries, undefined, "maxEntries");
  const maxOutputTokens = Math.min(
    MAX_OUTPUT_TOKENS,
    positiveInteger(options.maxOutputTokens, DEFAULT_MAX_OUTPUT_TOKENS, "maxOutputTokens"),
  );
  const shellFilesystem = new WorkspaceShellFileSystem(options.filesystem, maxEntries, options.executionLimits?.maxInputBytes ?? DEFAULT_EXECUTION_LIMITS.maxInputBytes);
  // Shared workspaces refresh before every command. Opening here duplicates
  // the first refresh and puts a remote storage listing on chat-only startup.
  if (!options.refreshFilesystemBeforeExec) await shellFilesystem.open();
  const filesystem = shellFilesystem.workspace();
  const runtime = await createJustBashRuntime({
    filesystem: shellFilesystem,
    binaryIO: options.binaryIO,
    onExecution: options.onExecution,
    lazyInitialize: options.lazyInitialize === true,
    loadInterpreter: options.loadInterpreter,
    cwd: filesystem.root,
    env: {
      HOME: filesystem.root,
      PWD: filesystem.root,
      PATH: filesystem.root,
    },
    fetch: options.fetch,
    network: options.network,
    networkMode: options.networkMode ?? (typeof options.fetch === "function"
      ? "host-fetch"
      : options.network === false || options.network === undefined
        ? undefined
        : "restricted-http"),
    customCommands: options.customCommands,
    aroundExecute: options.refreshFilesystemBeforeExec
      ? async ({ execute, signal }) => {
        signal.throwIfAborted();
        await shellFilesystem.open(signal);
        signal.throwIfAborted();
        return execute();
      }
      : undefined,
    executionTimeoutMs,
    defaultMaxOutputTokens: maxOutputTokens,
    maxOutputTokens,
    executionLimits: {
      ...options.executionLimits,
      ...(executionTimeoutMs === undefined ? {} : { maxExecutionTimeMs: executionTimeoutMs }),
      ...(maxEntries === undefined ? {} : { maxTraversalEntries: maxEntries }),
    },
  });

  return Object.freeze({ ...runtime, filesystem });
}

/**
 * Constructs the common Just Bash interpreter, execution tool, instructions, and descriptor over
 * a caller-owned Just Bash filesystem adapter. Hosts retain ownership of persistence and locking.
 */
export async function createJustBashRuntime(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("Just Bash runtime options must be an object");
  }
  if (!options.filesystem || typeof options.filesystem !== "object") {
    throw new TypeError("Just Bash runtime filesystem is required");
  }
  const cwd = normalizeRoot(requiredString(options.cwd, "cwd"));
  const executionTimeoutMs = positiveInteger(
    options.executionTimeoutMs,
    DEFAULT_EXECUTION_LIMITS.maxExecutionTimeMs,
    "executionTimeoutMs",
  );
  const defaultMaxOutputTokens = positiveInteger(
    options.defaultMaxOutputTokens,
    DEFAULT_MAX_OUTPUT_TOKENS,
    "defaultMaxOutputTokens",
  );
  const maxOutputTokens = positiveInteger(
    options.maxOutputTokens,
    defaultMaxOutputTokens,
    "maxOutputTokens",
  );
  if (defaultMaxOutputTokens > maxOutputTokens) {
    throw new RangeError("defaultMaxOutputTokens cannot exceed maxOutputTokens");
  }
  const executionLimits = Object.freeze({ ...DEFAULT_EXECUTION_LIMITS, ...options.executionLimits });
  // Do not evaluate just-bash/browser on chat-only managed turns. Initialization
  // is shared across concurrent first commands; the existing execution tail
  // still serializes refresh + command execution after initialization.
  const networkEnabled = typeof options.fetch === "function"
    || options.network !== false && options.network !== undefined;
  let bash;
  let registeredCommands;
  let initialization;
  const initialize = (signal) => {
    if (initialization) return awaitInitialization(initialization, signal);
    const attempt = (async () => {
    const { Bash, defineCommand } = await (options.loadInterpreter?.() ?? import(DEFAULT_INTERPRETER_SPECIFIER));
    if (initialization !== attempt) return undefined;
    const customCommands = typeof options.customCommands === "function"
      ? await options.customCommands({ defineCommand })
      : options.customCommands;
    if (initialization !== attempt) return undefined;
    const loaded = new Bash({
      cwd,
      env: options.env,
      fs: options.filesystem,
      ...(typeof options.fetch === "function"
        ? { fetch: options.fetch }
        : options.network === false || options.network === undefined
          ? {}
          : { network: options.network }),
      customCommands: [...createSearchCommands({ Bash }), ...customCommands ?? []],
      executionLimitProfile: "normal",
      // Allocation builders require safe integer capacities, even for tiny output.
      executionLimits: {
        ...executionLimits,
        maxOutputSize: Math.min(executionLimits.maxOutputSize, Number.MAX_SAFE_INTEGER),
        maxStringLength: Math.min(executionLimits.maxStringLength, Number.MAX_SAFE_INTEGER),
      },
    });
    // An abandoned loader must never publish into a later initialization.
    if (initialization === attempt) {
      registeredCommands = customCommands;
      bash = loaded;
    }
    return loaded;
    })();
    initialization = attempt;
    // A failed first import/build must not poison all later shell calls.
    void attempt.catch(() => { if (initialization === attempt) initialization = undefined; });
    return awaitInitialization(attempt, signal);
  };
  function awaitInitialization(attempt, signal) {
    return abortable(attempt, signal).catch((error) => {
      if (signal?.aborted && initialization === attempt) initialization = undefined;
      throw error;
    });
  }
  if (!options.lazyInitialize || typeof options.customCommands === "function") await initialize();
  const descriptor = describeRuntime({
    commands: bash ? [...bash.commands.keys()] : [
      ...BUILTIN_COMMANDS,
      ...(networkEnabled ? ["curl"] : []),
      ...(options.customCommands ?? []),
    ].map((command) => typeof command === "string" ? command : command.name),
    cwd,
    customCommands: registeredCommands ?? options.customCommands,
    executionLimits,
    networkMode: options.networkMode,
    networkEnabled,
  });
  const instructions = typeof options.instructions === "function"
    ? options.instructions(descriptor)
    : defaultInstructions(descriptor);
  let executionTail = Promise.resolve();

  const tool = namedTool("exec_command", {
    ...(options.supportsParallelToolCalls === undefined
      ? {}
      : { supportsParallelToolCalls: options.supportsParallelToolCalls }),
    description: "Run a one-shot Bash command in the persistent virtual workspace and return its output and exit code. Use cat or sed to read files, rg to search, and quoted heredocs to write multiline text. No native process, PTY, or process session.",
    parameters: EXEC_COMMAND_PARAMETERS,
    outputSchema: EXECUTION_OUTPUT_SCHEMA,
    handler(input, context) {
      const admittedAt = now();
      // Only a bounded first literal token is inspected. No parsing, argument
      // capture, hashing, or upstream logger (which includes source/output).
      const command = commandFamily(input);
      const emit = (event) => {
        try {
          const pending = options.onExecution?.(Object.freeze({ command, command_scope: "first_literal", ...event }), context);
          // Observers are observational, including an accidentally async sink.
          if (pending && typeof pending.then === "function") void Promise.resolve(pending).catch(() => {});
        } catch { /* Logging must never alter shell behavior. */ }
      };
      emit({ phase: "queued" });
      const execute = async () => {
        const startedAt = now();
        emit({ phase: "started", queue_ms: Math.max(0, startedAt - admittedAt) });
        let category = "input_validation", admissionCommand;
        try {
          const result = await executeCommand({
            initialize,
            onCategory: (value, command) => { category = value; admissionCommand = command; },
            startedAt,
            input,
            root: cwd,
            signal: context?.signal,
            executionTimeoutMs,
            defaultMaxOutputTokens,
            maxOutputTokens,
            aroundExecute: options.aroundExecute,
            filesystem: options.filesystem,
            binaryIO: options.binaryIO,
            executionLimits,
            binaryEnabled: () => !registeredCommands?.some((command) => command.name === "cat" || command.name === "sha256sum"),
            outputTruncationNotice: options.outputTruncationNotice,
            retainNoticeWithinLimit: options.retainNoticeWithinLimit,
          });
          emit({ phase: "finished", status: result.exit_code === 0 ? "success" : "error",
            exit_code: result.exit_code, category, ...(admissionCommand ? { admission_command: admissionCommand } : {}), duration_ms: Math.max(0, now() - startedAt),
            output_truncated: result.original_token_count !== undefined });
          return result;
        } catch (error) {
          emit({ phase: "finished", status: "error", exit_code: null,
            category: context?.signal?.aborted ? "cancelled" : category,
            ...(admissionCommand ? { admission_command: admissionCommand } : {}),
            duration_ms: Math.max(0, now() - startedAt), output_truncated: false });
          throw error;
        }
      };
      const result = executionTail.then(execute, execute);
      executionTail = result.then(() => undefined, () => undefined);
      return result;
    },
  });

  return Object.freeze({ bash, descriptor, instructions, tool, exec: tool.handler });
}

async function executeCommand({
  initialize,
  onCategory,
  startedAt,
  input,
  root,
  signal,
  executionTimeoutMs,
  defaultMaxOutputTokens,
  maxOutputTokens,
  aroundExecute,
  filesystem,
  binaryIO,
  executionLimits,
  binaryEnabled,
  outputTruncationNotice = OUTPUT_TRUNCATION_NOTICE,
  retainNoticeWithinLimit = true,
}) {
  if (!input || typeof input !== "object" || Array.isArray(input)) {
    throw new TypeError("exec_command input must be an object");
  }
  if (typeof input.cmd !== "string" || !input.cmd.trim()) {
    throw new TypeError("exec_command.cmd must be a non-empty string");
  }
  if (input.tty === true) throw new Error("Just Bash does not provide PTY sessions");
  if (input.sandbox_permissions === "require_escalated") {
    throw new Error("Just Bash cannot escape its virtual workspace");
  }
  if (input.shell !== undefined && input.shell !== "bash" && input.shell !== "/bin/bash") {
    throw new Error("exec_command supports only the embedded Bash interpreter");
  }
  const workdir = input.workdir === undefined
    ? root
    : resolvePath(root, root, requiredString(input.workdir, "workdir"));
  const outputTokens = Math.min(
    maxOutputTokens,
    positiveInteger(input.max_output_tokens, defaultMaxOutputTokens, "max_output_tokens"),
  );
  onCategory("exception");
  const deadline = new AbortController();
  const abort = () => deadline.abort(signal?.reason);
  signal?.addEventListener("abort", abort, { once: true });
  if (signal?.aborted) abort();
  const timeout = executionTimeoutMs === undefined ? undefined : setTimeout(
    () => deadline.abort(new Error(`exec_command exceeded ${executionTimeoutMs} milliseconds`)),
    executionTimeoutMs,
  );
  let result;
  try {
    // Import and remote metadata are asynchronous host boundaries. Cancellation
    // can release their waiter; synchronous interpreter work still needs admission.
    const bash = await initialize(deadline.signal);
    const execute = async () => {
      const binary = binaryEnabled() ? await tryExecuteBinaryCommand({
        bash, filesystem, command: input.cmd, cwd: workdir, root,
        signal: deadline.signal, binaryIO, executionLimits,
      }) : undefined;
      return binary ?? bash.exec(input.cmd, { cwd: workdir, signal: deadline.signal });
    };
    result = typeof aroundExecute === "function"
      ? await aroundExecute({ execute, signal: deadline.signal })
      : await execute();
  } catch (error) {
    if (!deadline.signal.aborted) {
      onCategory(error?.fatalSearchAdmission ? "search_admission"
        : error?.shellLimit ? "resource_limit" : "exception", error?.fatalSearchAdmission ? admissionCommandLabel(error.message) : undefined);
      throw error;
    }
    result = { stdout: "", stderr: "bash: execution aborted\n", exitCode: 124 };
  } finally {
    clearTimeout(timeout);
    signal?.removeEventListener("abort", abort);
  }
  const category = deadline.signal.aborted
    ? signal?.aborted ? "cancelled" : "timeout"
    : resultCategory(result);
  onCategory(category, category === "search_admission" ? admissionCommandLabel(result.stderr) : undefined);
  const combined = `${result.stdout}${result.stderr}`;
  const maxCharacters = outputTokens * 4;
  const truncated = combined.length > maxCharacters;
  const retainedCharacters = retainNoticeWithinLimit
    ? Math.max(0, maxCharacters - outputTruncationNotice.length)
    : maxCharacters;
  return {
    output: truncated
      ? !retainNoticeWithinLimit || maxCharacters >= outputTruncationNotice.length
        ? `${combined.slice(0, retainedCharacters)}${outputTruncationNotice}`
        : combined.slice(0, maxCharacters)
      : combined,
    wall_time_seconds: (now() - startedAt) / 1000,
    exit_code: result.exitCode,
    ...(truncated ? { original_token_count: Math.ceil(combined.length / 4) } : {}),
  };
}

// Labels are code-owned constants, never arbitrary executable names. Unknown,
// dynamic, quoted, assignment-prefixed, or long first words remain "other".
const TELEMETRY_COMMANDS = new Set([...BUILTIN_COMMANDS, "curl", "git", "gh", "ssh", "ffmpeg", "ffprobe", "pdftotext"]);
function commandFamily(input) {
  if (typeof input?.cmd !== "string") return "other";
  const match = /^\s{0,64}([a-z][a-z0-9-]{0,31})(?=\s|[;|&<>]|$)/.exec(input.cmd.slice(0, 128));
  return match && TELEMETRY_COMMANDS.has(match[1]) ? match[1] : "other";
}

// Diagnostics may identify a refused search later in a pipeline. Only this
// fixed label leaves the bounded inspection; it is best-effort attribution.
function admissionCommandLabel(diagnostic) {
  if (typeof diagnostic !== "string") return;
  for (const match of diagnostic.slice(0, 4096).matchAll(/^(?:bash: )?(rg|grep|fgrep|egrep|sed|awk): ([^\r\n]*)/gm)) {
    if (/search_admission|synchronous regex.*admission|uncertain dynamic regex admission/.test(match[2])) return match[1];
  }
}

function resultCategory(result) {
  if (result.exitCode === 0) return "none";
  // Inspection stays local and bounded; only the enum is emitted. No error
  // text or arbitrary error property is passed across the observer boundary.
  const diagnostic = typeof result.stderr === "string" ? result.stderr.slice(0, 4096) : "";
  if (/search_admission|synchronous regex.*admission|uncertain dynamic regex admission/.test(diagnostic)) return "search_admission";
  if (/limit exceeded|exceeded.*limit|resource limit|size limit|maximum.*exceeded|input ceiling|workspace exceeds/i.test(diagnostic)) return "resource_limit";
  if (result.exitCode === 124) return "timeout";
  if (result.exitCode === 127) return "command_not_found";
  if (/syntax error|parse error|unexpected token|unterminated/i.test(diagnostic)) return "syntax";
  return "command_exit";
}

function describeRuntime({
  commands,
  cwd,
  customCommands,
  executionLimits,
  networkEnabled,
  networkMode,
}) {
  const customCommandNames = [...customCommands ?? []].map(({ name }) => name);
  return Object.freeze({
    shell: "nanocodex-just-bash",
    commands: Object.freeze([...new Set(commands)].sort()),
    customCommands: Object.freeze(customCommandNames.sort()),
    cwd,
    limits: Object.freeze(Object.fromEntries(Object.entries(executionLimits).filter(
      ([, value]) => Number.isFinite(value) && value !== Number.MAX_SAFE_INTEGER,
    ))),
    network: Object.freeze({
      enabled: networkEnabled,
      mode: networkMode ?? (networkEnabled ? "http" : "disabled"),
    }),
    pty: false,
    sessions: false,
    sandboxEscalation: false,
  });
}

function defaultInstructions(descriptor) {
  const network = descriptor.network.enabled
    ? `HTTP is available through the host-owned ${descriptor.network.mode} fetch boundary.`
    : "Network commands are unavailable.";
  return `You have an in-process Bash interpreter and a persistent virtual filesystem rooted at ${descriptor.cwd}.
Use exec_command with ordinary shell commands directly:
- Read whole files with cat; read a line range with sed -n '20,80p' FILE; use head or tail for a short preview.
- Find files with rg --files and search contents with rg -n 'PATTERN' PATH. Use rg -F for literal text.
- Edit existing text with sed -i 's/old/new/g' FILE; use cp and mv for copies and renames.
- Write multiline text with a quoted heredoc (cat > FILE <<'EOF', literal body, then EOF on its own line).
  Choose a delimiter absent from the body. A quoted delimiter preserves dollar signs, backticks, and backslashes.
- Use printf when formatted output or exact bytes without a trailing newline are needed. Do not build file
  readers, line-by-line file writers, or decorative output separators out of printf/echo loops.
Use these commands without a capability probe. bat is not bundled; use cat or sed here. An attached native
Hand has its own installed commands; this command list applies only to the in-process workspace.
When the user requests an explicit shell operation that maps directly to an
available command, call exec_command immediately and once with the complete command. Do not inspect the runtime,
account, or workspace, search for another tool, or split the operation into exploratory calls before trying it.
For an ordinary clone request, use exactly gh repo clone OWNER/REPO DESTINATION or git clone URL DESTINATION.
By default these commands download and extract a source archive: all current files, without .git or history.
Use --branch for a requested revision. An explicit --depth requests a Git checkout with that history depth.
Do not add depth, filter, branch, or other flags unless the user requests them, and do not inspect a successful clone.
Only investigate after that direct command fails or when the user explicitly asks for investigation.
Available commands: ${descriptor.commands.join(", ")}. Use ${descriptor.cwd}/tmp, not /tmp, for temporary files. Commands run without a host process, container, PTY, session, or sandbox
escalation, and cannot access paths outside ${descriptor.cwd}. The shell is one-shot per call, but files persist
across calls and agent restarts. ${network} Model subscription credentials are never exposed to the shell.`;
}

class WorkspaceShellFileSystem {
  #source;
  #root;
  #maxEntries;
  #maxReadBytes;
  #entries = new Map();
  #children = new Map();
  #sortedPaths;
  #opened = false;
  #opening;

  constructor(workspace, maxEntries, maxReadBytes) {
    this.#source = workspace;
    this.#root = normalizeRoot(workspace.root);
    this.#maxEntries = maxEntries;
    this.#maxReadBytes = maxReadBytes;
    this.#set(this.#root, directoryEntry());
  }

  async open(signal) {
    if (this.#opening) return abortable(this.#opening, signal);
    const opening = this.#refresh(signal);
    this.#opening = opening;
    try {
      await abortable(opening, signal);
    } finally {
      if (this.#opening === opening) this.#opening = undefined;
    }
  }

  async #ensureOpen() {
    if (!this.#opened || this.#opening) await this.open();
  }

  async #refresh(signal) {
    const entries = await abortable(this.#source.list(".", { recursive: true, ...(this.#maxEntries === undefined ? {} : { maxEntries: this.#maxEntries }) }), signal);
    signal?.throwIfAborted();
    this.#entries.clear();
    this.#children.clear();
    this.#sortedPaths = undefined;
    this.#set(this.#root, directoryEntry());
    for (const entry of entries) {
      const path = resolvePath(this.#root, this.#root, entry.path);
      this.#addParents(path);
      this.#set(path, entry.kind === "directory"
        ? directoryEntry(entry.modifiedAt)
        : fileEntry(entry.size, entry.modifiedAt));
    }
    this.#opened = true;
  }

  workspace() {
    return Object.freeze({
      root: this.#root,
      list: (path = ".", options) => this.#source.list(
        resolvePath(this.#root, this.#root, path),
        options,
      ),
      readFile: (path) => this.#source.readFile(resolvePath(this.#root, this.#root, path)),
      writeFile: async (path, contents) => {
        await this.#ensureOpen();
        const absolute = resolvePath(this.#root, this.#root, path);
        const bytes = bytesFrom(contents);
        this.#assertCapacity(absolute);
        await this.#source.writeFile(absolute, bytes);
        this.#addParents(absolute);
        this.#set(absolute, fileEntry(bytes.byteLength));
      },
      remove: async (path, options) => {
        await this.#ensureOpen();
        const absolute = resolvePath(this.#root, this.#root, path);
        await this.#source.remove(absolute, options);
        this.#remove(absolute);
      },
      mkdir: async (path) => {
        await this.#ensureOpen();
        const absolute = resolvePath(this.#root, this.#root, path);
        this.#assertCapacity(absolute);
        await this.#source.mkdir(absolute);
        this.#addParents(absolute);
        this.#set(absolute, directoryEntry());
      },
    });
  }

  async readFile(path, options) {
    return decode(await this.readFileBuffer(path), encoding(options));
  }

  async readFileBytes(path) {
    return bytesToLatin1(await this.readFileBuffer(path));
  }

  async readFileBuffer(path) {
    const absolute = this.#resolve(path);
    if (absolute === "/dev/null") return new Uint8Array();
    const entry = this.#require(absolute);
    if (entry.kind !== "file") throw fsError("EISDIR", `${absolute} is a directory`);
    if (entry.size > this.#maxReadBytes) throw fsError("EFBIG", `file exceeds shell input ceiling (${this.#maxReadBytes} bytes); use a native Hand for larger files`);
    return this.#source.readFile(absolute);
  }

  async writeFile(path, content, options) {
    const absolute = this.#resolve(path);
    if (absolute === "/dev/null") return;
    const bytes = encode(content, encoding(options));
    this.#assertCapacity(absolute);
    await this.#source.writeFile(absolute, bytes);
    this.#addParents(absolute);
    this.#set(absolute, fileEntry(bytes.byteLength));
  }

  async appendFile(path, content, options) {
    const absolute = this.#resolve(path);
    if (absolute === "/dev/null") return;
    const suffix = encode(content, encoding(options));
    const prefix = await this.exists(absolute) ? await this.readFileBuffer(absolute) : new Uint8Array();
    const joined = new Uint8Array(prefix.byteLength + suffix.byteLength);
    joined.set(prefix);
    joined.set(suffix, prefix.byteLength);
    await this.writeFile(absolute, joined);
  }

  async exists(path) {
    try {
      const absolute = this.#resolve(path);
      return DEVICES.has(absolute) || this.#entries.has(absolute);
    } catch (error) {
      if (error?.code === "EPERM") return false;
      throw error;
    }
  }

  async stat(path) {
    const absolute = this.#resolve(path);
    if (DEVICES.has(absolute)) return statResult(fileEntry(0, 0), absolute);
    return statResult(this.#require(absolute), absolute);
  }

  lstat(path) {
    return this.stat(path);
  }

  async mkdir(path, options = {}) {
    const absolute = resolvePath(this.#root, this.#root, path);
    const existing = this.#entries.get(absolute);
    if (existing) {
      if (options.recursive && existing.kind === "directory") return;
      throw fsError("EEXIST", `${absolute} already exists`);
    }
    const parent = parentPath(absolute);
    if (!options.recursive && !this.#entries.has(parent)) {
      throw fsError("ENOENT", `parent directory ${parent} does not exist`);
    }
    this.#assertCapacity(absolute);
    await this.#source.mkdir(absolute);
    this.#addParents(absolute);
    this.#set(absolute, directoryEntry());
  }

  async readdir(path) {
    const absolute = resolvePath(this.#root, this.#root, path);
    const entry = this.#require(absolute);
    if (entry.kind !== "directory") throw fsError("ENOTDIR", `${absolute} is not a directory`);
    return [...this.#children.get(absolute) ?? []].sort();
  }

  async readdirWithFileTypes(path) {
    const absolute = resolvePath(this.#root, this.#root, path);
    return Promise.all((await this.readdir(absolute)).map(async (name) => {
      const entry = this.#require(`${absolute}/${name}`);
      return {
        name,
        isFile: entry.kind === "file",
        isDirectory: entry.kind === "directory",
        isSymbolicLink: false,
      };
    }));
  }

  async rm(path, options = {}) {
    const absolute = resolvePath(this.#root, this.#root, path);
    if (absolute === this.#root) throw fsError("EPERM", "cannot remove the workspace root");
    const entry = this.#entries.get(absolute);
    if (!entry) {
      if (options.force) return;
      throw fsError("ENOENT", `${absolute} does not exist`);
    }
    if (entry.kind === "directory" && !options.recursive && this.#hasChildren(absolute)) {
      throw fsError("ENOTEMPTY", `${absolute} is not empty`);
    }
    await this.#source.remove(absolute, { recursive: options.recursive === true });
    this.#remove(absolute);
  }

  async cp(sourcePath, destinationPath, options = {}) {
    const source = resolvePath(this.#root, this.#root, sourcePath);
    const destination = resolvePath(this.#root, this.#root, destinationPath);
    const entry = this.#require(source);
    if (source === destination || (entry.kind === "directory" && destination.startsWith(`${source}/`))) {
      throw fsError("EINVAL", "cannot copy a path onto itself or into its own subtree");
    }
    if (entry.kind === "directory") {
      if (!options.recursive) throw fsError("EISDIR", "copying a directory requires recursive mode");
      await this.mkdir(destination, { recursive: true });
      for (const name of await this.readdir(source)) {
        await this.cp(`${source}/${name}`, `${destination}/${name}`, options);
      }
      return;
    }
    await this.writeFile(destination, await this.readFileBuffer(source));
  }

  async mv(source, destination) {
    await this.cp(source, destination, { recursive: true });
    await this.rm(source, { recursive: true });
  }

  resolvePath(base, path) {
    return resolveShellPath(this.#root, base, path);
  }

  getAllPaths() {
    this.#sortedPaths ??= [...this.#entries.keys()].sort();
    return this.#sortedPaths.slice();
  }

  async chmod(path) {
    await this.stat(path);
  }

  async symlink() {
    throw fsError("ENOSYS", "the mounted workspace does not support symbolic links");
  }

  async link() {
    throw fsError("ENOSYS", "the mounted workspace does not support hard links");
  }

  async readlink() {
    throw fsError("ENOSYS", "the mounted workspace does not support symbolic links");
  }

  async realpath(path) {
    const absolute = resolvePath(this.#root, this.#root, path);
    await this.stat(absolute);
    return absolute;
  }

  async utimes(path) {
    await this.stat(path);
  }

  #resolve(path) {
    if (DEVICES.has(path)) return path;
    return resolvePath(this.#root, this.#root, path);
  }

  #require(path) {
    const entry = this.#entries.get(path);
    if (!entry) throw fsError("ENOENT", `${path} does not exist`);
    return entry;
  }

  #set(path, entry) {
    const exists = this.#entries.has(path);
    if (this.#maxEntries !== undefined && !exists && this.#entries.size - 1 >= this.#maxEntries) {
      throw fsError("EFBIG", `workspace exceeds ${this.#maxEntries} entries`);
    }
    this.#entries.set(path, entry);
    if (entry.kind === "directory") {
      if (!this.#children.has(path)) this.#children.set(path, new Set());
    } else {
      this.#children.delete(path);
    }
    if (!exists && path !== this.#root) {
      const parent = parentPath(path);
      if (!this.#children.has(parent)) this.#children.set(parent, new Set());
      this.#children.get(parent).add(path.slice(parent.length + 1));
    }
    this.#sortedPaths = undefined;
  }

  #assertCapacity(path) {
    if (this.#maxEntries === undefined) return;
    let additions = this.#entries.has(path) ? 0 : 1;
    const relative = path.slice(this.#root.length + 1);
    let current = this.#root;
    for (const segment of relative.split("/").slice(0, -1)) {
      current += `/${segment}`;
      if (!this.#entries.has(current)) additions += 1;
    }
    if (additions > this.#maxEntries - (this.#entries.size - 1)) {
      throw fsError("EFBIG", `workspace exceeds ${this.#maxEntries} entries`);
    }
  }

  #addParents(path) {
    const relative = path.slice(this.#root.length + 1);
    if (!relative) return;
    let current = this.#root;
    for (const segment of relative.split("/").slice(0, -1)) {
      current += `/${segment}`;
      if (!this.#entries.has(current)) this.#set(current, directoryEntry());
    }
  }

  #remove(path) {
    const removed = [];
    for (const candidate of this.#entries.keys()) {
      if (candidate === path || candidate.startsWith(`${path}/`)) removed.push(candidate);
    }
    for (const candidate of removed) {
      this.#entries.delete(candidate);
      this.#children.delete(candidate);
      const parent = parentPath(candidate);
      this.#children.get(parent)?.delete(candidate.slice(parent.length + 1));
    }
    this.#sortedPaths = undefined;
  }

  #hasChildren(path) {
    return (this.#children.get(path)?.size ?? 0) > 0;
  }
}

function directoryEntry(modifiedAt = Date.now()) {
  return { kind: "directory", modifiedAt, size: 0 };
}

function fileEntry(size = 0, modifiedAt = Date.now()) {
  return { kind: "file", modifiedAt, size: size ?? 0 };
}

function statResult(entry, path) {
  return {
    isFile: entry.kind === "file",
    isDirectory: entry.kind === "directory",
    isSymbolicLink: false,
    mode: entry.kind === "directory" ? 0o755 : 0o644,
    size: entry.size ?? 0,
    mtime: new Date(entry.modifiedAt ?? 0),
    identity: `workspace:${path}`,
  };
}

function resolvePath(root, base, path) {
  if (typeof path !== "string" || path.includes("\0")) throw fsError("EINVAL", "invalid path");
  const safeBase = normalizeRoot(base);
  if (safeBase !== root && !safeBase.startsWith(`${root}/`)) {
    throw fsError("EPERM", `working directory escapes ${root}`);
  }
  const source = path.startsWith("/") ? path : `${safeBase}/${path}`;
  const segments = [];
  for (const segment of source.replaceAll("\\", "/").split("/")) {
    if (!segment || segment === ".") continue;
    if (segment === "..") segments.pop();
    else segments.push(segment);
  }
  const absolute = `/${segments.join("/")}`;
  if (absolute !== root && !absolute.startsWith(`${root}/`)) {
    throw fsError("EPERM", `path escapes ${root}`);
  }
  return absolute;
}

function resolveShellPath(root, base, path) {
  if (DEVICES.has(path)) return path;
  if (path.startsWith("/")) return resolvePath(root, base, path);
  const safeBase = normalizeRoot(base);
  if (safeBase !== root && !safeBase.startsWith(`${root}/`)) {
    throw fsError("EPERM", `working directory escapes ${root}`);
  }
  const rootSegments = root.split("/").filter(Boolean);
  const segments = safeBase.split("/").filter(Boolean);
  for (const segment of path.replaceAll("\\", "/").split("/")) {
    if (!segment || segment === ".") continue;
    if (segment === "..") {
      if (segments.length > rootSegments.length) segments.pop();
    } else {
      segments.push(segment);
    }
  }
  return `/${segments.join("/")}`;
}

function normalizeRoot(root) {
  if (typeof root !== "string" || !root.startsWith("/")) {
    throw new TypeError("workspace root must be an absolute path");
  }
  const normalized = `/${root.split("/").filter((segment) => segment && segment !== ".").join("/")}`;
  if (normalized === "/" || normalized.includes("/../") || normalized.endsWith("/..")) {
    throw new TypeError("workspace root must be a bounded absolute path");
  }
  return normalized;
}

function parentPath(path) {
  return path.slice(0, path.lastIndexOf("/")) || "/";
}

function encoding(options) {
  return typeof options === "string" ? options : options?.encoding ?? "utf8";
}

function encode(content, selectedEncoding) {
  if (content instanceof Uint8Array) return content;
  if (selectedEncoding === "base64") {
    return Uint8Array.from(atob(content), (character) => character.charCodeAt(0));
  }
  if (selectedEncoding === "hex") {
    if (content.length % 2 !== 0 || !/^[a-f0-9]*$/i.test(content)) {
      throw fsError("EINVAL", "invalid hex input");
    }
    return Uint8Array.from(content.match(/../g) ?? [], (pair) => Number.parseInt(pair, 16));
  }
  if (["binary", "latin1", "ascii"].includes(selectedEncoding)) {
    return Uint8Array.from(content, (character) => character.charCodeAt(0) & 0xff);
  }
  return encoder.encode(content);
}

function decode(bytes, selectedEncoding) {
  if (selectedEncoding === "base64") return btoa(bytesToLatin1(bytes));
  if (selectedEncoding === "hex") {
    return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
  }
  if (selectedEncoding === "binary" || selectedEncoding === "latin1") return bytesToLatin1(bytes);
  if (selectedEncoding === "ascii") {
    return bytesToLatin1(Uint8Array.from(bytes, (byte) => byte & 0x7f));
  }
  return decoder.decode(bytes);
}

function bytesFrom(value) {
  if (typeof value === "string") return encoder.encode(value);
  if (value instanceof Uint8Array) return value;
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  throw new TypeError("workspace contents must be a string or byte array");
}

function bytesToLatin1(bytes) {
  let output = "";
  for (let offset = 0; offset < bytes.length; offset += 32_768) {
    output += String.fromCharCode(...bytes.subarray(offset, offset + 32_768));
  }
  return output;
}

function validateWorkspace(workspace) {
  if (!workspace || typeof workspace !== "object" || typeof workspace.root !== "string") {
    throw new TypeError("Just Bash requires a workspace handle");
  }
  for (const method of ["list", "readFile", "writeFile", "remove", "mkdir"]) {
    if (typeof workspace[method] !== "function") {
      throw new TypeError(`workspace handle requires ${method}()`);
    }
  }
}

function positiveInteger(value, fallback, name) {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || value <= 0) throw new TypeError(`${name} must be positive`);
  return value;
}

function requiredString(value, name) {
  if (typeof value !== "string" || !value.trim()) throw new TypeError(`${name} must be non-empty`);
  return value;
}

function fsError(code, message) {
  // just-bash filesystem safety helpers inspect errno text, not only .code.
  // Preserve both so a missing destination is provable, without treating
  // permission/unsupported errors as absence or bypassing identity guards.
  return Object.assign(new Error(`${code}: ${message}`), { code });
}

function now() {
  return globalThis.performance?.now?.() ?? Date.now();
}

// Only asynchronous host operations use this race. It cannot preempt CPU work.
function abortable(promise, signal) {
  if (!signal) return promise;
  signal.throwIfAborted();
  return new Promise((resolve, reject) => {
    const aborted = () => reject(signal.reason ?? new Error("execution aborted"));
    signal.addEventListener("abort", aborted, { once: true });
    Promise.resolve(promise).then(resolve, reject).finally(() => signal.removeEventListener("abort", aborted));
  });
}
