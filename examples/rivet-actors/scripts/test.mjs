import { spawn } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const temporaryDirectory = await mkdtemp(join(tmpdir(), "nanocodex-rivet-test-"));
const vitest = fileURLToPath(new URL("../node_modules/vitest/vitest.mjs", import.meta.url));

try {
  const { exitCode, reaperDone } = await new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [vitest, "run"], {
      env: {
        ...process.env,
        RIVET__file_system__path: join(temporaryDirectory, "engine-db"),
      },
      stdio: "inherit",
    });
    const reaper = process.platform === "win32" || !child.pid
      ? undefined
      : spawn(
        process.execPath,
        [fileURLToPath(new URL("reap-rivet-engine.mjs", import.meta.url)), String(child.pid)],
        { detached: true, stdio: "ignore" },
      );
    const reaperDone = reaper === undefined
      ? Promise.resolve()
      : new Promise((resolveReaper, rejectReaper) => {
        reaper.once("error", rejectReaper);
        reaper.once("exit", (code) => code === 0
          ? resolveReaper()
          : rejectReaper(new Error(`Rivet engine reaper exited with code ${code}`)));
      });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (signal) reject(new Error(`vitest terminated by ${signal}`));
      else resolve({ exitCode: code ?? 1, reaperDone });
    });
  });
  await reaperDone;
  process.exitCode = exitCode;
} finally {
  await rm(temporaryDirectory, { recursive: true, force: true });
}
