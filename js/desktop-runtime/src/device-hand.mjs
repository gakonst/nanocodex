import { isAbsolute } from "node:path";
import { spawn } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";

/** The Rust helper shares one OS-locked publisher with all terminal clients.
 * Closing stdin releases this app's interest, not another client's Hand. */
export async function describeDeviceHand(binary, env, { spawnProcess = spawn } = {}) {
  const child = spawnProcess(binary, ["__device-hand", "--describe"], { env, stdio: ["ignore", "pipe", "pipe"] });
  let output = "", error = "";
  child.stdout.on("data", data => { output = (output + data).slice(-8192); });
  child.stderr.on("data", data => { error = (error + data).slice(-8192); });
  const timeout = setTimeout(() => child.kill(), 10_000);
  try {
    await new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("close", code => code === 0 ? resolve() : reject(new Error(error || "The computer Hand could not start. Update nanocodex2.")));
    });
    const value = JSON.parse(output);
    if (!/^[0-9a-f-]{36}$/.test(value.id) || typeof value.name !== "string" || typeof value.workspace !== "string" || !isAbsolute(value.workspace)) throw new Error("Invalid computer Hand identity.");
    return { id: value.id, name: value.name, workspace: value.workspace, kind: "local" };
  } finally { clearTimeout(timeout); }
}

/** Verify and save the app login through the CLI's private account store.
 * The key travels only over stdin. The account-specific file is independent
 * of the global CLI login and existing LaunchAgent configuration. */
export async function saveDeviceHandLogin(binary, env, accountFile, { signal, spawnProcess = spawn } = {}) {
  if (!isAbsolute(accountFile)) throw new Error("The Hand credential file must have an absolute path.");
  const key = env.NANOCODEX_API_KEY;
  if (!/^ncx_live_[A-Za-z0-9_-]{12}_[A-Za-z0-9_-]{43}$/.test(key ?? "")) throw new Error("Sign in before connecting this computer.");
  signal?.throwIfAborted();
  const loginEnvironment = { ...env, NANOCODEX_ACCOUNT_FILE: accountFile };
  delete loginEnvironment.NANOCODEX_API_KEY;
  delete loginEnvironment.NC_API_KEY;
  const child = spawnProcess(binary, ["account", "login", "--with-api-key"], {
    env: loginEnvironment, stdio: ["pipe", "ignore", "ignore"], windowsHide: true,
  });
  const abort = () => child.kill("SIGTERM");
  const timeout = setTimeout(abort, 30_000);
  signal?.addEventListener("abort", abort, { once: true });
  try {
    await new Promise((resolve, reject) => {
      const fail = () => reject(new Error("Could not save the verified Hand login. Check account access and available disk space, then reconnect."));
      child.once("error", fail);
      child.once("close", code => code === 0 ? resolve() : fail());
      child.stdin.on("error", fail);
      child.stdin.end(key + "\n");
      if (signal?.aborted) abort();
    });
    signal?.throwIfAborted();
    return accountFile;
  } finally {
    clearTimeout(timeout);
    signal?.removeEventListener("abort", abort);
  }
}

export function connectDeviceHand({ binary, env, signal, onState, spawnProcess = spawn, timeoutMs = 30_000 }) {
  const child = spawnProcess(binary, ["__device-hand", "--parent-pipe"], { env, stdio: ["pipe", "pipe", "pipe"] });
  let settleReady, rejectReady, exited = false, stopping = false, error = "";
  const ready = new Promise((resolve, reject) => { settleReady = resolve; rejectReady = reject; });
  // "close" also waits for pipes inherited by the shared publisher on Windows.
  // Only the direct helper process owns this client lease; its exit releases it.
  const done = new Promise(resolve => {
    const finish = () => { exited = true; resolve(); };
    child.once("exit", finish);
    child.once("error", () => { if (!child.pid) finish(); }); // Spawn failures have no exit event.
  });
  let buffer = "";
  child.stdout.on("data", data => {
    buffer = (buffer + data).slice(-32_768);
    const lines = buffer.split("\n"); buffer = lines.pop();
    for (const line of lines) {
      if (stopping) continue;
      try {
        const state = JSON.parse(line);
        if (state.status === "error") {
          rejectReady(new Error(state.error || "Computer Hand stopped."));
          onState(state); void close(); continue;
        }
        if (!["connecting", "connected"].includes(state.status)) continue;
        onState(state);
        if (state.status === "connected") { clearTimeout(timer); settleReady(); }
      } catch { /* Only lifecycle JSON is part of this protocol. */ }
    }
  });
  child.stderr.on("data", data => { error = (error + data).slice(-4096); });
  child.on("error", rejectReady);
  child.on("close", code => {
    clearTimeout(timer);
    const reason = new Error(error || `Computer Hand stopped (${code}).`);
    rejectReady(reason);
    if (!stopping) onState({ status: "error", error: reason.message });
  });
  let closing;
  const close = () => closing ??= (async () => {
    stopping = true; clearTimeout(timer); signal.removeEventListener("abort", abort);
    rejectReady(new Error("Computer Hand connection stopped."));
    child.stdin.end();
    await Promise.race([done, delay(25_000, undefined, { ref: false })]);
    if (!exited) child.kill("SIGTERM");
    await Promise.race([done, delay(2_000, undefined, { ref: false })]);
    if (!exited) child.kill("SIGKILL");
    await done;
    // Stop observing our pipes without terminating the shared publisher.
    child.stdin.destroy();
    child.stdout.destroy();
    child.stderr.destroy();
  })();
  const abort = () => { void close(); };
  const timer = setTimeout(() => { rejectReady(new Error("The computer Hand did not connect within 30 seconds.")); void close(); }, timeoutMs);
  timer.unref();
  signal.addEventListener("abort", abort, { once: true });
  if (signal.aborted) abort();
  return { ready, close };
}
