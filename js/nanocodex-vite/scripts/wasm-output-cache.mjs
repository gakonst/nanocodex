// Uses Node built-ins and Python 3.11+ tomllib; no Rust or pnpm setup needed.
import { execFileSync } from "node:child_process";
import assert from "node:assert/strict";
import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, readdir, realpath, rename, rm, writeFile } from "node:fs/promises";
import { dirname, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { assertCachedManagedWasmAttestation, hashManagedWasmArtifacts } from "../../nanocodex/scripts/check-managed-wasm.mjs";

export const rustToolchain = "1.97";
const root = fileURLToPath(new URL("../../../", import.meta.url));
const metadataPath = ".ci-wasm-cache/outputs.json";
const rawPath = ".ci-wasm-cache/source.wasm";
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");
const excluded = new Set([".git", "target", "node_modules", "pkg-web", "pkg-node"]);

async function walk(directory) {
  const result = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    if (excluded.has(entry.name)) continue;
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) result.push(...await walk(path));
    else if (entry.isFile() || entry.isSymbolicLink()) result.push(path);
  }
  return result;
}

// Include the local dependency closure conservatively across features/targets.
// Registry/git dependencies are pinned by Cargo.lock. No Cargo metadata call.
function dependencyDirectories(repository) {
  // Parse the entire closure in one Python process. tomllib handles all valid
  // Cargo TOML key/table/string layouts instead of approximating that grammar.
  return JSON.parse(execFileSync("python3", ["-c", String.raw`
import json, pathlib, sys, tomllib
root = pathlib.Path(sys.argv[1]).resolve()
def manifest(directory):
    with (directory / "Cargo.toml").open("rb") as file:
        return tomllib.load(file)
workspace = manifest(root).get("workspace", {}).get("dependencies", {})
visited = set()
def visit(directory):
    directory = directory.resolve()
    if not directory.is_relative_to(root):
        raise ValueError("local Rust dependencies must remain inside repository")
    if directory in visited:
        return
    visited.add(directory)
    data = manifest(directory)
    tables = [data, *data.get("target", {}).values()]
    for table in tables:
        for kind in ("dependencies", "build-dependencies"):
            for name, dependency in table.get(kind, {}).items():
                if not isinstance(dependency, dict):
                    continue
                base = directory
                if dependency.get("workspace"):
                    dependency = workspace[name]
                    base = root
                if isinstance(dependency, dict) and "path" in dependency:
                    visit(base / dependency["path"])
visit(root / "js/nanocodex")
print(json.dumps(sorted(str(path) for path in visited)))
`, repository], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }));
}

export async function fingerprint(repository = root, mode = "release", environment = process.env) {
  try { return await fingerprintInputs(repository, mode, environment); }
  catch (error) {
    // A new unsupported manifest/include cannot accidentally reuse stale output.
    // A unique key still permits the ordinary build and its cache-save step.
    console.error(`WASM cache reuse disabled: ${error.stderr?.toString().trim().split("\n").at(-1) || error.message.split("\n")[0]}`);
    return sha(randomUUID());
  }
}

async function fingerprintInputs(repository = root, mode = "release", environment = process.env) {
  repository = await realpath(repository);
  assert.ok(["release", "development"].includes(mode));
  const files = new Set();
  for (const directory of dependencyDirectories(repository)) {
    files.add(resolve(directory, "Cargo.toml"));
    if (directory === resolve(repository, "js/nanocodex")) {
      for (const path of await walk(resolve(directory, "src"))) files.add(path);
      for (const name of ["build.rs", "README.md"]) {
        try { await readFile(resolve(directory, name)); files.add(resolve(directory, name)); }
        catch (error) { if (error.code !== "ENOENT") throw error; }
      }
    } else {
      for (const path of await walk(directory)) files.add(path);
    }
  }
  for (const name of ["Cargo.toml", "Cargo.lock", "js/nanocodex-vite/scripts/build-js-package.sh",
    "js/nanocodex-vite/scripts/wasm-output-cache.mjs", "js/nanocodex-vite/scripts/wasm-memory-views.mjs",
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
    }
  }
  const pkg = JSON.parse(await readFile(resolve(repository, "js/nanocodex/package.json"), "utf8"));
  const buildEnvironment = Object.fromEntries(Object.entries(environment)
    .filter(([name]) => /^(RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|RUSTC|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|CARGO_INCREMENTAL|CARGO_PROFILE_.*|CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_.*)$/.test(name))
    // cargo +1.97 overrides RUSTUP_TOOLCHAIN. Dev/test overrides do not affect
    // --profile wasm, and rust-toolchain setup commonly introduces them.
    .filter(([name, value]) => name !== "CARGO_INCREMENTAL" || value !== (mode === "release" ? "0" : "1"))
    .filter(([name]) => !name.startsWith("CARGO_PROFILE_") || (mode === "release"
      ? /^CARGO_PROFILE_(WASM|RELEASE)_/.test(name)
      : /^CARGO_PROFILE_DEV_/.test(name))).sort());
  const hash = createHash("sha256");
  hash.update(JSON.stringify({ schema: 1, mode, rustToolchain, bindgen: "0.2.126", binaryen: pkg.devDependencies.binaryen, buildEnvironment }));
  for (const path of [...files].sort()) hash.update(JSON.stringify([relative(repository, path), sha(await readFile(path))]));
  return hash.digest("hex");
}

async function outputs(repository) {
  const web = pathToFileURL(`${resolve(repository, "js/nanocodex/pkg-web")}/`);
  const artifacts = await hashManagedWasmArtifacts(web);
  const node = {};
  for (const name of ["nanocodex.js", "nanocodex.d.ts", "package.json"]) node[name] = sha(await readFile(resolve(repository, "js/nanocodex/pkg-node", name)));
  return { artifacts, node, sourceWasmSha256: sha(await readFile(resolve(repository, rawPath))) };
}

export async function check(repository = root, mode = "release") {
  const retained = JSON.parse(await readFile(resolve(repository, metadataPath), "utf8"));
  assert.equal(retained.schema, 1);
  assert.equal(retained.fingerprint, await fingerprint(repository, mode));
  const current = await outputs(repository);
  assert.deepEqual(retained.outputs, current);
  assertCachedManagedWasmAttestation(JSON.parse(await readFile(resolve(repository, "js/nanocodex/pkg-web/nanocodex-build.json"), "utf8")), current);
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
  await atomicWrite(resolve(repository, metadataPath), `${JSON.stringify({ schema: 1, fingerprint: await fingerprint(repository, mode), outputs: await outputs(repository) })}\n`);
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const [command, mode = "release", source] = process.argv.slice(2);
  if (command === "key") console.log(await fingerprint(root, mode));
  else if (command === "check") {
    try { await check(root, mode); console.log("WASM output cache verified"); }
    catch (error) { console.error(`WASM output cache miss: ${error.message}`); process.exitCode = 1; }
  } else if (command === "save" && source) await save(root, mode, resolve(source));
  else throw new Error("usage: wasm-output-cache.mjs key|check [release|development], or save <mode> <raw-wasm>");
}
