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

// The host owns resources and cancellation. Interpreter ceilings must not
// turn otherwise valid commands, repositories, or data files into errors.
const UNLIMITED_EXECUTION_LIMITS = Object.freeze({
  maxSourceBytes: Infinity,
  maxExecDepth: Infinity,
  maxCallDepth: Infinity,
  maxCommandCount: Infinity,
  maxLoopIterations: Infinity,
  maxAwkIterations: Infinity,
  maxSedIterations: Infinity,
  maxJqIterations: Infinity,
  maxQueryTokens: Infinity,
  maxQueryDepth: Infinity,
  maxQueryElements: Infinity,
  maxAwkParserTokens: Infinity,
  maxAwkParserDepth: Infinity,
  maxAwkParserOperations: Infinity,
  maxCsvRows: Infinity,
  maxCsvCells: Infinity,
  maxWorkUnits: Infinity,
  maxTraversalEntries: Infinity,
  maxTraversalDepth: Infinity,
  maxTraversalWork: Infinity,
  maxLiveBytes: Infinity,
  maxInputBytes: Infinity,
  maxFileSystemBytes: Infinity,
  maxDatabaseBytes: Infinity,
  maxDatabaseResultBytes: Infinity,
  maxArchiveBytes: Infinity,
  maxArchiveCompressedBytes: Infinity,
  maxArchiveEntryBytes: Infinity,
  maxArchiveEntries: Infinity,
  maxWorkerMessageBytes: Infinity,
  maxExecutionTimeMs: Infinity,
  maxSqliteTimeoutMs: Infinity,
  maxPythonTimeoutMs: Infinity,
  maxJsTimeoutMs: Infinity,
  maxGlobOperations: Infinity,
  maxStringLength: Infinity,
  maxArrayElements: Infinity,
  maxHeredocSize: Infinity,
  maxSubstitutionDepth: Infinity,
  maxBraceExpansionResults: Infinity,
  maxOutputSize: Infinity,
  maxFileDescriptors: Infinity,
  maxSourceDepth: Infinity,
});

const DEVICES = new Set(["/dev/full", "/dev/null", "/dev/stderr", "/dev/stdout"]);

export async function justBash(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("Just Bash options must be an object");
  }
  validateWorkspace(options.filesystem);
  const executionTimeoutMs = positiveInteger(
    options.executionTimeoutMs,
    undefined,
    "executionTimeoutMs",
  );
  const maxEntries = options.maxEntries === undefined
    ? undefined : positiveInteger(options.maxEntries, undefined, "maxEntries");
  const maxOutputTokens = Math.min(
    MAX_OUTPUT_TOKENS,
    positiveInteger(options.maxOutputTokens, DEFAULT_MAX_OUTPUT_TOKENS, "maxOutputTokens"),
  );
  const shellFilesystem = new WorkspaceShellFileSystem(options.filesystem, maxEntries);
  await shellFilesystem.open();
  const filesystem = shellFilesystem.workspace();
  const runtime = await createJustBashRuntime({
    filesystem: shellFilesystem,
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
        await shellFilesystem.open();
        signal.throwIfAborted();
        return execute();
      }
      : undefined,
    executionTimeoutMs,
    defaultMaxOutputTokens: maxOutputTokens,
    maxOutputTokens,
    executionLimits: {
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
    undefined,
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
  const executionLimits = Object.freeze({ ...UNLIMITED_EXECUTION_LIMITS, ...options.executionLimits });
  const { Bash, defineCommand } = await import("just-bash/browser");
  const customCommands = typeof options.customCommands === "function"
    ? await options.customCommands({ defineCommand })
    : options.customCommands;
  const bash = new Bash({
    cwd,
    env: options.env,
    fs: options.filesystem,
    ...(typeof options.fetch === "function"
      ? { fetch: options.fetch }
      : options.network === false || options.network === undefined
        ? {}
        : { network: options.network }),
    ...(customCommands === undefined ? {} : { customCommands: [...customCommands] }),
    executionLimitProfile: "normal",
    executionLimits,
  });
  const descriptor = describeRuntime({
    bash,
    cwd,
    customCommands,
    executionLimits,
    networkMode: options.networkMode,
    networkEnabled: typeof options.fetch === "function" || options.network !== false && options.network !== undefined,
  });
  const instructions = typeof options.instructions === "function"
    ? options.instructions(descriptor)
    : defaultInstructions(descriptor);
  let executionTail = Promise.resolve();

  const tool = namedTool("exec_command", {
    ...(options.supportsParallelToolCalls === undefined
      ? {}
      : { supportsParallelToolCalls: options.supportsParallelToolCalls }),
    description: "Runs a shell command, returning output or a session ID for ongoing interaction.",
    parameters: EXEC_COMMAND_PARAMETERS,
    outputSchema: EXECUTION_OUTPUT_SCHEMA,
    handler(input, context) {
      const execute = () => executeCommand({
        bash,
        input,
        root: cwd,
        signal: context?.signal,
        executionTimeoutMs,
        defaultMaxOutputTokens,
        maxOutputTokens,
        aroundExecute: options.aroundExecute,
        outputTruncationNotice: options.outputTruncationNotice,
        retainNoticeWithinLimit: options.retainNoticeWithinLimit,
      });
      const result = executionTail.then(execute, execute);
      executionTail = result.then(() => undefined, () => undefined);
      return result;
    },
  });

  return Object.freeze({ bash, descriptor, instructions, tool, exec: tool.handler });
}

async function executeCommand({
  bash,
  input,
  root,
  signal,
  executionTimeoutMs,
  defaultMaxOutputTokens,
  maxOutputTokens,
  aroundExecute,
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
  const deadline = new AbortController();
  const abort = () => deadline.abort(signal?.reason);
  signal?.addEventListener("abort", abort, { once: true });
  if (signal?.aborted) abort();
  const timeout = executionTimeoutMs === undefined ? undefined : setTimeout(
    () => deadline.abort(new Error(`exec_command exceeded ${executionTimeoutMs} milliseconds`)),
    executionTimeoutMs,
  );
  const startedAt = now();
  let result;
  try {
    const execute = () => bash.exec(input.cmd, { cwd: workdir, signal: deadline.signal });
    result = typeof aroundExecute === "function"
      ? await aroundExecute({ execute, signal: deadline.signal })
      : await execute();
  } finally {
    clearTimeout(timeout);
    signal?.removeEventListener("abort", abort);
  }
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

function describeRuntime({
  bash,
  cwd,
  customCommands,
  executionLimits,
  networkEnabled,
  networkMode,
}) {
  const customCommandNames = [...customCommands ?? []].map(({ name }) => name);
  return Object.freeze({
    shell: "nanocodex-just-bash",
    commands: Object.freeze([...bash.commands.keys()].sort()),
    customCommands: Object.freeze(customCommandNames.sort()),
    cwd,
    limits: Object.freeze(Object.fromEntries(Object.entries(executionLimits).filter(([, value]) => Number.isFinite(value)))),
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
Use exec_command for shell work. When the user requests an explicit shell operation that maps directly to an
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
  #entries = new Map();
  #sortedPaths;

  constructor(workspace, maxEntries) {
    this.#source = workspace;
    this.#root = normalizeRoot(workspace.root);
    this.#maxEntries = maxEntries;
  }

  async open() {
    const entries = await this.#source.list(".", { recursive: true, ...(this.#maxEntries === undefined ? {} : { maxEntries: this.#maxEntries }) });
    this.#entries.clear();
    this.#sortedPaths = undefined;
    this.#entries.set(this.#root, directoryEntry());
    for (const entry of entries) {
      const path = resolvePath(this.#root, this.#root, entry.path);
      this.#addParents(path);
      this.#set(path, entry.kind === "directory"
        ? directoryEntry(entry.modifiedAt)
        : fileEntry(entry.size, entry.modifiedAt));
    }
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
        const absolute = resolvePath(this.#root, this.#root, path);
        const bytes = bytesFrom(contents);
        this.#assertCapacity(absolute);
        await this.#source.writeFile(absolute, bytes);
        this.#addParents(absolute);
        this.#set(absolute, fileEntry(bytes.byteLength));
      },
      remove: async (path, options) => {
        const absolute = resolvePath(this.#root, this.#root, path);
        await this.#source.remove(absolute, options);
        this.#remove(absolute);
      },
      mkdir: async (path) => {
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
    const prefix = `${absolute}/`;
    const names = new Set();
    for (const candidate of this.#entries.keys()) {
      if (!candidate.startsWith(prefix)) continue;
      const remainder = candidate.slice(prefix.length);
      if (remainder && !remainder.includes("/")) names.add(remainder);
    }
    return [...names].sort();
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
    return resolvePath(this.#root, base, path);
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
    if (this.#maxEntries !== undefined && !this.#entries.has(path) && this.#entries.size - 1 >= this.#maxEntries) {
      throw fsError("EFBIG", `workspace exceeds ${this.#maxEntries} entries`);
    }
    this.#entries.set(path, entry);
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
    for (const candidate of this.#entries.keys()) {
      if (candidate === path || candidate.startsWith(`${path}/`)) this.#entries.delete(candidate);
    }
    this.#sortedPaths = undefined;
  }

  #hasChildren(path) {
    for (const candidate of this.#entries.keys()) {
      if (candidate.startsWith(`${path}/`)) return true;
    }
    return false;
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
  return Object.assign(new Error(message), { code });
}

function now() {
  return globalThis.performance?.now?.() ?? Date.now();
}
