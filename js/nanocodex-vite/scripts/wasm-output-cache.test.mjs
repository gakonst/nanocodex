import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { test } from "node:test";
import { check, fingerprint, fingerprintInputs, save } from "./wasm-output-cache.mjs";

const repository = new URL("../../../", import.meta.url);
test("content key, attestation integrity, and pre-Cargo reuse", async () => {
  const root = await mkdtemp(resolve(tmpdir(), "nanocodex-wasm-cache-"));
  const put = async (path, value) => {
    await mkdir(resolve(root, path, ".."), { recursive: true });
    await writeFile(resolve(root, path), value);
  };
  try {
    const scripts = ["js/nanocodex-vite/scripts/build-js-package.sh", "js/nanocodex-vite/scripts/wasm-output-cache.mjs", "js/nanocodex-vite/scripts/wasm-memory-views.mjs", "js/nanocodex/scripts/deduplicate-wasm.mjs", "js/nanocodex/scripts/write-package-types.mjs", "js/nanocodex/scripts/write-wasm-attestation.mjs", "js/nanocodex/scripts/check-managed-wasm.mjs"];
    for (const path of scripts) await put(path, await readFile(new URL(path, repository)));
    await put("Cargo.toml", '[workspace.dependencies]\ncore = { path = "crates/core" }\n');
    await put("Cargo.lock", "locked");
    await put("js/nanocodex/Cargo.toml", '[dependencies]\ncore.workspace = true\n');
    await put("js/nanocodex/src/lib.rs", "wasm source");
    await put("crates/core/Cargo.toml", '[package]\nname = "core"\n');
    await put("crates/core/src/lib.rs", "core source");
    await put("js/nanocodex/package.json", '{"devDependencies":{"binaryen":"132.0.0"}}');
    await put(".cargo/config.toml", "# config");
    const key = await fingerprint(root);
    assert.equal(await fingerprintInputs(root), key);
    assert.equal(await fingerprint(root, "release", { ...process.env, RUSTUP_TOOLCHAIN: "1.97", CARGO_PROFILE_DEV_DEBUG: "0", CARGO_PROFILE_TEST_DEBUG: "0", CARGO_INCREMENTAL: "0" }), key);
    const manifestPath = "js/nanocodex/Cargo.toml";
    const oldManifest = await readFile(resolve(root, manifestPath));
    await put(manifestPath, '[dependencies.core]\nworkspace = true\n');
    const dottedKey = await fingerprint(root);
    await put("crates/core/src/lib.rs", "changed dotted dependency");
    assert.notEqual(await fingerprint(root), dottedKey);
    await put("crates/core/src/lib.rs", "core source");
    await put(manifestPath, oldManifest);
    await put("shared/prompt.md", "embedded prompt");
    await put("js/nanocodex/src/lib.rs", 'const PROMPT: &str = include_str!("../../../shared/prompt.md");');
    const includeKey = await fingerprint(root);
    await put("shared/prompt.md", "changed embedded prompt");
    assert.notEqual(await fingerprint(root), includeKey);
    await put("js/nanocodex/src/lib.rs", "wasm source");
    await put(manifestPath, '[dependencies."core"]\nworkspace = true\n');
    assert.equal(await fingerprint(root), await fingerprint(root), "quoted dependency table is supported");
    await put(manifestPath, '[dependencies]\n  core = { workspace = true }\n');
    const indentedKey = await fingerprint(root);
    await put("crates/core/src/lib.rs", "changed indented dependency");
    assert.notEqual(await fingerprint(root), indentedKey);
    await put("crates/core/src/lib.rs", "core source");
    await put(manifestPath, '[invalid TOML');
    await assert.rejects(fingerprintInputs(root), "release identity must surface resolution failures");
    assert.notEqual(await fingerprint(root), await fingerprint(root), "invalid manifest disables reuse");
    await put(manifestPath, oldManifest);
    await put("js/nanocodex/cloudflare/worker.mjs", "unrelated worker change");
    assert.equal(await fingerprint(root), key);
    for (const path of ["crates/core/src/lib.rs", "Cargo.lock", ".cargo/config.toml", "js/nanocodex-vite/scripts/wasm-memory-views.mjs"]) {
      const before = await readFile(resolve(root, path));
      await put(path, `${before}\nchanged`);
      assert.notEqual(await fingerprint(root), key, path);
      await put(path, before);
    }
    assert.notEqual(await fingerprint(root, "development"), key);
    assert.notEqual(await fingerprint(root, "release", { RUSTFLAGS: "-C opt-level=1" }), key);
    for (const dir of ["pkg-web", "pkg-node"]) {
      for (const name of ["nanocodex.js", "nanocodex.d.ts", "package.json", ...(dir === "pkg-web" ? ["nanocodex_bg.js", "nanocodex_bg.wasm", "nanocodex_worker.js"] : [])]) await put(`js/nanocodex/${dir}/${name}`, name);
    }
    await put("raw.wasm", "raw WASM bytes");
    const git = (...args) => execFileSync("git", args, { cwd: root, stdio: "pipe" });
    git("init"); git("add", "."); git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.test", "commit", "-m", "fixture");
    execFileSync(process.execPath, [resolve(root, "js/nanocodex/scripts/write-wasm-attestation.mjs"), resolve(root, "raw.wasm")]);
    await save(root, "release", resolve(root, "raw.wasm"));
    await check(root);
    // The lock is already held by the fixture; Cargo and wasm-bindgen must not run.
    const result = execFileSync("bash", [resolve(root, scripts[0]), "--release"], {
      cwd: root, encoding: "utf8", env: { ...process.env, NANOCODEX_WASM_LOCK_HELD: root },
    });
    assert.match(result, /skipped Cargo and binding generation/);
    for (const path of ["js/nanocodex/pkg-node/nanocodex.js", "js/nanocodex/pkg-web/nanocodex_bg.wasm", ".ci-wasm-cache/source.wasm"]) {
      const before = await readFile(resolve(root, path));
      await put(path, "corrupted");
      await assert.rejects(check(root));
      await put(path, before);
      await rm(resolve(root, path));
      await assert.rejects(check(root));
      await put(path, before);
    }
    await assert.rejects(check(root, "development"));
  } finally { await rm(root, { recursive: true, force: true }); }
});


test("WASM closure skips only proven target mismatches and preserves host dependencies", async () => {
  const fixture = await inputFixture();
  const { root, put } = fixture;
  try {
    const cases = [
      ['cfg(not(target_family = "wasm"))', false],
      ['cfg(unix)', false],
      ['cfg (unix)', false],
      ['cfg (not(unknown_flag))', true],
      ['cfg(windows)', false],
      ['x86_64-unknown-linux-gnu', false],
      ['wasm32-unknown-unknown', true],
      ['cfg(all(target_arch = "wasm32", target_os = "unknown"))', true],
      ['cfg(any(unix, target_family = "wasm"))', true],
      ['cfg(not(any(unix, windows)))', true],
      ['cfg(all(unknown_flag, unix))', false],
      ['cfg(any(unknown_flag, unix))', true],
      ['cfg(not(unknown_flag))', true],
      ['cfg(target_feature = "atomics")', true],
      ['cfg(feature = "optional-feature")', true],
      ['cfg(future_predicate(target_arch = "wasm32"))', true],
    ];
    for (const [platform, included] of cases) {
      await put("js/nanocodex/Cargo.toml", `[target.'${platform}'.dependencies]\ncore = { path = "../../crates/core", optional = true }\n`);
      const before = await fingerprintInputs(root);
      await put("crates/core/src/lib.rs", `changed for ${platform}`);
      assert.equal((await fingerprintInputs(root)) !== before, included, platform);
      await put("crates/core/src/lib.rs", "core source");
    }
    await put("js/nanocodex/Cargo.toml", '[dependencies]\ncore = { path = "../../crates/core", optional = true }\n');
    const optional = await fingerprintInputs(root);
    await put("crates/core/src/lib.rs", "optional dependency still included");
    assert.notEqual(await fingerprintInputs(root), optional, "do not infer optional feature activation");

    for (const kind of ["build-dependency", "proc-macro"]) {
      await put("js/nanocodex/Cargo.toml", kind === "build-dependency"
        ? `[target.'cfg(unix)'.build-dependencies]\ncore = { path = "../../crates/core" }\n`
        : '[dependencies]\ncore = { path = "../../crates/core" }\n');
      await put("crates/core/Cargo.toml", `[package]\nname = "core"\n${kind === "proc-macro" ? "[lib]\nproc-macro = true\n" : ""}[target.'cfg(unix)'.dependencies]\nhost = { path = "../host" }\n`);
      await put("crates/host/Cargo.toml", '[package]\nname = "host"\n');
      await put("crates/host/src/lib.rs", "host dependency source");
      const before = await fingerprintInputs(root);
      await put("crates/host/src/lib.rs", "host dependency changed");
      assert.notEqual(await fingerprintInputs(root), before, kind);
    }
  } finally { await fixture.close(); }
});

test("standalone test directories are omitted unless builds or production references need them", async () => {
  const fixture = await inputFixture();
  const { root, put } = fixture;
  try {
    const beforeTests = await fingerprintInputs(root);
    await put("crates/core/tests/standalone.rs", "integration test");
    await put("crates/core/benches/standalone.rs", "benchmark");
    assert.equal(await fingerprintInputs(root), beforeTests);
    await put("crates/core/tests/standalone.rs", "changed integration test");
    assert.equal(await fingerprintInputs(root), beforeTests);

    await put("crates/core/tests/prompt.txt", "embedded asset");
    await put("crates/core/src/lib.rs", 'const PROMPT: &str = include_str!("../tests/prompt.txt");');
    const beforeAsset = await fingerprintInputs(root);
    await put("crates/core/tests/prompt.txt", "changed embedded asset");
    assert.notEqual(await fingerprintInputs(root), beforeAsset, "literal includes retain excluded-directory assets");

    await put("crates/core/src/lib.rs", '#[path = "../tests/production.rs"] mod production;');
    await put("crates/core/tests/production.rs", "mod sibling;");
    await put("crates/core/tests/sibling.rs", "production child module");
    const beforeModule = await fingerprintInputs(root);
    await put("crates/core/tests/sibling.rs", "changed production child module");
    assert.notEqual(await fingerprintInputs(root), beforeModule, "referenced Rust modules retain their ordinary mod children");
    await put("crates/core/src/lib.rs", "core source");

    await put("crates/core/src/tests/production.rs", "nested source remains conservative");
    const beforeNested = await fingerprintInputs(root);
    await put("crates/core/src/tests/production.rs", "changed nested source");
    assert.notEqual(await fingerprintInputs(root), beforeNested);

    for (const build of ["build.rs", "custom-build.rs"]) {
      await put("crates/core/Cargo.toml", `[package]\nname = "core"\n${build === "build.rs" ? "" : 'build = "custom-build.rs"\n'}`);
      await put(`crates/core/${build}`, "fn main() {}");
      const before = await fingerprintInputs(root);
      await put("crates/core/tests/standalone.rs", `build input for ${build}`);
      assert.notEqual(await fingerprintInputs(root), before, "build scripts may read test assets");
      await rm(resolve(root, `crates/core/${build}`));
    }
    await put("crates/core/Cargo.toml", '[package]\nname = "core"\n[lib]\npath = "tests/library.rs"\n');
    await put("crates/core/tests/library.rs", "production library");
    const explicitTarget = await fingerprintInputs(root);
    await put("crates/core/tests/library.rs", "changed production library");
    assert.notEqual(await fingerprintInputs(root), explicitTarget, "explicit production entry points remain covered");
  } finally { await fixture.close(); }
});

async function inputFixture() {
  const root = await mkdtemp(resolve(tmpdir(), "nanocodex-wasm-inputs-"));
  const put = async (path, value) => {
    await mkdir(resolve(root, path, ".."), { recursive: true });
    await writeFile(resolve(root, path), value);
  };
  for (const path of ["js/nanocodex-vite/scripts/build-js-package.sh", "js/nanocodex-vite/scripts/wasm-output-cache.mjs", "js/nanocodex-vite/scripts/wasm-memory-views.mjs", "js/nanocodex/scripts/deduplicate-wasm.mjs", "js/nanocodex/scripts/write-package-types.mjs", "js/nanocodex/scripts/write-wasm-attestation.mjs", "js/nanocodex/scripts/check-managed-wasm.mjs"]) await put(path, "// build policy");
  await put("Cargo.toml", '[workspace]\n');
  await put("Cargo.lock", "locked");
  await put("js/nanocodex/Cargo.toml", '[dependencies]\ncore = { path = "../../crates/core" }\n');
  await put("js/nanocodex/package.json", '{"devDependencies":{"binaryen":"132.0.0"}}');
  await put("js/nanocodex/src/lib.rs", "wasm source");
  await put("crates/core/Cargo.toml", '[package]\nname = "core"\n');
  await put("crates/core/src/lib.rs", "core source");
  return { root, put, close: () => rm(root, { recursive: true, force: true }) };
}
