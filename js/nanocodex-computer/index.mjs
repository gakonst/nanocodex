import { execFile, spawn } from "node:child_process";
import { access, stat } from "node:fs/promises";
import { constants, readFileSync } from "node:fs";
import { isAbsolute, join, resolve } from "node:path";
import { homedir } from "node:os";
import { promisify } from "node:util";
import { namedTool } from "nanocodex-tools/named-tool";
import { toolResult } from "nanocodex-tools/runtime/code-runtime";

const runSetup = promisify(execFile);
const preparations = new Map();
const interruptedGuidance = "Upstream/native input may still be running and effects are uncertain; do not replay uncertain input. Call cua_repl.js_reset, then inspect the surface before continuing. Reset does not prove earlier input stopped.";
const supportsManagedComputer = () => ["darwin", "win32"].includes(process.platform);
const managedRoot = () => join(process.env.NANOCODEX_DIR || join(process.env.HOME || process.env.USERPROFILE || homedir(), ".nanocodex"), "runtimes", "openai-cua");
function managedProvider() {
  if (!supportsManagedComputer()) return undefined;
  let source;
  try { source = readFileSync(join(managedRoot(), "provider.json"), "utf8"); }
  catch (error) { if (error.code === "ENOENT") return undefined; throw error; }
  if (source.length > 65536) throw new Error("Invalid managed CUA receipt; run nanocodex2 computer setup");
  const value = JSON.parse(source);
  if (value?.status !== "installed" || value.transport !== "mcp" || typeof value.executable !== "string" || !isAbsolute(value.executable)
    || !Array.isArray(value.args) || !value.args.every(arg => typeof arg === "string")
    || !value.environment || typeof value.environment !== "object" || Array.isArray(value.environment) || !Object.values(value.environment).every(v => typeof v === "string")) {
    throw new Error("Invalid managed CUA receipt; run nanocodex2 computer setup");
  }
  return value;
}
const managedComputer = () => managedProvider()?.executable;
function managedOptions(options) {
  if (process.env.NANOCODEX_COMPUTER) return options;
  const managed = managedProvider();
  if (!managed || resolve(options.executable) !== resolve(managed.executable)) return options;
  return { ...options, args: options.args ?? managed.args, environment: { ...managed.environment, ...options.environment } };
}
async function executableExists(path) {
  try { if (!path) return false; await access(path, constants.X_OK); return (await stat(path)).isFile(); } catch { return false; }
}

/** Select the installed helper’s current host once per process and install root. */
export async function ensureComputer({ binary } = {}) {
  if (process.env.NANOCODEX_COMPUTER || !supportsManagedComputer()) return discoverComputer({ binary });
  const retry = "Run nanocodex2 computer setup to retry, or set NANOCODEX_COMPUTER to an explicit provider (off disables CUA).";
  if (!binary) {
    const executable = managedComputer();
    if (await executableExists(executable)) return executable;
    throw new Error(`OpenAI CUA setup requires the installed Nanocodex native helper. Reinstall Nanocodex. ${retry}`);
  }
  const identity = JSON.stringify([resolve(binary), resolve(managedRoot())]);
  if (preparations.has(identity)) return preparations.get(identity);
  const preparation = (async () => {
    try {
      // Setup needs the selected install root, not account/model credentials.
      const env = Object.fromEntries(["PATH", "HOME", "USERPROFILE", "LOCALAPPDATA", "APPDATA", "TMPDIR", "TEMP", "SystemRoot", "NANOCODEX_DIR", "HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY", "NO_PROXY"]
        .filter(key => process.env[key] !== undefined).map(key => [key, process.env[key]]));
      const { stdout } = await runSetup(binary, ["computer", "setup"], { env, windowsHide: true, timeout: 600_000, maxBuffer: 1024 * 1024 });
      const receipt = JSON.parse(stdout);
      const installed = managedComputer();
      if (receipt.status !== "installed" || receipt.transport !== "mcp" || typeof receipt.executable !== "string"
        || !installed || resolve(receipt.executable) !== resolve(installed) || !await executableExists(installed)) {
        throw new Error(receipt.status === "unsupported" ? "The native helper does not support OpenAI CUA on this platform." : "The native helper did not publish the managed OpenAI CUA executable.");
      }
      return installed;
    } catch (error) {
      throw new Error(`OpenAI CUA setup failed: ${String(error.message).slice(0, 200)} ${retry}`, { cause: error });
    }
  })();
  preparations.set(identity, preparation);
  try { return await preparation; } catch (error) { preparations.delete(identity); throw error; }
}

/** Read-only discovery of trusted installed providers; no app or browser starts. */
export async function discoverComputer({ binary } = {}) {
  const explicit = process.env.NANOCODEX_COMPUTER;
  if (["off", "none", "0"].includes(explicit)) return undefined;
  if (explicit) return explicit;
  if (supportsManagedComputer() && await executableExists(managedComputer())) return managedComputer();
}

/** Discover the provider's actual MCP declarations before publishing tools. */
export async function connectComputerTools(options) {
  if (!options?.executable) throw new TypeError("A trusted CUA executable is required");
  options = managedOptions(options);
  const providerProcess = new ComputerProcess(options.executable, options.args ?? [], options.environment ?? {});
  try {
    const definitions = await providerProcess.start();
    return createComputerTools({ ...options, definitions });
  } finally { providerProcess.close(); }
}

/** A native attachment owns the executable/configuration, each conversation a JS scope. */
export function createComputerTools(options) {
  const { executable, args = [], environment = {}, definitions } = managedOptions(options);
  if (!executable) throw new TypeError("A trusted CUA executable is required");
  if (definitions === undefined) throw new TypeError("MCP providers require connectComputerTools discovery or a trusted catalog");
  const catalog = validateCatalog(definitions);
  const sessions = new Map();
  let disposed = false;
  const releaseSession = id => {
    const session = sessions.get(id);
    sessions.delete(id);
    session?.lifetime.abort(new Error("CUA conversation was released"));
    session?.process?.close();
  };
  const close = async () => { disposed = true; for (const id of sessions.keys()) releaseSession(id); };
  const invoke = (name, input, context) => {
    const id = context.sessionId;
    if (!id) throw new Error("CUA requires a conversation identity");
    if (disposed) throw new Error("CUA attachment is closed");
    if (!sessions.has(id)) {
      sessions.set(id, {
        lifetime: new AbortController(), process: undefined, interrupted: false,
        tail: Promise.resolve(),
      });
    }
    const session = sessions.get(id);
    // Tool arguments (including timeout_ms) belong exclusively to the provider.
    // Queueing and transport must not consume or duplicate its execution budget.
    const signal = AbortSignal.any([session.lifetime.signal, ...(context.signal ? [context.signal] : [])]);
    let dispatched = false;
    const cancellationReason = () => dispatched
      ? new Error(`CUA call cancelled after dispatch; native effects may continue and completion is unknown. ${interruptedGuidance}`, { cause: signal.reason })
      : signal.reason;
    const run = async () => {
      signal.throwIfAborted();
      if (disposed) throw new Error("CUA attachment is closed");
      if (sessions.get(id) !== session) throw new Error("CUA conversation was released");
      if (session.interrupted && name !== "js_reset") throw new Error(`CUA session interrupted. ${interruptedGuidance}`);
      const abort = () => { session.process?.close(cancellationReason()); session.process = undefined; };
      signal.addEventListener("abort", abort, { once: true });
      try {
        if (!session.process) {
          signal.throwIfAborted();
          if (disposed) throw new Error("CUA attachment is closed");
          session.process = new ComputerProcess(executable, args, environment);
          const discovered = await session.process.start();
          const matches = canonical(discovered) === canonical(catalog);
          if (!matches) throw new Error("CUA provider catalog changed; reconnect the attachment before invoking it");
        }
        signal.throwIfAborted();
        dispatched = true;
        const result = await session.process.rpc("tools/call", {
          name, arguments: input,
          _meta: { "x-codex-turn-metadata": {
            session_id: id,
            ...(context.turnId == null ? {} : { turn_id: context.turnId }),
            thread_id: id, call_id: context.callId, model: context.model,
          } },
        });
        signal.throwIfAborted();
        const content = outputContent(result);
        if (name === "js_reset" && result.isError !== true) session.interrupted = false;
        return toolResult(content, result, { value: result, success: result.isError !== true, metadata: result._meta });
      } catch (error) {
        session.process?.close(); session.process = undefined;
        if (dispatched) {
          session.interrupted = true;
          throw new Error(`CUA call interrupted after dispatch. ${interruptedGuidance}`, { cause: error });
        }
        throw error;
      } finally {
        signal.removeEventListener("abort", abort);
      }
    };
    // Provider calls are ordered, but every conversation owns an independent
    // chain and process. Long work in one conversation never blocks another.
    const result = session.tail.then(run);
    session.tail = result.catch(() => {});
    // A queued cancellation reaches the caller immediately. The queued run
    // still checks the signal and session identity before touching its process.
    return interruptible(result, signal, cancellationReason);
  };
  const allTools = catalog.map(tool => namedTool(`mcp__cua_repl__${tool.name}`, {
      description: tool.description ?? "", parameters: tool.inputSchema,
      ...(tool.outputSchema === undefined ? {} : { outputSchema: tool.outputSchema }),
      providerDefinition: tool,
      supportsParallelToolCalls: true,
      handler: (input, context) => invoke(tool.name, input, context), releaseSession, dispose: close,
    }));
  return Object.freeze({
    close,
    definitions: catalog,
    tools: allTools.filter((_, index) => modelVisible(catalog[index])),
    tool: name => allTools.find(tool => tool.name === name || tool.name === `mcp__cua_repl__${name}`),
  });
}

function interruptible(result, signal, cancellationReason) {
  return new Promise((resolve, reject) => {
    const abort = () => reject(cancellationReason());
    if (signal.aborted) abort();
    else signal.addEventListener("abort", abort, { once: true });
    result.then(resolve, reject).finally(() => signal.removeEventListener("abort", abort));
  });
}

class ComputerProcess {
  constructor(executable, args, environment) {
    const env = Object.fromEntries(["PATH", "HOME", "USER", "LOGNAME", "TMPDIR", "TEMP", "SystemRoot", "LOCALAPPDATA", "LANG", "SKY_ENABLE_AUDIO"]
      .filter(key => process.env[key] !== undefined).map(key => [key, process.env[key]]));
    this.child = spawn(executable, args, { env: { ...env, ...environment }, stdio: ["pipe", "pipe", "ignore"], windowsHide: true });
    this.pending = new Map(); this.sequence = 0; this.lines = new LineBuffer();
    this.child.stdout.on("data", chunk => {
      try {
        this.lines.push(chunk);
        for (let line; (line = this.lines.shift()) !== undefined;) {
          const value = JSON.parse(line.toString("utf8"));
          if (value.method) {
            if (value.id !== undefined) void this.serverRequest(value).catch(error => this.close(error));
            continue;
          }
          const pending = this.pending.get(value.id);
          if (!pending) throw new Error("CUA response belongs to an unknown call");
          this.pending.delete(value.id);
          if (value.error) pending.reject(new Error(value.error.message ?? "CUA runtime error"));
          else pending.resolve(value.result);
        }
      } catch (error) { this.close(error); }
    });
    this.child.once("error", error => this.close(error));
    this.child.once("exit", () => this.close(new Error("CUA runtime exited")));
    this.child.stdin.on("error", error => this.close(error));
  }
  async serverRequest(request) {
    await this.send({ jsonrpc: "2.0", id: request.id, error: { code: -32601, message: "Method not found" } });
  }
  async start() {
    // Bound only provider startup/catalog discovery, never tools/call or queue wait.
    const timer = setTimeout(() => this.close(new Error("CUA provider startup/discovery timed out after 120000 ms")), 120_000);
    timer.unref();
    try {
      await this.initialize();
      return await this.discover();
    } finally { clearTimeout(timer); }
  }
  async initialize() {
    await this.rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "nanocodex-computer", version: "0.1.0" } });
    await this.send({ jsonrpc: "2.0", method: "notifications/initialized" });
  }
  async discover() {
    const tools = [], cursors = new Set();
    let cursor;
    do {
      const page = await this.rpc("tools/list", cursor ? { cursor } : {});
      if (!Array.isArray(page.tools)) throw new Error("CUA provider did not return an MCP tools/list catalog");
      tools.push(...page.tools);
      cursor = page.nextCursor;
      if (cursor !== undefined && (typeof cursor !== "string" || !cursor || cursors.has(cursor))) throw new Error("CUA provider returned an invalid or repeated tools/list cursor");
      cursors.add(cursor);
    } while (cursor !== undefined);
    return validateCatalog(tools);
  }

  send(value) {
    if (this.closed) throw this.closed;
    const data = JSON.stringify(value) + "\n";
    return new Promise((resolve, reject) => {
      this.child.stdin.write(data, error => error ? reject(error) : resolve());
    });
  }
  async rpc(method, params) {
    const id = ++this.sequence;
    const response = new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
    });
    try { await this.send({ jsonrpc: "2.0", id, method, params }); }
    catch (error) { this.pending.delete(id); throw error; }
    return response;
  }
  close(error = new Error("CUA session stopped")) {
    if (this.closed) return;
    this.closed = error;
    this.child.kill("SIGKILL");
    this.child.stdin.destroy(); this.child.stdout.destroy();
    for (const pending of this.pending.values()) pending.reject(error);
    this.pending.clear(); this.lines.clear();
  }
}

// stdout can carry multi-megabyte screenshots or text in one JSONL record.
// Keep incoming chunks as a queue and copy each completed record exactly once;
// repeatedly concatenating the full prefix makes fragmented large results O(n²).
class LineBuffer {
  chunks = [];
  push(chunk) { if (chunk.length) this.chunks.push(chunk); }
  shift() {
    let length = 0;
    for (let index = 0; index < this.chunks.length; index++) {
      const chunk = this.chunks[index];
      const newline = chunk.indexOf(10);
      if (newline === -1) { length += chunk.length; continue; }
      const line = Buffer.allocUnsafe(length + newline);
      let offset = 0;
      for (let part = 0; part < index; part++) {
        this.chunks[part].copy(line, offset);
        offset += this.chunks[part].length;
      }
      chunk.copy(line, offset, 0, newline);
      const remainder = chunk.subarray(newline + 1);
      this.chunks = [
        ...(remainder.length ? [remainder] : []),
        ...this.chunks.slice(index + 1),
      ];
      return line;
    }
  }
  clear() { this.chunks = []; }
}

export function outputContent(result) {
  if (!Array.isArray(result?.content)) throw new TypeError("CUA result requires content");
  return result.content.map(item => {
    if (item.type === "text" && typeof item.text === "string") return { type: "input_text", text: item.text };
    if (item.type === "image" && typeof item.mimeType === "string" && typeof item.data === "string") {
      return { type: "input_image", image_url: `data:${item.mimeType};base64,${item.data}`, detail: "original" };
    }
    if (item.type === "audio" && typeof item.mimeType === "string" && typeof item.data === "string") {
      return { type: "input_audio", audio_url: `data:${item.mimeType};base64,${item.data}` };
    }
    // The model transport has no native MCP resource block. Preserve its full
    // JSON as text; the untouched provider result is also available as value.
    return { type: "input_text", text: JSON.stringify(item) };
  });
}

function canonical(value) {
  return JSON.stringify(value, function (_key, item) {
    return item && typeof item === "object" && !Array.isArray(item)
      ? Object.fromEntries(Object.keys(item).sort().map(key => [key, item[key]])) : item;
  });
}

function validateCatalog(tools) {
  const names = new Set();
  for (const tool of tools) {
    if (typeof tool?.name !== "string" || !tool.name || names.has(tool.name)) throw new Error("CUA provider tool names must be non-empty and unique");
    if (tool.description !== undefined && typeof tool.description !== "string") throw new Error("CUA provider tool description must be a string");
    if (!tool.inputSchema || typeof tool.inputSchema !== "object" || Array.isArray(tool.inputSchema)) throw new Error("CUA provider tool inputSchema must be a JSON schema object");
    names.add(tool.name);
  }
  return structuredClone(tools);
}

function modelVisible(tool) {
  const visibility = tool._meta?.ui?.visibility;
  return !Array.isArray(visibility) || visibility.includes("model");
}
