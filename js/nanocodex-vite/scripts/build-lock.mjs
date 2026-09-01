import { randomUUID } from "node:crypto";
import { spawn } from "node:child_process";
import { mkdir, open, readFile, rename, rm, stat } from "node:fs/promises";
import { dirname } from "node:path";
import { pathToFileURL } from "node:url";

const POLL_INTERVAL_MS = 50;
const STALE_LOCK_GRACE_MS = 5_000;

export async function withBuildLock(lockPath, operation, {
  pollIntervalMs = POLL_INTERVAL_MS,
  staleLockGraceMs = STALE_LOCK_GRACE_MS,
} = {}) {
  const owner = await acquireBuildLock(lockPath, { pollIntervalMs, staleLockGraceMs });
  try {
    return await operation();
  } finally {
    await releaseBuildLock(lockPath, owner);
  }
}

async function acquireBuildLock(lockPath, { pollIntervalMs, staleLockGraceMs }) {
  await mkdir(dirname(lockPath), { recursive: true });
  const owner = { pid: process.pid, token: randomUUID() };

  for (;;) {
    try {
      const handle = await open(lockPath, "wx");
      try {
        await handle.writeFile(`${JSON.stringify(owner)}\n`, "utf8");
      } finally {
        await handle.close();
      }
      return owner;
    } catch (error) {
      if (error?.code !== "EEXIST") throw error;
      if (await reclaimStaleBuildLock(lockPath, staleLockGraceMs)) continue;
      await delay(pollIntervalMs);
    }
  }
}

async function releaseBuildLock(lockPath, owner) {
  try {
    const current = JSON.parse(await readFile(lockPath, "utf8"));
    if (current.pid === owner.pid && current.token === owner.token) {
      await rm(lockPath, { force: true });
    }
  } catch (error) {
    if (error?.code !== "ENOENT" && !(error instanceof SyntaxError)) throw error;
  }
}

async function reclaimStaleBuildLock(lockPath, graceMs) {
  let current;
  let metadata;
  try {
    [current, metadata] = await Promise.all([
      readFile(lockPath, "utf8"),
      stat(lockPath),
    ]);
  } catch (error) {
    return error?.code === "ENOENT";
  }

  let owner;
  try {
    owner = JSON.parse(current);
  } catch {
    owner = undefined;
  }
  if (owner && processIsAlive(owner.pid)) return false;
  if (!owner && Date.now() - metadata.mtimeMs < graceMs) return false;

  const stalePath = `${lockPath}.stale-${process.pid}-${randomUUID()}`;
  try {
    await rename(lockPath, stalePath);
  } catch (error) {
    return error?.code === "ENOENT";
  }
  await rm(stalePath, { force: true });
  return true;
}

function processIsAlive(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return error?.code === "EPERM";
  }
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

async function runCommand(lockPath, command, arguments_) {
  await withBuildLock(lockPath, () => new Promise((resolve, reject) => {
    const child = spawn(command, arguments_, {
      env: { ...process.env, NANOCODEX_WASM_BUILD_LOCK_HELD: "1" },
      stdio: "inherit",
    });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0) resolve();
      else reject(new Error(`${command} exited with ${code ?? signal}`));
    });
    for (const signal of ["SIGINT", "SIGTERM"]) {
      process.once(signal, () => child.kill(signal));
    }
  }));
}

const invoked = process.argv[1]
  ? pathToFileURL(process.argv[1]).href
  : undefined;
if (invoked === import.meta.url) {
  const [lockPath, command, ...arguments_] = process.argv.slice(2);
  if (!lockPath || !command) throw new Error("build lock requires a lock path and command");
  await runCommand(lockPath, command, arguments_);
}
