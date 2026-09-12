import { spawn } from "node:child_process";
import { access, readFile } from "node:fs/promises";
import { constants } from "node:fs";
import { delimiter, dirname, join } from "node:path";
import { homedir } from "node:os";
import { fileURLToPath } from "node:url";
import { namedTool } from "nanocodex-tools/named-tool";
import { toolResult } from "nanocodex-tools/runtime/code-runtime";
import { CUA_JS_NAME, CUA_RESET_NAME, CUA_DESCRIPTION, CUA_PARAMETERS, CUA_RESET_DESCRIPTION, CUA_RESET_PARAMETERS, validateInput } from "./contract.mjs";

const MAX_FRAME = 8 * 1024 * 1024;

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
  for (const [name, flag] of [["NANOCODEX_COMPUTER_SECURITY_CONFIG", "--security-config"], ["NANOCODEX_COMPUTER_CDP", "--cdp"]]) {
    if (process.env[name]) launchArgs.push(flag, process.env[name]);
  }
  const sessions = new Map();
  let serial = Promise.resolve();
  let disposed = false;
  const releaseSession = id => { sessions.get(id)?.close(); sessions.delete(id); };
  const close = async () => { disposed = true; for (const id of sessions.keys()) releaseSession(id); };
  const invoke = (reset, input, context) => {
    const value = validateInput(input, reset);
    const run = async () => {
      context.signal?.throwIfAborted();
      if (disposed) throw new Error("CUA attachment is closed");
      const id = context.sessionId;
      if (!id) throw new Error("CUA requires a conversation identity");
      if (sessions.has(id) && !sessions.get(id) && !reset) throw new Error("CUA session was interrupted. Call cua_repl.js_reset, then select the surface again.");
      if (!sessions.has(id) && sessions.size >= 32) throw new Error("CUA attachment has reached its 32-conversation limit");
      let session = sessions.get(id);
      const abort = () => { session?.close(); sessions.set(id, null); };
      const timeout = setTimeout(abort, (value.timeout_ms ?? 30_000) + 5_000);
      context.signal?.addEventListener("abort", abort, { once: true });
      try {
        if (!session) {
          let desktopEnvironment = {};
          if (desktopRuntime) {
            const ready = JSON.parse(await readFile(join(desktopRuntime, "ready"), "utf8")
              .catch(() => { throw new Error("This Hand's desktop is unavailable; start its screen before using CUA."); }));
            if (typeof ready.display !== "string" || !ready.display.startsWith(":")) throw new Error("Hand desktop did not publish a local X display");
            desktopEnvironment = { DISPLAY: ready.display, XAUTHORITY: join(desktopRuntime, "Xauthority"), WAYLAND_DISPLAY: undefined };
          }
          context.signal?.throwIfAborted();
          if (disposed) throw new Error("CUA attachment is closed");
          session = new ComputerProcess(executable, launchArgs, { ...environment, ...desktopEnvironment });
          sessions.set(id, session);
          await session.initialize();
        }
        const result = await session.rpc("tools/call", {
          name: reset ? "js_reset" : "js", arguments: value,
          _meta: { "x-codex-turn-metadata": { thread_id: id, call_id: context.callId, model: context.model } },
        });
        context.signal?.throwIfAborted();
        const content = outputContent(result);
        return toolResult(content, result, { value: result, success: result.isError !== true });
      } catch (error) {
        session?.close(); sessions.set(id, null); throw error;
      } finally { clearTimeout(timeout); context.signal?.removeEventListener("abort", abort); }
    };
    const result = serial.then(run);
    serial = result.catch(() => {});
    return result;
  };
  return Object.freeze({
    close,
    tools: [
      namedTool(CUA_JS_NAME, { description: CUA_DESCRIPTION, parameters: CUA_PARAMETERS, handler: (input, context) => invoke(false, input, context), releaseSession, dispose: close }),
      namedTool(CUA_RESET_NAME, { description: CUA_RESET_DESCRIPTION, parameters: CUA_RESET_PARAMETERS, handler: (input, context) => invoke(true, input, context), releaseSession, dispose: close }),
    ],
  });
}

class ComputerProcess {
  constructor(executable, args, environment) {
    const env = Object.fromEntries(["PATH", "HOME", "USER", "LOGNAME", "TMPDIR", "TEMP", "SystemRoot", "LOCALAPPDATA", "DISPLAY", "XAUTHORITY", "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS", "LANG"]
      .filter(key => process.env[key] !== undefined).map(key => [key, process.env[key]]));
    this.child = spawn(executable, [...args, "--allow-native-control", "serve"], { env: { ...env, ...environment }, stdio: ["pipe", "pipe", "ignore"], windowsHide: true });
    this.pending = new Map(); this.sequence = 0; this.buffer = Buffer.alloc(0);
    this.child.stdout.on("data", chunk => {
      try {
        this.buffer = Buffer.concat([this.buffer, chunk]);
        let end;
        while ((end = this.buffer.indexOf(10)) !== -1) {
          if (end > MAX_FRAME) throw new Error("CUA response exceeds protocol limit");
          const value = JSON.parse(this.buffer.subarray(0, end).toString("utf8"));
          this.buffer = this.buffer.subarray(end + 1);
          if (value.method) {
            if (value.id !== undefined) this.send({ jsonrpc: "2.0", id: value.id, error: { code: -32601, message: "No interactive approval channel; configure host-approved surfaces." } });
            continue;
          }
          const pending = this.pending.get(value.id);
          if (!pending) throw new Error("CUA response belongs to an unknown call");
          this.pending.delete(value.id);
          if (value.error) pending.reject(new Error(value.error.message ?? "CUA runtime error"));
          else pending.resolve(value.result);
        }
        if (this.buffer.length > MAX_FRAME) throw new Error("CUA response exceeds protocol limit");
      } catch (error) { this.close(error); }
    });
    this.child.once("error", error => this.close(error));
    this.child.once("exit", () => this.close(new Error("CUA runtime exited")));
    this.child.stdin.on("error", error => this.close(error));
  }
  async initialize() {
    await this.rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "nanocodex-computer", version: "0.1.0" } });
    this.send({ jsonrpc: "2.0", method: "notifications/initialized" });
  }
  send(value) {
    if (this.closed) throw this.closed;
    const data = JSON.stringify(value) + "\n";
    if (Buffer.byteLength(data) > MAX_FRAME) throw new Error("CUA request exceeds protocol limit");
    this.child.stdin.write(data);
  }
  rpc(method, params) {
    return new Promise((resolve, reject) => {
      const id = ++this.sequence;
      this.pending.set(id, { resolve, reject });
      try { this.send({ jsonrpc: "2.0", id, method, params }); }
      catch (error) { this.pending.delete(id); reject(error); }
    });
  }
  close(error = new Error("CUA session stopped")) {
    if (this.closed) return;
    this.closed = error;
    this.child.kill("SIGKILL");
    this.child.stdin.destroy(); this.child.stdout.destroy();
    for (const pending of this.pending.values()) pending.reject(error);
    this.pending.clear(); this.buffer = Buffer.alloc(0);
  }
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
