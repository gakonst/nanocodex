import assert from "node:assert/strict";
import { chmod, copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync, spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const buildLockModule = fileURLToPath(new URL("../scripts/build-lock.mjs", import.meta.url));
const buildScript = fileURLToPath(new URL("../scripts/build-js-package.sh", import.meta.url));
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

test("signals terminate the whole generator process group", {
  skip: process.platform === "win32",
}, async () => {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-build-lock-signal-"));
  const lockPath = join(directory, "nanocodex.lock");
  const markerPath = join(directory, "marker.log");
  const workerPath = join(directory, "worker.mjs");
  await writeFile(workerPath, `
    import { appendFile } from "node:fs/promises";
    const [markerPath] = process.argv.slice(2);
    await appendFile(markerPath, "started\\n");
    await new Promise((resolve) => setTimeout(resolve, 500));
    await appendFile(markerPath, "survived\\n");
  `);

  const child = spawn(
    process.execPath,
    [
      buildLockModule,
      lockPath,
      "sh",
      "-c",
      '"$1" "$2" "$3"',
      "nanocodex-signal-test",
      process.execPath,
      workerPath,
      markerPath,
    ],
    { stdio: "ignore" },
  );
  try {
    await waitForFile(markerPath);
    child.kill("SIGTERM");
    const result = await waitForExit(child);
    assert.notEqual(result.code, 0);
    await new Promise((resolve) => setTimeout(resolve, 650));
    assert.equal(await readFile(markerPath, "utf8"), "started\n");
    await assert.rejects(readFile(lockPath), { code: "ENOENT" });
  } finally {
    child.kill("SIGKILL");
    await rm(directory, { recursive: true, force: true });
  }
});

test("the WASM cache requires every generated glue artifact", async () => {
  for (const [missingPackage, missingArtifact] of [
    ["pkg-web", "nanocodex.js"],
    ["pkg-web", "nanocodex.d.ts"],
    ["pkg-node", "nanocodex.d.ts"],
    ["pkg-node", "package.json"],
  ]) {
    const directory = await mkdtemp(join(tmpdir(), "nanocodex-build-cache-"));
    const repository = join(directory, "repository");
    const scripts = join(repository, "js/nanocodex-vite/scripts");
    const webPackage = join(repository, "js/nanocodex/pkg-web");
    const nodePackage = join(repository, "js/nanocodex/pkg-node");
    const fakeBin = join(directory, "bin");
    const markerPath = join(directory, "wasm-bindgen-called");
    const builder = join(scripts, "build-js-package.sh");
    const wasmArtifact = join(
      repository,
      "target/wasm32-unknown-unknown/wasm/nanocodex_wasm.wasm",
    );
    try {
      await Promise.all([
        mkdir(scripts, { recursive: true }),
        mkdir(webPackage, { recursive: true }),
        mkdir(nodePackage, { recursive: true }),
        mkdir(fakeBin, { recursive: true }),
        mkdir(join(repository, "target/wasm32-unknown-unknown/wasm"), { recursive: true }),
      ]);
      await copyFile(buildScript, builder);
      await chmod(builder, 0o755);
      await writeFile(wasmArtifact, "fake wasm\n");
      await Promise.all([
        writeFile(join(webPackage, "nanocodex.js"), "web glue\n"),
        writeFile(join(webPackage, "nanocodex.d.ts"), "web types\n"),
        writeFile(join(webPackage, "nanocodex_bg.wasm"), "web wasm\n"),
        writeFile(join(webPackage, "nanocodex_bg.js"), "web glue\n"),
        writeFile(join(webPackage, "nanocodex_worker.js"), "worker glue\n"),
        writeFile(join(nodePackage, "nanocodex.js"), "node glue\n"),
        writeFile(join(nodePackage, "nanocodex.d.ts"), "node types\n"),
        writeFile(join(nodePackage, "package.json"), '{"type":"commonjs"}\n'),
      ]);
      await rm(join(repository, missingPackage, missingArtifact), { force: true });
      await writeFakeCommand(fakeBin, "cargo", "exit 0");
      await writeFakeCommand(
        fakeBin,
        "wasm-bindgen",
        'if [ "$1" = "--version" ]; then echo wasm-bindgen-test; else printf called > "$NANOCODEX_WASM_BINDGEN_MARKER"; exit 17; fi',
      );
      await writeFakeCommand(fakeBin, "node", "exit 0");
      await writeFakeCommand(
        join(repository, "js/nanocodex/node_modules/.bin"),
        "wasm-opt",
        'if [ "$1" = "--version" ]; then echo wasm-opt-test; else exit 17; fi',
      );
      const checksum = execFileSync("cksum", [wasmArtifact], { encoding: "utf8" }).trim();
      await writeFile(
        join(webPackage, ".nanocodex-bindgen-stamp"),
        `wasm-bindgen-test\nwasm-opt-test\nworker-bundler-v1-simd\n${checksum}\n`,
      );
      const result = await runProcess(builder, {
        cwd: repository,
        env: {
          ...process.env,
          PATH: `${fakeBin}:${process.env.PATH}`,
          NANOCODEX_WASM_BUILD_LOCK_HELD: "1",
          NANOCODEX_WASM_BINDGEN_MARKER: markerPath,
          CARGO_TARGET_DIR: join(repository, "target"),
        },
      });
      assert.notEqual(result.code, 0, `cache incorrectly accepted missing ${missingArtifact}`);
      assert.equal(await readFile(markerPath, "utf8"), "called");
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  }
});

async function writeFakeCommand(directory, name, body) {
  await mkdir(directory, { recursive: true });
  const path = join(directory, name);
  await writeFile(path, `#!/bin/sh\n${body}\n`);
  await chmod(path, 0o755);
}

async function waitForFile(path) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try {
      return await readFile(path, "utf8");
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
  }
  throw new Error(`timed out waiting for ${path}`);
}

function waitForExit(child) {
  return new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
}

function runProcess(command, { cwd, env }) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, [], { cwd, env, stdio: "ignore" });
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
}

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
