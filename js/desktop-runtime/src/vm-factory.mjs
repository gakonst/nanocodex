import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import { isAbsolute, join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";

/** A desktop factory must explicitly opt into a desktop image. Never promote
 * the legacy shell-only rootfs into the unified desktop mount recipe. */
export function desktopFactoryRecipe(defaults, host, dataDirectory) {
  if (!defaults.desktopRootfs) return undefined;
  for (const field of ["binary", "desktopRootfs", "guestRuntime"]) {
    if (typeof defaults[field] !== "string" || !isAbsolute(defaults[field]) || defaults[field].includes("\0")) throw new Error(`Choose an absolute ${field} path for the desktop factory.`);
  }
  const label = host.name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 40) || "desktop";
  const factoryName = defaults.factoryName ?? `${label}-${createHash("sha256").update(host.id).digest("hex").slice(0, 12)}`;
  if (typeof factoryName !== "string" || !/^[a-z0-9](?:[a-z0-9._-]{0,61}[a-z0-9])?$/.test(factoryName) || ["cf_sandbox", "cloudflare", "host"].includes(factoryName)) throw new Error("Choose a non-reserved lowercase factory name of 1–63 characters.");
  const args = ["host", "--factory-name", factoryName, "--vm-template", defaults.desktopRootfs,
    "--vm-guest-runtime", defaults.guestRuntime, "--state-dir", join(dataDirectory, "vm-factories", factoryName),
    "--vm-cache", join(dataDirectory, "vm-cache"),
    "--vm-workspace", "/workspace", "--vm-memory-mib", "2048", "--log-format", "json"];
  if (defaults.firmware) args.push("--vm-firmware", defaults.firmware);
  return { factoryName, binary: defaults.binary, args };
}

/** Credentials enter only the child's environment. Child output is reduced to
 * known lifecycle stages and errors sanitized by the runtime before display. */
export function superviseVmFactory({ binary, args, env, signal, onState, sanitize = () => "Desktop factory failed.",
  spawnProcess = spawn, readyTimeoutMs = 90_000, retryDelays = [1_000, 2_000, 4_000], stopTimeoutMs = 20_000, killTimeoutMs = 2_000 }) {
  const controller = new AbortController();
  const abort = () => controller.abort();
  signal.addEventListener("abort", abort, { once: true });
  if (signal.aborted) abort();
  let resolveReady;
  const ready = new Promise(resolve => { resolveReady = resolve; });
  const update = value => { if (!controller.signal.aborted) onState(value); };
  async function attempt() {
    controller.signal.throwIfAborted();
    const child = spawnProcess(binary, args, { env, detached: process.platform !== "win32", stdio: ["ignore", "pipe", "pipe"] });
    let didClose = false, failure, timeout;
    let resolveClosed;
    const closed = new Promise(resolve => { resolveClosed = resolve; });
    child.once("error", error => { failure = sanitize(error); });
    child.once("close", (code, childSignal) => { didClose = true; clearTimeout(timeout); resolveClosed({ code, childSignal }); });
    const sendSignal = value => {
      if (!child.pid || didClose) return;
      try { process.kill(process.platform === "win32" ? child.pid : -child.pid, value); }
      catch (error) { if (error.code !== "ESRCH") throw error; }
    };
    let stopping;
    const stop = () => stopping ??= (async () => {
      if (didClose) return;
      sendSignal("SIGINT");
      await Promise.race([closed, delay(stopTimeoutMs, undefined, { ref: false })]);
      if (!didClose) { sendSignal("SIGTERM"); await Promise.race([closed, delay(killTimeoutMs, undefined, { ref: false })]); }
      if (!didClose) sendSignal("SIGKILL");
      await closed;
    })();
    const stopOnAbort = () => { void stop().catch(() => {}); };
    controller.signal.addEventListener("abort", stopOnAbort, { once: true });
    const armDeadline = () => {
      if (timeout) return;
      timeout = setTimeout(() => { failure = "The desktop factory did not connect within its readiness deadline."; void stop().catch(() => {}); }, readyTimeoutMs);
      timeout.unref();
    };
    const consume = () => {
      let buffer = "";
      return chunk => {
        buffer = (buffer + chunk.toString()).slice(-32_768);
        const lines = buffer.split("\n"); buffer = lines.pop();
        for (const line of lines) {
          if (controller.signal.aborted || stopping) continue;
          try {
            const entry = JSON.parse(line);
            if (entry.fields?.stage === "vm.host.ready") {
              clearTimeout(timeout); timeout = undefined; failure = undefined; update({ status: "connected" }); resolveReady();
            }
            if (entry.fields?.stage === "vm.host.reconnecting") {
              // Registration outages must not terminate the factory or its guests.
              update({ status: "reconnecting", ...(entry.fields.error ? { error: sanitize(entry.fields.error) } : {}) });
            }
            if (entry.level === "ERROR") failure = sanitize(entry.fields?.error ?? entry.fields?.message ?? "Desktop factory failed.");
          } catch { if (line.startsWith("Error: ")) failure = sanitize(line.slice(7)); }
        }
      };
    };
    child.stdout.on("data", consume()); child.stderr.on("data", consume());
    armDeadline();
    try {
      if (controller.signal.aborted) await stop();
      const result = await closed;
      return failure || `Desktop factory exited (${result.code ?? result.childSignal}).`;
    } finally {
      clearTimeout(timeout);
      controller.signal.removeEventListener("abort", stopOnAbort);
      await stop();
    }
  }
  const done = (async () => {
    try {
      for (let index = 0; !controller.signal.aborted; index++) {
        update({ status: index === 0 ? "connecting" : "reconnecting", attempt: index + 1 });
        let error;
        try { error = await attempt(); } catch (cause) { error = sanitize(cause); }
        if (controller.signal.aborted) break;
        if (index >= retryDelays.length) { update({ status: "error", error }); break; }
        update({ status: "reconnecting", error, attempt: index + 1 });
        await delay(retryDelays[index], undefined, { signal: controller.signal, ref: false });
      }
    } catch (error) { if (!controller.signal.aborted) update({ status: "error", error: sanitize(error) }); }
    finally { signal.removeEventListener("abort", abort); resolveReady(); }
  })();
  return { ready, done, async close() { abort(); await done; } };
}
