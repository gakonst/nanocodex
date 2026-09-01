import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { test } from "node:test";

const buildLockModule = fileURLToPath(new URL("../scripts/build-lock.mjs", import.meta.url));

test("the build lock serializes concurrent WASM generators", async () => {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-build-lock-"));
  const lockPath = join(directory, "nanocodex.lock");
  const eventsPath = join(directory, "events.log");
  const workerPath = join(directory, "worker.mjs");
  await writeFile(workerPath, `
    import { appendFile } from "node:fs/promises";
    import { withBuildLock } from ${JSON.stringify(buildLockModule)};

    const [lockPath, eventsPath] = process.argv.slice(2);
    await withBuildLock(lockPath, async () => {
      await appendFile(eventsPath, "start\\n");
      await new Promise((resolve) => setTimeout(resolve, 75));
      await appendFile(eventsPath, "end\\n");
    });
  `);

  try {
    await Promise.all([
      runWorker(workerPath, lockPath, eventsPath),
      runWorker(workerPath, lockPath, eventsPath),
    ]);
    const events = (await readFile(eventsPath, "utf8")).trim().split("\n");
    let active = 0;
    let maximumActive = 0;
    for (const event of events) {
      if (event === "start") active += 1;
      if (event === "end") active -= 1;
      maximumActive = Math.max(maximumActive, active);
    }
    assert.deepEqual(events, ["start", "end", "start", "end"]);
    assert.equal(maximumActive, 1);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

function runWorker(workerPath, lockPath, eventsPath) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [workerPath, lockPath, eventsPath], {
      stdio: "ignore",
    });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0) resolve();
      else reject(new Error(`build-lock worker exited with ${code ?? signal}`));
    });
  });
}
