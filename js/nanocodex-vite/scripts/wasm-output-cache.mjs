// Needs Node and Cargo (`cargo metadata`); no Rust target or pnpm setup.
import { execFileSync } from "node:child_process";
import assert from "node:assert/strict";
import { createHash, randomUUID } from "node:crypto";
import { existsSync, readFileSync, realpathSync } from "node:fs";
import { mkdir, readFile, readdir, realpath, rename, rm, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { assertCachedManagedWasmAttestation, hashManagedWasmArtifacts } from "../../nanocodex/scripts/check-managed-wasm.mjs";

export const rustToolchain = "1.97";
const root = fileURLToPath(new URL("../../../", import.meta.url));
const metadataPath = ".ci-wasm-cache/outputs.json";
const rawPath = ".ci-wasm-cache/source.wasm";
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
const excluded = new Set([".git", "target", "node_modules", "pkg-web", "pkg-node"]);

async function walk(directory, skipStandaloneTests = false) {
  const result = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    if (excluded.has(entry.name) || (skipStandaloneTests && ["tests", "benches"].includes(entry.name))) continue;
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) result.push(...await walk(path));
    else if (entry.isFile() || entry.isSymbolicLink()) result.push(path);
  }
  return result;
}

// Three-valued cfg evaluation for wasm32-unknown-unknown: true, false, or null
// (unknown). Unknown predicates keep their dependencies in the input set.
function wasmCfg(platform) {
  const cfg = /^cfg\s*\(([\s\S]*)\)$/.exec(platform.trim());
  if (!cfg) return /^cfg\b/.test(platform.trim()) ? null : platform === "wasm32-unknown-unknown";
  const known = { target_arch: "wasm32", target_os: "unknown", target_family: "wasm", target_env: "", target_vendor: "unknown", target_pointer_width: "32", target_endian: "little" };
  // Reduce leaves to 1/0/?, then fold all/any/not from the inside out.
  let expression = cfg[1]
    .replace(/([A-Za-z_]\w*)\s*=\s*"([^"\\]*)"/g, (_, name, value) => (Object.hasOwn(known, name) ? (known[name] === value ? "1" : "0") : "?"))
    .replace(/\b([A-Za-z_]\w*)\b(?!\s*\()/g, (_, name) => (name === "unix" || name === "windows" ? "0" : "?"));
  for (let previous; previous !== expression;) {
    previous = expression;
    expression = expression.replace(/\b([A-Za-z_]\w*)\s*\(\s*((?:[01?]\s*,?\s*)*)\)/g, (_, name, list) => {
      const values = list.split(",").map((value) => value.trim()).filter(Boolean);
      if (name === "all") return values.includes("0") ? "0" : values.includes("?") ? "?" : "1";
      if (name === "any") return values.includes("1") ? "1" : values.includes("?") ? "?" : "0";
      if (name === "not" && values.length === 1) return { 1: "0", 0: "1", "?": "?" }[values[0]];
      return "?";
    });
  }
  return { 1: true, 0: false }[expression.trim()] ?? null;
}

// Local packages the WASM crate can compile, from Cargo's own manifest view.
// Only target tables proven inapplicable to wasm32 are skipped; optional,
// build, and proc-macro dependencies stay included. Cargo.lock pins the rest.
function dependencyDirectories(repository) {
  const { packages } = JSON.parse(execFileSync("cargo", ["metadata", "--format-version", "1", "--no-deps", "--offline"],
    { cwd: repository, encoding: "utf8", maxBuffer: 64 << 20, stdio: ["ignore", "pipe", "pipe"] }));
  const byDirectory = new Map(packages.map((pkg) => [realpathSync(dirname(pkg.manifest_path)), pkg]));
  const visited = new Set();
  const inputs = new Map();
  const visit = (directory, target) => {
    directory = realpathSync(directory);
    const location = relative(repository, directory);
    if (location.startsWith("..") || isAbsolute(location)) throw new Error(`local Rust dependency ${directory} must remain inside repository`);
    const pkg = byDirectory.get(directory);
    if (!pkg) throw new Error(`local Rust dependency ${location} is not a workspace member`);
    const kinds = pkg.targets.flatMap((entry) => entry.kind);
    // A proc macro and its dependency tree compile for the build host.
    target &&= !kinds.includes("proc-macro");
    if (visited.has(`${target}:${directory}`)) return;
    visited.add(`${target}:${directory}`);
    const buildScript = kinds.includes("custom-build");
    // Explicit production entry points can live in normally test-only folders.
    const keepTests = buildScript || pkg.targets.some((entry) => !entry.kind.some((kind) => ["test", "bench", "example"].includes(kind))
      && ["tests", "benches"].includes(relative(directory, entry.src_path).split(/[\\/]/)[0]));
    inputs.set(directory, { directory, buildScript, skipStandaloneTests: !keepTests });
    for (const dependency of pkg.dependencies) {
      if (!dependency.path || dependency.kind === "dev") continue;
      // Build dependencies, including target-specific ones, are built for the host.
      const dependencyTarget = target && dependency.kind !== "build";
      if (dependencyTarget && dependency.target && wasmCfg(dependency.target) === false) continue;
      visit(dependency.path, dependencyTarget);
    }
  };
  visit(resolve(repository, "js/nanocodex"), true);
  return [...inputs.keys()].sort().map((directory) => inputs.get(directory));
}

// Every file whose content can change the WASM outputs, as sorted absolute paths.
async function inputFiles(repository) {
  const files = new Set();
  const omittedTestDirectories = new Set();
  for (const { directory, buildScript, skipStandaloneTests } of dependencyDirectories(repository)) {
    files.add(resolve(directory, "Cargo.toml"));
    if (skipStandaloneTests) for (const name of ["tests", "benches"]) omittedTestDirectories.add(resolve(directory, name));
    if (directory === resolve(repository, "js/nanocodex") && !buildScript) {
      for (const path of await walk(resolve(directory, "src"))) files.add(path);
      for (const name of ["build.rs", "README.md"]) {
        try { await readFile(resolve(directory, name)); files.add(resolve(directory, name)); }
        catch (error) { if (error.code !== "ENOENT") throw error; }
      }
    } else {
      for (const path of await walk(directory, skipStandaloneTests)) files.add(path);
    }
  }
  for (const name of ["Cargo.toml", "Cargo.lock", "js/nanocodex-vite/scripts/build-js-package.sh",
    "js/nanocodex-vite/scripts/wasm-output-cache.mjs", "js/nanocodex-vite/scripts/wasm-memory-views.mjs",
    "js/nanocodex-vite/scripts/native-binaryen.mjs",
    "js/nanocodex/scripts/deduplicate-wasm.mjs", "js/nanocodex/scripts/write-package-types.mjs",
    "js/nanocodex/scripts/write-wasm-attestation.mjs", "js/nanocodex/scripts/check-managed-wasm.mjs"]) files.add(resolve(repository, name));
  for (const name of [".cargo", "rust-toolchain", "rust-toolchain.toml"]) {
    try {
      if (name === ".cargo") for (const path of await walk(resolve(repository, name))) files.add(path);
      else { await readFile(resolve(repository, name)); files.add(resolve(repository, name)); }
    } catch (error) { if (error.code !== "ENOENT") throw error; }
  }
  // Rust literal include/#[path] references can leave their crate directory.
  // Follow those recursively while retaining checkout-relative content keys.
  for (const path of files) {
    if (!path.endsWith(".rs")) continue;
    const source = await readFile(path, "utf8");
    for (const match of source.matchAll(/(?:include(?:_str|_bytes)?!\s*\(\s*|#\[path\s*=\s*)(?:r(#{0,8}))?"([^"\n]+)"/g)) {
      const included = resolve(dirname(path), match[2]);
      assert.ok(!relative(repository, included).startsWith(".."), "Rust include must remain inside repository");
      await readFile(included);
      files.add(included);
      // A production Rust module in tests/ can itself use ordinary mod children.
      // Retain that subtree rather than approximating Rust module resolution.
      if (included.endsWith(".rs")) for (const directory of omittedTestDirectories) {
        if (!relative(directory, included).startsWith("..")) {
          for (const path of await walk(directory)) files.add(path);
          omittedTestDirectories.delete(directory);
        }
      }
    }
  }
  return [...files].sort();
}

// Resolution errors propagate: an unprovable input set fails the build loudly.
export async function fingerprintInputs(repository = root, mode = "release", environment = process.env) {
  repository = await realpath(repository);
  assert.ok(["release", "development"].includes(mode));
  const files = await inputFiles(repository);
  const pkg = JSON.parse(await readFile(resolve(repository, "js/nanocodex/package.json"), "utf8"));
  const buildEnvironment = Object.fromEntries(Object.entries(environment)
    .filter(([name]) => /^(RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|RUSTC|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|CARGO_INCREMENTAL|CARGO_PROFILE_.*|CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_.*)$/.test(name))
    // CI installs this transparent compiler cache only after a cache miss.
    // Its presence cannot change either the planned release or the output key;
    // custom wrappers remain inputs because they may transform compilation.
    .filter(([name, value]) => name !== "RUSTC_WRAPPER" || value !== "sccache")
    // cargo +1.97 overrides RUSTUP_TOOLCHAIN. Dev/test overrides do not affect
    // --profile wasm, and rust-toolchain setup commonly introduces them.
    .filter(([name, value]) => name !== "CARGO_INCREMENTAL" || value !== (mode === "release" ? "0" : "1"))
    .filter(([name]) => !name.startsWith("CARGO_PROFILE_") || (mode === "release"
      ? /^CARGO_PROFILE_(WASM|RELEASE)_/.test(name)
      : /^CARGO_PROFILE_DEV_/.test(name))).sort());
  const hash = createHash("sha256");
  hash.update(JSON.stringify({ schema: 1, mode, rustToolchain, bindgen: "0.2.126", binaryen: pkg.devDependencies.binaryen, buildEnvironment }));
  for (const path of files) hash.update(JSON.stringify([relative(repository, path), sha(await readFile(path))]));
  return hash.digest("hex");
}

async function outputs(repository) {
  const web = pathToFileURL(`${resolve(repository, "js/nanocodex/pkg-web")}/`);
  const artifacts = await hashManagedWasmArtifacts(web);
  const node = {};
  for (const name of ["nanocodex.js", "nanocodex.d.ts", "package.json"]) node[name] = sha(await readFile(resolve(repository, "js/nanocodex/pkg-node", name)));
  return { artifacts, node, sourceWasmSha256: sha(await readFile(resolve(repository, rawPath))) };
}

// Resolution errors propagate before the try, so they are never mistaken for a miss.
export async function check(repository = root, mode = "release") {
  const key = await fingerprintInputs(repository, mode);
  try {
    const retained = JSON.parse(await readFile(resolve(repository, metadataPath), "utf8"));
    assert.equal(retained.schema, 1);
    assert.equal(retained.fingerprint, key, "WASM inputs changed");
    const current = await outputs(repository);
    assert.deepEqual(retained.outputs, current, "outputs do not match retained metadata");
    assertCachedManagedWasmAttestation(JSON.parse(await readFile(resolve(repository, "js/nanocodex/pkg-web/nanocodex-build.json"), "utf8")), current);
  } catch (error) { throw new CacheMiss(error.message.split("\n")[0]); }
}

export class CacheMiss extends Error {}

// turbo.json must hash every WASM input into nanocodex#build, or cached
// downstream tasks (Worker bundles embedding the WASM) replay stale outputs
// after a Rust edit. Supported input forms: $TURBO_DEFAULT$ (the package),
// $TURBO_ROOT$/<file>, and $TURBO_ROOT$/<directory>/**.
export async function assertTurboInputs(repository = root) {
  repository = await realpath(repository);
  const inputs = JSON.parse(await readFile(resolve(repository, "turbo.json"), "utf8")).tasks?.["nanocodex#build"]?.inputs ?? [];
  const covered = inputs.map((input) => {
    if (input === "$TURBO_DEFAULT$") return "js/nanocodex/**";
    if (!input.startsWith("$TURBO_ROOT$/")) throw new Error(`unsupported nanocodex#build input ${input}`);
    const path = input.slice("$TURBO_ROOT$/".length);
    if (/[*?[{!]/.test(path.replace(/\/\*\*$/, ""))) throw new Error(`unsupported nanocodex#build input glob ${input}`);
    return path;
  });
  const missing = (await inputFiles(repository)).map((path) => relative(repository, path))
    .filter((path) => !covered.some((input) => input.endsWith("/**") ? path.startsWith(input.slice(0, -2)) : path === input));
  if (missing.length) {
    throw new Error(`turbo.json nanocodex#build inputs omit WASM inputs; add them (for example $TURBO_ROOT$/<crate>/**): ${missing.slice(0, 10).join(", ")}${missing.length > 10 ? ", ..." : ""}`);
  }
}

async function atomicWrite(path, bytes) {
  const temporary = `${path}.${randomUUID()}.tmp`;
  try { await writeFile(temporary, bytes, { flag: "wx" }); await rename(temporary, path); }
  finally { await rm(temporary, { force: true }); }
}

export async function save(repository = root, mode = "release", source) {
  // Remove pre-release cache locations so older local builds cannot publish raw WASM.
  for (const name of [".nanocodex-source.wasm", ".nanocodex-output-cache.json"]) {
    await rm(resolve(repository, "js/nanocodex/pkg-web", name), { force: true });
  }
  await mkdir(resolve(repository, ".ci-wasm-cache"), { recursive: true });
  await writeFile(resolve(repository, ".ci-wasm-cache/.gitignore"), "*\n");
  await atomicWrite(resolve(repository, rawPath), await readFile(source));
  await atomicWrite(resolve(repository, metadataPath), `${JSON.stringify({ schema: 1, fingerprint: await fingerprintInputs(repository, mode), outputs: await outputs(repository) })}\n`);
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const [command, mode = "release", source] = process.argv.slice(2);
  if (command === "key") console.log(await fingerprintInputs(root, mode));
  else if (command === "check") {
    // Exit 1 is an ordinary miss (rebuild). Resolution failures exit 2 so the
    // build stops instead of treating an unprovable input set as a miss.
    try { await check(root, mode); console.log("WASM output cache verified"); }
    catch (error) {
      if (!(error instanceof CacheMiss)) { console.error("WASM input fingerprint failed:", error); process.exit(2); }
      console.error(`WASM output cache miss: ${error.message}`);
      process.exitCode = 1;
    }
  } else if (command === "check-turbo") await assertTurboInputs(root);
  else if (command === "save" && source) await save(root, mode, resolve(source));
  else throw new Error("usage: wasm-output-cache.mjs key|check|check-turbo [release|development], or save <mode> <raw-wasm>");
}
