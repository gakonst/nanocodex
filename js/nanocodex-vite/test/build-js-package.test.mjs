import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import {
  chmod,
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const sourceBuildScript = fileURLToPath(
  new URL("../scripts/build-js-package.sh", import.meta.url),
);
const generatedArtifacts = [
  "pkg-web/nanocodex.js",
  "pkg-web/nanocodex.d.ts",
  "pkg-web/nanocodex_bg.wasm",
  "pkg-web/nanocodex_bg.js",
  "pkg-web/nanocodex_worker.js",
  "pkg-web/package.json",
  "pkg-node/nanocodex.js",
  "pkg-node/nanocodex.d.ts",
  "pkg-node/package.json",
];

test("concurrent cold WASM package builds are serialized", async () => {
  const fixture = await createBuildFixture();
  try {
    await Promise.all([runBuild(fixture), runBuild(fixture)]);

    const events = (await readFile(fixture.buildEvents, "utf8")).trim().split("\n");
    assert.deepEqual(events, ["start", "end", "start", "end"]);
    for (const artifact of generatedArtifacts) {
      assert((await readFile(join(fixture.packageRoot, artifact))).length > 0, artifact);
    }
    for (const artifact of ["pkg-web/nanocodex.js", "pkg-web/nanocodex_bg.js", "pkg-node/nanocodex.js"]) {
      const glue = await readFile(join(fixture.packageRoot, artifact), "utf8");
      assert.ok(glue.includes("new Uint8Array(wasm.memory.buffer, ptr, len)"), artifact);
      assert.ok(!glue.includes("getUint8ArrayMemory0().subarray("), artifact);
    }
  } finally {
    await fixture.close();
  }
});

test("an incomplete Node package cache regenerates bindings", async () => {
  const fixture = await createBuildFixture();
  try {
    await runBuild(fixture, {
      CACHE_VALID: "0",
      NANOCODEX_WASM_BUILD_DELAY: "0",
      NANOCODEX_WASM_LOCK_HELD: fixture.repository,
    });
    await rm(fixture.bindgenEvents);
    await runBuild(fixture, {
      CACHE_VALID: "1",
      NANOCODEX_WASM_BUILD_DELAY: "0",
      NANOCODEX_WASM_LOCK_HELD: fixture.repository,
    });
    await assert.rejects(readFile(fixture.bindgenEvents), { code: "ENOENT" });

    const viewScript = join(fixture.repository, "js/nanocodex-vite/scripts/wasm-memory-views.mjs");
    await writeFile(viewScript, `${await readFile(viewScript, "utf8")}\n// Changed generator policy.\n`);
    await runBuild(fixture, {
      CACHE_VALID: "1",
      NANOCODEX_WASM_BUILD_DELAY: "0",
      NANOCODEX_WASM_LOCK_HELD: fixture.repository,
    });
    assert.deepEqual((await readFile(fixture.bindgenEvents, "utf8")).trim().split("\n"),
      ["nodejs", "web", "bundler"], "memory-view changes must invalidate generated bindings");

    for (const artifact of ["pkg-node/nanocodex.d.ts", "pkg-node/package.json"]) {
      await Promise.all([
        rm(join(fixture.packageRoot, artifact)),
        rm(fixture.bindgenEvents, { force: true }),
      ]);
      await runBuild(fixture, {
        CACHE_VALID: "1",
        NANOCODEX_WASM_BUILD_DELAY: "0",
        NANOCODEX_WASM_LOCK_HELD: fixture.repository,
      });
      assert.deepEqual(
        (await readFile(fixture.bindgenEvents, "utf8")).trim().split("\n"),
        ["nodejs", "web", "bundler"],
        `${artifact} must invalidate the generated package cache`,
      );
    }
  } finally {
    await fixture.close();
  }
});

async function createBuildFixture() {
  const temporary = await mkdtemp(join(tmpdir(), "nanocodex-wasm-build-"));
  const repository = join(temporary, "repository");
  const scripts = join(repository, "js/nanocodex-vite/scripts");
  const packageRoot = join(repository, "js/nanocodex");
  const fakeBin = join(temporary, "bin");
  const buildScript = join(scripts, "build-js-package.sh");
  const buildEvents = join(temporary, "build-events.log");
  const bindgenEvents = join(temporary, "bindgen-events.log");
  const target = join(repository, "target");

  await Promise.all([
    mkdir(scripts, { recursive: true }),
    mkdir(join(packageRoot, "pkg-node"), { recursive: true }),
    mkdir(join(packageRoot, "pkg-web"), { recursive: true }),
    mkdir(fakeBin, { recursive: true }),
  ]);
  await copyFile(sourceBuildScript, buildScript);
  await copyFile(new URL("../scripts/wasm-memory-views.mjs", import.meta.url), join(scripts, "wasm-memory-views.mjs"));
  await chmod(buildScript, 0o755);
  await Promise.all([
    writeExecutable(join(fakeBin, "cargo"), `#!/bin/sh
printf 'start\\n' >> "$NANOCODEX_WASM_BUILD_EVENTS"
sleep "$NANOCODEX_WASM_BUILD_DELAY"
artifact="$CARGO_TARGET_DIR/wasm32-unknown-unknown/debug/nanocodex_wasm.wasm"
mkdir -p "$(dirname "$artifact")"
printf 'fixture wasm\\n' > "$artifact"
printf 'end\\n' >> "$NANOCODEX_WASM_BUILD_EVENTS"
`),
    writeExecutable(join(fakeBin, "wasm-bindgen"), `#!/bin/sh
if [ "$1" = "--version" ]; then
  printf 'wasm-bindgen-fixture\\n'
  exit 0
fi
target=""
output=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --target) shift; target="$1" ;;
    --out-dir) shift; output="$1" ;;
  esac
  shift
done
mkdir -p "$output"
printf 'fixture wasm\\n' > "$output/nanocodex_bg.wasm"
cat > "$output/nanocodex.js" <<'GLUE'
getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
getUint8ArrayMemory0().subarray(ptr, ptr + len);
getUint8ArrayMemory0().subarray(ptr, ptr + buf.length);
getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
GLUE
printf 'fixture types\\n' > "$output/nanocodex.d.ts"
cp "$output/nanocodex.js" "$output/nanocodex_bg.js"
printf '%s\\n' "$target" >> "$NANOCODEX_WASM_BINDGEN_EVENTS"
`),
    writeExecutable(join(fakeBin, "node"), `#!/bin/sh
case "$1" in
  *wasm-memory-views.mjs)
    exec '${process.execPath.replaceAll("'", "'\\''")}' "$@"
    ;;
  *write-package-types.mjs)
    printf '{"type":"commonjs"}\\n' > js/nanocodex/pkg-node/package.json
    printf '{"type":"module"}\\n' > js/nanocodex/pkg-web/package.json
    ;;
  *write-wasm-attestation.mjs)
    if [ "$2" = "--check-cache" ] && [ "$CACHE_VALID" != "1" ]; then
      exit 1
    fi
    ;;
esac
`),
  ]);

  return {
    bindgenEvents,
    buildEvents,
    buildScript,
    packageRoot,
    repository,
    env: {
      ...process.env,
      CACHE_VALID: "0",
      CARGO_TARGET_DIR: target,
      NANOCODEX_WASM_BINDGEN_EVENTS: bindgenEvents,
      NANOCODEX_WASM_BUILD_DELAY: "0.15",
      NANOCODEX_WASM_BUILD_EVENTS: buildEvents,
      PATH: `${fakeBin}:${process.env.PATH}`,
    },
    close: () => rm(temporary, { recursive: true, force: true }),
  };
}

async function writeExecutable(path, contents) {
  await writeFile(path, contents);
  await chmod(path, 0o755);
}

function runBuild(fixture, environment = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(fixture.buildScript, [], {
      cwd: fixture.repository,
      env: { ...fixture.env, ...environment },
      stdio: ["ignore", "ignore", "pipe"],
    });
    let stderr = "";
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0) resolve();
      else reject(new Error(`WASM build exited with ${code ?? signal}: ${stderr}`));
    });
  });
}
