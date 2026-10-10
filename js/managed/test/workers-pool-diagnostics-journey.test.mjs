// Journey for test/workers-pool-diagnostics.ts: a workers-pool file that passes
// but whose runner does not report "testfileFinished" (the #951 symptom) must be
// named while the run is stalled. The diagnostic must not end, pass or retry
// that run: once the journey releases the held import, vitest finishes the run
// normally.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const managed = fileURLToPath(new URL("..", import.meta.url));
const vitest = fileURLToPath(new URL("../node_modules/vitest/vitest.mjs", import.meta.url));
const FIXTURE = "test-fixtures/workers-pool-stall/held-import.test.ts";

test("names the stalled workers-pool file while the run waits, then lets it finish", async () => {
  const scratch = mkdtempSync(join(tmpdir(), "nanocodex-pool-stall-"));
  const release = join(scratch, "release");
  const child = spawn(process.execPath, [vitest, "run", "--config", "vitest.pool-stall.config.ts"], {
    cwd: managed,
    env: { ...process.env, NANOCODEX_WORKERS_POOL_STALL_MS: "5000", NANOCODEX_POOL_STALL_RELEASE: release, NO_COLOR: "1" },
    detached: true,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let output = "";
  let exited = null;
  const exit = new Promise(resolve => child.on("exit", (code, signal) => { exited = { code, signal }; resolve(); }));
  child.stdout.on("data", chunk => { output += chunk; });
  child.stderr.on("data", chunk => { output += chunk; });
  const plain = () => output.replace(/\u001b\[[0-9;]*m/g, "");
  const tail = () => plain().slice(-4000);
  const until = async (done, ms) => {
    const deadline = Date.now() + ms;
    while (!done() && !exited && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 200));
  };
  try {
    // These bound the journey itself; the diagnostic under test has no deadline.
    await until(() => plain().includes("✓ " + FIXTURE) && plain().includes("[workers-pool-diagnostics] " + FIXTURE), 60_000);
    assert.equal(exited, null, "vitest exited while the import was still held:\n" + tail());
    assert.ok(plain().includes("✓ " + FIXTURE), "fixture did not pass:\n" + tail());
    const stalled = plain();
    assert.ok(stalled.includes("[workers-pool-diagnostics] " + FIXTURE + " has not reported testfileFinished"), "no diagnostic named the stalled file:\n" + tail());
    assert.doesNotMatch(stalled, /Test Files/, "vitest summarized the run before testfileFinished");
    const log = /Log: (\S+\.log)/.exec(stalled)?.[1];
    assert.ok(log, "diagnostic did not name its log file:\n" + tail());
    assert.ok(readFileSync(log, "utf8").includes(FIXTURE));

    // Releasing the import must let the same run finish on its own and pass.
    writeFileSync(release, "");
    await until(() => false, 60_000);
    assert.deepEqual(exited, { code: 0, signal: null }, "vitest did not finish after the release:\n" + tail());
    assert.match(plain(), /Test Files\s+1 passed \(1\)/);
    assert.equal(plain().split("[workers-pool-diagnostics] ").length - 1, 1, "expected exactly one report:\n" + tail());
    console.log(stalled.split("\n").filter(line => line.includes("[workers-pool-diagnostics]")).join("\n"));
  } finally {
    if (!exited) {
      process.kill(-child.pid, "SIGKILL");
      await exit;
    }
    rmSync(scratch, { recursive: true, force: true });
  }
});
