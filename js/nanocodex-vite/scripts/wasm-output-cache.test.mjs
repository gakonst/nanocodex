import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { test } from "node:test";
import { check, fingerprintInputs, save } from "./wasm-output-cache.mjs";

const repository = new URL("../../../", import.meta.url);
const scripts = ["js/nanocodex-vite/scripts/build-js-package.sh", "js/nanocodex-vite/scripts/wasm-output-cache.mjs", "js/nanocodex-vite/scripts/wasm-memory-views.mjs", "js/nanocodex/scripts/deduplicate-wasm.mjs", "js/nanocodex/scripts/write-package-types.mjs", "js/nanocodex/scripts/write-wasm-attestation.mjs", "js/nanocodex/scripts/check-managed-wasm.mjs"];
scripts.push("js/nanocodex-tools/scripts/sync-code-tools.mjs");
for (const name of ["code-tools.mjs", "code-values.mjs", "code-discovery.mjs"]) {
  scripts.push(`js/nanocodex-tools/runtime/${name}`, `crates/nanocodex-oai-tools/src/code_mode/${name}`);
}
const pkg = (name, extra = "") => `[package]\nname = "${name}"\nversion = "0.0.0"\nedition = "2021"\n${extra}`;
const turbo = (crates) => JSON.stringify({ tasks: { "nanocodex#build": { inputs: ["$TURBO_DEFAULT$", "$TURBO_ROOT$/Cargo.toml", "$TURBO_ROOT$/Cargo.lock", "$TURBO_ROOT$/.cargo/**", "$TURBO_ROOT$/js/nanocodex-vite/scripts/**", ...crates.map((crate) => `$TURBO_ROOT$/crates/${crate}/**`)] } } });

async function fixture(t) {
  const root = await mkdtemp(resolve(tmpdir(), "nanocodex-wasm-cache-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const put = async (path, value) => {
    await mkdir(resolve(root, path, ".."), { recursive: true });
    await writeFile(resolve(root, path), value);
  };
  for (const path of scripts) await put(path, await readFile(new URL(path, repository)));
  await put("Cargo.toml", '[workspace]\nmembers = ["js/nanocodex", "crates/*"]\n[workspace.dependencies]\ncore = { path = "crates/core" }\n');
  await put("Cargo.lock", "version = 4\n");
  await put("js/nanocodex/Cargo.toml", pkg("wasm", "[dependencies]\ncore.workspace = true\n"));
  await put("js/nanocodex/src/lib.rs", "// wasm source");
  await put("js/nanocodex/package.json", '{"devDependencies":{"binaryen":"132.0.0"}}');
  await put("crates/core/Cargo.toml", pkg("core"));
  await put("crates/core/src/lib.rs", "// core source");
  await put("crates/nanocodex-oai-tools/Cargo.toml", pkg("nanocodex-oai-tools"));
  await put("crates/nanocodex-oai-tools/src/lib.rs", "// embedded assets only in this fixture");
  await put("crates/host/Cargo.toml", pkg("host"));
  await put("crates/host/src/lib.rs", "// host source");
  await put(".cargo/config.toml", "# config");
  await put("turbo.json", turbo(["core"]));
  // Returns whether editing `path` changes the WASM key.
  const affects = async (path) => {
    const key = await fingerprintInputs(root);
    const before = await readFile(resolve(root, path)).catch(() => null);
    await put(path, `${before ?? ""}\n${path.endsWith(".toml") ? "#" : "//"} changed`);
    const changed = (await fingerprintInputs(root)) !== key;
    if (before === null) await rm(resolve(root, path)); else await put(path, before);
    return changed;
  };
  const build = () => execFileSync("bash", [resolve(root, scripts[0]), "--release"], { cwd: root, encoding: "utf8", stdio: "pipe", env: { ...process.env, NANOCODEX_WASM_LOCK_HELD: root } });
  return { root, put, affects, build };
}

test("content key, loud failures, and verified pre-Cargo reuse", async (t) => {
  const { root, put, affects, build } = await fixture(t);
  const key = await fingerprintInputs(root);
  assert.equal(await fingerprintInputs(root, "release", { ...process.env, RUSTUP_TOOLCHAIN: "1.97", CARGO_PROFILE_DEV_DEBUG: "0", CARGO_INCREMENTAL: "0" }), key);
  assert.notEqual(await fingerprintInputs(root, "development"), key);
  assert.notEqual(await fingerprintInputs(root, "release", { RUSTFLAGS: "-C opt-level=1" }), key);
  assert.notEqual(await fingerprintInputs(root, "release", { CARGO_PROFILE_WASM_OPT_LEVEL: "s" }), key);
  for (const path of ["crates/core/src/lib.rs", "crates/core/Cargo.toml", "Cargo.toml", "Cargo.lock", ".cargo/config.toml", "js/nanocodex-vite/scripts/wasm-memory-views.mjs"]) assert.ok(await affects(path), path);
  assert.equal(await affects("js/nanocodex/cloudflare/worker.mjs"), false);

  const embedded = "crates/nanocodex-oai-tools/src/code_mode/code-discovery.mjs";
  const originalEmbedded = await readFile(resolve(root, embedded));
  await put(embedded, "// stale discovery source");
  assert.throws(build, /Rust Code Mode code-discovery.mjs is stale/);
  await put(embedded, originalEmbedded);

  await put("turbo.json", turbo([]));
  assert.throws(build, /turbo\.json nanocodex#build inputs omit WASM inputs/);
  await put("turbo.json", turbo(["core"]));
  const manifest = await readFile(resolve(root, "crates/core/Cargo.toml"));
  await put("crates/core/Cargo.toml", "[invalid TOML");
  await assert.rejects(fingerprintInputs(root));
  assert.throws(build, /unclosed table/, "an unresolvable input set stops the build instead of rebuilding without a cache");
  await put("crates/core/Cargo.toml", manifest);

  for (const dir of ["pkg-web", "pkg-node"]) {
    for (const name of ["nanocodex.js", "nanocodex.d.ts", "package.json", ...(dir === "pkg-web" ? ["nanocodex_bg.js", "nanocodex_bg.wasm", "nanocodex_worker.js"] : [])]) await put(`js/nanocodex/${dir}/${name}`, name);
  }
  await put("raw.wasm", "raw WASM bytes");
  const git = (...args) => execFileSync("git", args, { cwd: root, stdio: "pipe" });
  git("init"); git("add", "."); git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.test", "commit", "-m", "fixture");
  execFileSync(process.execPath, [resolve(root, "js/nanocodex/scripts/write-wasm-attestation.mjs"), resolve(root, "raw.wasm")]);
  await save(root, "release", resolve(root, "raw.wasm"));
  await check(root);
  assert.match(build(), /skipped Cargo and binding generation/);
  for (const path of ["js/nanocodex/pkg-node/nanocodex.js", "js/nanocodex/pkg-web/nanocodex_bg.wasm", ".ci-wasm-cache/source.wasm", ".ci-wasm-cache/outputs.json"]) {
    const before = await readFile(resolve(root, path));
    await put(path, "corrupted");
    await assert.rejects(check(root), path);
    await rm(resolve(root, path));
    await assert.rejects(check(root), path);
    await put(path, before);
  }
  await put("crates/core/src/lib.rs", "// edited");
  await assert.rejects(check(root), /WASM inputs changed/);
});

test("input set follows Cargo's local dependency graph for wasm32", async (t) => {
  const { put, affects } = await fixture(t);
  const dependOnHost = (section) => put("js/nanocodex/Cargo.toml", pkg("wasm", `${section}\nhost = { path = "../../crates/host" }\n`));
  for (const [section, included] of [
    ["[target.'cfg(not(target_family = \"wasm\"))'.dependencies]", false],
    ["[target.'cfg(unix)'.dependencies]", false],
    ["[target.'wasm32-unknown-unknown'.dependencies]", true],
    ["[target.'cfg(any(unknown_flag, unix))'.dependencies]", true],
    ["[target.'cfg(unix)'.build-dependencies]", true],
    ["[dev-dependencies]", false],
  ]) {
    await dependOnHost(section);
    assert.equal(await affects("crates/host/src/lib.rs"), included, section);
  }
  // A proc macro compiles for the host, so its native-only dependencies still count.
  await put("js/nanocodex/Cargo.toml", pkg("wasm", "[dependencies]\ncore.workspace = true\n"));
  await put("crates/core/Cargo.toml", pkg("core", "[lib]\nproc-macro = true\n[target.'cfg(unix)'.dependencies]\nhost = { path = \"../host\" }\n"));
  assert.ok(await affects("crates/host/src/lib.rs"));
  await put("crates/core/Cargo.toml", pkg("core"));

  assert.equal(await affects("crates/core/tests/standalone.rs"), false);
  await put("crates/core/tests/prompt.txt", "embedded asset");
  await put("crates/core/src/lib.rs", 'const PROMPT: &str = include_str!("../tests/prompt.txt");');
  assert.ok(await affects("crates/core/tests/prompt.txt"), "literal includes are tracked");
  await put("crates/core/build.rs", "fn main() {}");
  assert.ok(await affects("crates/core/tests/standalone.rs"), "build scripts may read test assets");
});

// Exercise the shipped key command across the same process boundary as Actions:
// planning/cache lookup runs before the cache-miss path installs compiler tools.
test("WASM key survives compiler-cache setup but retains compiler and source inputs", async (t) => {
  const { root, put } = await fixture(t);
  const beforeSetup = { ...process.env };
  for (const name of ["RUSTC_WRAPPER", "CARGO_INCREMENTAL", "RUSTFLAGS"]) delete beforeSetup[name];
  const afterSetup = { ...beforeSetup, RUSTC_WRAPPER: "sccache", CARGO_INCREMENTAL: "0" };
  const command = await realpath(resolve(root, "js/nanocodex-vite/scripts/wasm-output-cache.mjs"));
  const key = (env) => execFileSync(process.execPath,
    [command, "key", "release"],
    { cwd: root, env, encoding: "utf8" }).trim();
  const planned = key(beforeSetup);
  assert.match(planned, /^[a-f0-9]{64}$/);
  assert.equal(key(afterSetup), planned, "compiler-cache setup must not invalidate a planned release or cache key");
  assert.notEqual(key({ ...afterSetup, RUSTC_WRAPPER: "custom-rustc-wrapper" }), planned);
  assert.notEqual(key({ ...afterSetup, RUSTFLAGS: "-C opt-level=1" }), planned);
  await put("crates/core/src/lib.rs", "// changed production source");
  assert.notEqual(key(afterSetup), planned);
});
