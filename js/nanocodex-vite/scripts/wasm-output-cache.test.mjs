import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import { test } from "node:test";
import { check, fingerprint, save } from "./wasm-output-cache.mjs";

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
