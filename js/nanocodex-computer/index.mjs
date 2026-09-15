import { spawn } from "node:child_process";
import { access, readFile } from "node:fs/promises";
import { constants } from "node:fs";
import { delimiter, dirname, join } from "node:path";
import { homedir } from "node:os";
import { fileURLToPath } from "node:url";
import { namedTool } from "nanocodex-tools/named-tool";
import { toolResult } from "nanocodex-tools/runtime/code-runtime";
import { CUA_JS_NAME, CUA_RESET_NAME, CUA_DESCRIPTION, CUA_PARAMETERS, CUA_RESET_DESCRIPTION, CUA_RESET_PARAMETERS, validateInput } from "./contract.mjs";

/** Discover only the trusted installed companion; no app or browser is started. */
export async function discoverComputer({ binary } = {}) {
  const explicit = process.env.NANOCODEX_COMPUTER;
  if (["off", "none", "0"].includes(explicit)) return undefined;
  if (explicit) return explicit;
  const name = process.platform === "win32" ? "nanocodex-computer.exe" : "nanocodex-computer";
  const candidates = [
    ...(binary ? [join(dirname(binary), name)] : []),
    join(dirname(process.execPath), name),
    // Source checkout only. Bundled builds resolve these to nonexistent paths.
    join(dirname(fileURLToPath(import.meta.url)), "../../crates/experimental/nanocodex-computer/runtime/target/debug", name),
    join(dirname(fileURLToPath(import.meta.url)), "../../crates/experimental/nanocodex-computer/runtime/target/release", name),
    join(homedir(), ".cargo", "bin", name),
    ...(process.env.PATH ?? "").split(delimiter).filter(Boolean).map(path => join(path, name)),
  ];
  for (const path of candidates) {
    try { await access(path, constants.X_OK); return path; } catch {}
  }
}

/** A native attachment owns the executable/configuration, each conversation a JS scope. */
export function createComputerTools({ executable, args = [], environment = {}, desktopRuntime }) {
  if (!executable) throw new TypeError("A trusted CUA executable is required");
  const launchArgs = [...args];
  for (const [name, flag] of [["NANOCODEX_COMPUTER_SECURITY_CONFIG", "--security-config"], ["NANOCODEX_COMPUTER_CDP", "--cdp"], ["NANOCODEX_COMPUTER_BROWSER_PREFERENCES", "--browser-preferences"], ["NANOCODEX_COMPUTER_IAB_CONFIG", "--iab-config"], ["NANOCODEX_COMPUTER_RUNTIME_CONFIG", "--runtime-config"], ["NANOCODEX_COMPUTER_PLATFORM_CONFIG", "--platform-config"]]) {
    if (process.env[name]) launchArgs.push(flag, process.env[name]);
  }
  const sessions = new Map();
  let disposed = false;
  const releaseSession = id => {
    const session = sessions.get(id);
    sessions.delete(id);
    session?.lifetime.abort(new Error("CUA conversation was released"));
    session?.process?.close();
  };
  const close = async () => { disposed = true; for (const id of sessions.keys()) releaseSession(id); };
  const invoke = (reset, input, context) => {
    const value = validateInput(input, reset);
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
    const signal = AbortSignal.any([session.lifetime.signal, ...(context.signal ? [context.signal] : [])]);
    const run = async () => {
      signal.throwIfAborted();
      if (disposed) throw new Error("CUA attachment is closed");
      if (sessions.get(id) !== session) throw new Error("CUA conversation was released");
      if (session.interrupted && !reset) throw new Error("CUA session was interrupted. Call cua_repl.js_reset, then select the surface again.");
      const deadline = new AbortController();
      const operation = AbortSignal.any([signal, deadline.signal]);
      const abort = () => { session.process?.close(operation.reason); session.process = undefined; session.interrupted = true; };
      const cancelTimeout = value.timeout_ms === undefined
        ? () => {}
        : scheduleDeadline(() => deadline.abort(new Error("CUA runtime timed out; call cua_repl.js_reset before continuing")), value.timeout_ms + 5_000);
      operation.addEventListener("abort", abort, { once: true });
      try {
        if (!session.process) {
          let desktopEnvironment = {};
          if (desktopRuntime) {
            const ready = JSON.parse(await readFile(join(desktopRuntime, "ready"), "utf8")
              .catch(() => { throw new Error("This Hand's desktop is unavailable; start its screen before using CUA."); }));
            if (typeof ready.display !== "string" || !ready.display.startsWith(":")) throw new Error("Hand desktop did not publish a local X display");
            desktopEnvironment = { DISPLAY: ready.display, XAUTHORITY: join(desktopRuntime, "Xauthority"), WAYLAND_DISPLAY: undefined };
          }
          operation.throwIfAborted();
          if (disposed) throw new Error("CUA attachment is closed");
          session.process = new ComputerProcess(executable, launchArgs, { ...environment, ...desktopEnvironment });
          await session.process.initialize();
        }
        const result = await session.process.rpc("tools/call", {
          name: reset ? "js_reset" : "js", arguments: value,
          _meta: { "x-codex-turn-metadata": { thread_id: id, call_id: context.callId, model: context.model } },
        });
        operation.throwIfAborted();
        const content = outputContent(result);
        session.interrupted = false;
        return toolResult(content, result, { value: result, success: result.isError !== true, metadata: result._meta });
      } catch (error) {
        session.process?.close(); session.process = undefined; session.interrupted = true; throw error;
      } finally { cancelTimeout(); operation.removeEventListener("abort", abort); }
    };
    // A QuickJS scope is ordered, but every conversation owns an independent
    // chain and process. Long work in one conversation never blocks another.
    const result = session.tail.then(run);
    session.tail = result.catch(() => {});
    // A queued cancellation reaches the caller immediately. The queued run
    // still checks the signal and session identity before touching its process.
    return interruptible(result, signal);
  };
  return Object.freeze({
    close,
    tools: [
      namedTool(CUA_JS_NAME, { description: CUA_DESCRIPTION, parameters: CUA_PARAMETERS, supportsParallelToolCalls: true, handler: (input, context) => invoke(false, input, context), releaseSession, dispose: close }),
      namedTool(CUA_RESET_NAME, { description: CUA_RESET_DESCRIPTION, parameters: CUA_RESET_PARAMETERS, supportsParallelToolCalls: true, handler: (input, context) => invoke(true, input, context), releaseSession, dispose: close }),
    ],
  });
}

function interruptible(result, signal) {
  return new Promise((resolve, reject) => {
    const abort = () => reject(signal.reason);
    if (signal.aborted) abort();
    else signal.addEventListener("abort", abort, { once: true });
    result.then(resolve, reject).finally(() => signal.removeEventListener("abort", abort));
  });
}

// Node's setTimeout overflows above 2^31-1 ms. Long valid Codex timeouts must
// remain long instead of unexpectedly terminating the kernel after one ms.
function scheduleDeadline(callback, milliseconds) {
  const deadline = performance.now() + milliseconds;
  let timer;
  const tick = () => {
    const remaining = deadline - performance.now();
    if (remaining <= 0) callback();
    else timer = setTimeout(tick, Math.min(remaining, 2_147_483_647));
  };
  tick();
  return () => clearTimeout(timer);
}

class ComputerProcess {
  constructor(executable, args, environment) {
    const env = Object.fromEntries(["PATH", "HOME", "USER", "LOGNAME", "TMPDIR", "TEMP", "SystemRoot", "LOCALAPPDATA", "DISPLAY", "XAUTHORITY", "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "LANG", "SKY_ENABLE_AUDIO"]
      .filter(key => process.env[key] !== undefined).map(key => [key, process.env[key]]));
    this.child = spawn(executable, [...args, "--allow-native-control", "serve"], { env: { ...env, ...environment }, stdio: ["pipe", "pipe", "ignore"], windowsHide: true });
    this.pending = new Map(); this.sequence = 0; this.lines = new LineBuffer();
    this.child.stdout.on("data", chunk => {
      try {
        this.lines.push(chunk);
        for (let line; (line = this.lines.shift()) !== undefined;) {
          const value = JSON.parse(line.toString("utf8"));
          if (value.method) {
            if (value.id !== undefined) void this.send({ jsonrpc: "2.0", id: value.id, error: { code: -32601, message: "No interactive approval channel; configure host-approved surfaces." } }).catch(error => this.close(error));
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
  async initialize() {
    await this.rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "nanocodex-computer", version: "0.1.0" } });
    await this.send({ jsonrpc: "2.0", method: "notifications/initialized" });
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
    if (item.type === "image" && ["image/png", "image/jpeg", "image/webp"].includes(item.mimeType) && typeof item.data === "string") {
      const bytes = decodeBase64(item.data);
      const mime = bytes.subarray(0, 8).equals(Buffer.from([137, 80, 78, 71, 13, 10, 26, 10])) ? "image/png"
        : bytes[0] === 255 && bytes[1] === 216 && bytes[2] === 255 ? "image/jpeg"
        : bytes.toString("ascii", 0, 4) === "RIFF" && bytes.toString("ascii", 8, 12) === "WEBP" ? "image/webp" : undefined;
      if (!mime) throw new TypeError("Invalid CUA image bytes");
      return { type: "input_image", image_url: `data:${mime};base64,${item.data}`, detail: "original" };
    }
    if (item.type === "audio" && ["audio/wav", "audio/mpeg"].includes(item.mimeType) && typeof item.data === "string") {
      decodeBase64(item.data);
      return { type: "input_audio", audio_url: `data:${item.mimeType};base64,${item.data}` };
    }
    throw new TypeError("Unsupported CUA output content");
  });
}

function decodeBase64(data) {
  if (!/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(data) || !data.length) throw new TypeError("Invalid CUA base64 content");
  return Buffer.from(data, "base64");
}
