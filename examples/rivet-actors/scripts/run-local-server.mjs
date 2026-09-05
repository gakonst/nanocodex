import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const args = process.argv.slice(2);
const useEnvFile = args[0] === "--env-file";
const entry = args[useEnvFile ? 1 : 0];
if (!entry) throw new Error("run-local-server requires an entry module");
const entryArgs = args.slice(useEnvFile ? 2 : 1);
const childArgs = [
  ...(useEnvFile ? ["--env-file-if-exists=../../.env"] : []),
  "--import",
  "tsx",
  entry,
  ...entryArgs,
];
const child = spawn(process.execPath, childArgs, {
  cwd: process.cwd(),
  detached: false,
  env: process.env,
  stdio: "inherit",
});
if (process.platform !== "win32" && !process.env.RIVET_ENDPOINT && child.pid) {
  const reaper = spawn(
    process.execPath,
    [fileURLToPath(new URL("reap-rivet-engine.mjs", import.meta.url)), String(child.pid)],
    { detached: true, stdio: "ignore" },
  );
  reaper.unref();
}

let stopping = false;
let forceTimer;
for (const signal of ["SIGINT", "SIGTERM"]) {
  process.once(signal, () => stop(signal));
}

const [code, signal] = await new Promise((resolve, reject) => {
  child.once("error", reject);
  child.once("exit", (exitCode, exitSignal) => resolve([exitCode, exitSignal]));
});
if (forceTimer) clearTimeout(forceTimer);
process.exitCode = code ?? signalExitCode(signal);

function stop(signal) {
  if (stopping) return;
  stopping = true;
  const forwarded = signal === "SIGINT" ? "SIGTERM" : signal;
  signalDescendants(forwarded);
  signalChild(forwarded);
  forceTimer = setTimeout(() => {
    signalDescendants("SIGKILL");
    signalChild("SIGKILL");
  }, 15_000);
}

function signalDescendants(signal) {
  if (!child.pid) return;
  if (process.platform === "win32") {
    spawnSync("taskkill", ["/PID", String(child.pid), "/T", "/F"], { stdio: "ignore" });
    return;
  }
  // Rivet's local engine starts in its own process group, so signaling only
  // the app group does not reach it. pkill exits 1 when no child remains.
  spawnSync("pkill", [`-${signal}`, "-P", String(child.pid)], { stdio: "ignore" });
}

function signalChild(signal) {
  try {
    child.kill(signal);
  } catch (error) {
    if (error?.code !== "ESRCH") throw error;
  }
}

function signalExitCode(signal) {
  if (signal === "SIGINT") return 130;
  if (signal === "SIGTERM") return 143;
  if (signal === "SIGKILL") return 137;
  return signal ? 1 : 0;
}
