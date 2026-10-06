import { spawn } from "node:child_process";
import { realpath } from "node:fs/promises";
import { dirname, isAbsolute, join } from "node:path";

/** Prepare only the companion of this exact Hand release. No PATH lookup,
 * credentials, account login, service startup, or fallback installer. */
export async function prepareHandService(binary, { signal } = {}) {
  signal?.throwIfAborted();
  if (typeof binary !== "string" || !isAbsolute(binary)) throw new Error("Choose an absolute device Hand executable path.");
  const executable = await realpath(binary);
  signal?.throwIfAborted();
  const installer = join(dirname(executable), "nanocodex");
  const env = Object.fromEntries(["HOME", "PATH", "TMPDIR", "LANG", "USER"].filter(key => process.env[key] !== undefined).map(key => [key, process.env[key]]));
  const child = spawn(installer, ["hand", "install", "--prepare", "--executable", executable], {
    env, stdio: ["ignore", "ignore", "ignore"],
  });
  // Preparation only writes dormant state atomically. Wait for exit on cancel
  // so it cannot race a subsequent account's activation or an explicit opt-out.
  const abort = () => child.kill("SIGKILL");
  signal?.addEventListener("abort", abort, { once: true });
  let timedOut = false;
  const timeout = setTimeout(() => { timedOut = true; child.kill("SIGKILL"); }, 30_000);
  timeout.unref();
  try {
    const code = await new Promise((resolve, reject) => {
      child.once("error", () => reject(new Error("The device Hand needs its matching nanocodex installer beside it. Reinstall Nanocodex or check the configured device binary.")));
      child.once("close", resolve);
    });
    signal?.throwIfAborted();
    if (timedOut) throw new Error("Background Hand preparation timed out. Reopen Nanocodex to retry.");
    if (code !== 0) throw new Error("Background Hand preparation failed. Check disk space and install a matching nanocodex companion that supports hand install --prepare.");
  } finally {
    clearTimeout(timeout);
    signal?.removeEventListener("abort", abort);
  }
}
