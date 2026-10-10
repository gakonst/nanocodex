// Behavioral gate for CI selection: real Git histories, a real Cargo workspace
// graph, and the actual CLI entry points used by ci.yml (`select`, `verify`).
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { families } from "./select-jobs.mjs";

const script = fileURLToPath(new URL("./select-jobs.mjs", import.meta.url));
const heavy = ["hands", "windows", "vm", "voice", "python", "rust_extra", "preview", "codeql"];
const only = (...on) => Object.fromEntries(families.map(name => [name, on.includes(name)]));
const everything = only(...families);

// A miniature workspace with the real job-root package names:
// Hand/VM consumers of OpenAI tools, independent Claude consumers, and a leaf.
function workspace(t) {
  const cwd = mkdtempSync(join(tmpdir(), "ci-selector-"));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const git = (...args) => execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
  const write = (path, content = "") => {
    mkdirSync(dirname(join(cwd, path)), { recursive: true });
    writeFileSync(join(cwd, path), content);
  };
  const crate = (dir, name, deps = {}) => {
    write(`${dir}/Cargo.toml`, `[package]\nname = "${name}"\nversion = "0.0.0"\nedition = "2021"\n[dependencies]\n`
      + Object.entries(deps).map(([dep, path]) => `${dep} = { path = "${path}" }\n`).join(""));
    write(`${dir}/src/lib.rs`);
  };
  write("Cargo.toml", `[workspace]\nresolver = "2"\nmembers = ["crates/*", "crates/nanocodex-oai-tools/macros", "bin/*"]\n`);
  crate("crates/oai-api", "nanocodex-oai-api");
  crate("crates/nanocodex-oai-tools/macros", "nanocodex-oai-tools-macros");
  crate("crates/nanocodex-oai-tools", "nanocodex-oai-tools", { "nanocodex-oai-tools-macros": "macros" });
  crate("crates/nanocodex-claude-tools", "nanocodex-claude-tools");
  crate("crates/nanocodex-claude", "nanocodex-claude", { "nanocodex-claude-tools": "../nanocodex-claude-tools" });
  crate("crates/vm", "nanocodex-vm", { "nanocodex-oai-api": "../oai-api", "nanocodex-oai-tools": "../nanocodex-oai-tools" });
  crate("crates/phone", "nanocodex-phone");
  crate("bin/nanocodex", "nanocodex-bin", { "nanocodex-vm": "../../crates/vm" });
  crate("bin/hand", "nanocodex-hand-daemon");
  write("README.md");
  execFileSync("cargo", ["generate-lockfile", "--offline"], { cwd, stdio: ["ignore", "pipe", "pipe"] });
  git("init", "-q");
  git("config", "user.name", "CI selector test");
  git("config", "user.email", "ci@example.invalid");
  const commit = () => { git("add", "-A"); git("commit", "-qm", "fixture"); return git("rev-parse", "HEAD"); };
  const initial = commit();
  const run = (args, env) => spawnSync(process.execPath, [script, ...args], {
    cwd, encoding: "utf8", env: { ...process.env, NANOCODEX_CI_TESTS: "", GITHUB_STEP_SUMMARY: "", GITHUB_REPOSITORY: "gakonst/nanocodex", ...env },
  });
  // Runs the selector as ci.yml does and parses its GITHUB_OUTPUT lines.
  const select = (eventName, event, env = {}) => {
    const eventPath = join(cwd, ".git", "event.json");
    const outputPath = join(cwd, ".git", "output");
    writeFileSync(eventPath, JSON.stringify(event));
    writeFileSync(outputPath, "");
    const result = run([], { GITHUB_EVENT_NAME: eventName, GITHUB_EVENT_PATH: eventPath, GITHUB_OUTPUT: outputPath, ...env });
    assert.equal(result.status, 0, result.stderr);
    const raw = Object.fromEntries(readFileSync(outputPath, "utf8").trim().split("\n").map(line => line.split(/=(.*)/s).slice(0, 2)));
    return { raw, jobs: Object.fromEntries(families.map(name => [name, raw[name] === "true"])) };
  };
  return { cwd, git, write, commit, initial, select, run };
}

test("changed crates select only the jobs in their reverse-dependency closure", t => {
  const w = workspace(t);
  const push = (...files) => {
    const before = w.git("rev-parse", "HEAD");
    for (const file of files) w.write(file, `// ${Math.random()}\n`);
    return w.select("push", { before, after: w.commit() });
  };
  // A shared library reaches the Hand, Windows, and VM jobs through its dependents.
  const shared = push("crates/oai-api/src/client.rs");
  assert.deepEqual(shared.jobs, only("hands", "windows", "vm", "rust", "rust_extra", "policy"));
  assert.equal(shared.raw.packages, "nanocodex-bin nanocodex-oai-api nanocodex-vm");
  // A leaf crate runs Rust quality for itself and nothing native.
  const leaf = push("crates/phone/src/lib.rs");
  assert.deepEqual(leaf.jobs, only("rust", "rust_extra", "policy"));
  assert.equal(leaf.raw.packages, "nanocodex-phone");
  // Documentation needs no compilation; JS app sources need the WASM artifact.
  assert.deepEqual(push("docs/guide.md").jobs, only("policy"));
  assert.deepEqual(push("js/account/src/page.tsx").jobs, only("apps", "wasm", "policy"));
  // Backend-only private-input changes must exercise the shipped terminal too.
  for (const path of ["js/managed/src/browser-vault-save.ts", "js/managed/test/private-input-tui.chrome.mjs", "js/egress/src/vault-fields.ts"]) {
    assert.deepEqual(push(path).jobs, only("hands", "bindings", "wasm", "policy"), path);
  }
  assert.deepEqual(push("js/account/worker/managedProxy.ts").jobs, only("hands", "apps", "bindings", "wasm", "policy"));
  assert.equal(shared.raw.tests, "false", "tests stay paused unless the owner switch is on");
});

test("legacy SSH CLI and journey-only changes select shared Hands while drafts stay fast", t => {
  const w = workspace(t);
  for (const path of ["bin/nanocodex/src/login.rs", "bin/nanocodex/tests/ssh_import_cli_e2e.mjs"]) {
    const base = w.git("rev-parse", "HEAD");
    w.write(path, "// SSH import change\n");
    const head = w.commit();
    const expected = only("hands", "windows", "rust", "rust_extra", "policy");
    const push = w.select("push", { before: base, after: head });
    assert.deepEqual(push.jobs, expected, `${path}: push`);
    assert.equal(push.raw.packages, "nanocodex-bin", path);
    const pr = draft => ({ pull_request: { draft, base: { sha: base }, head: { sha: head } } });
    const ready = w.select("pull_request", pr(false));
    assert.deepEqual(ready.jobs, expected, `${path}: ready PR`);
    assert.equal(ready.raw.tests, "false", "CLI journeys remain selected while broader tests are paused");
    const draft = w.select("pull_request", pr(true));
    assert.deepEqual(draft.jobs, only("rust", "policy"), `${path}: draft PR`);
    assert.equal(draft.raw.heavy, "false", path);
  }
});

test("provider split retains Rust ownership of the unchanged npm Code Mode asset", t => {
  const w = workspace(t);
  w.write("js/nanocodex-tools/runtime/code-tools.mjs", "export const changed = true;\n");
  const asset = w.select("push", { before: w.initial, after: w.commit() });
  for (const family of ["hands", "windows", "vm", "rust", "rust_extra", "apps", "bindings", "preview", "wasm", "policy"]) {
    assert.equal(asset.jobs[family], true, family);
  }
  assert.equal(asset.raw.packages, "nanocodex-bin nanocodex-oai-tools nanocodex-vm");
  const before = w.git("rev-parse", "HEAD");
  w.write("crates/nanocodex-claude-tools/src/lib.rs", "// independent Claude adapters\n");
  const claude = w.select("push", { before, after: w.commit() });
  assert.deepEqual(claude.jobs, only("rust", "rust_extra", "policy"));
  assert.equal(claude.raw.packages, "nanocodex-claude nanocodex-claude-tools");
  const macroBefore = w.git("rev-parse", "HEAD");
  w.write("crates/nanocodex-oai-tools/macros/src/lib.rs", "// renamed procedural macro\n");
  const macro = w.select("push", { before: macroBefore, after: w.commit() });
  assert.deepEqual(macro.jobs, only("hands", "windows", "vm", "rust", "rust_extra", "policy"));
  assert.equal(macro.raw.packages, "nanocodex-bin nanocodex-oai-tools nanocodex-oai-tools-macros nanocodex-vm");
});

test("workspace-wide inputs, deleted crates, and unknown paths fail open", t => {
  const w = workspace(t);
  for (const path of ["Cargo.lock", ".github/workflows/ci.yml", "scripts/ci/select-jobs.mjs", "crates/gone/src/lib.rs", "unknown.txt"]) {
    w.git("checkout", "-q", "--detach", w.initial);
    w.write(path, path);
    const result = w.select("push", { before: w.initial, after: w.commit() });
    assert.deepEqual(result.jobs, everything, path);
    assert.equal(result.raw.packages, "*", path);
  }
});

test("PRs diff against the merge base; drafts keep only the fast lane", t => {
  const w = workspace(t);
  w.git("checkout", "-q", "-b", "feature");
  w.write("crates/vm/src/lib.rs", "// feature\n");
  const head = w.commit();
  w.git("checkout", "-q", "-b", "base", w.initial);
  w.write("Cargo.lock", readFileSync(join(w.cwd, "Cargo.lock"), "utf8") + "# base-only change must not leak into the PR diff\n");
  const base = w.commit();
  const pr = draft => ({ pull_request: { draft, base: { sha: base }, head: { sha: head } } });
  const ready = w.select("pull_request", pr(false));
  assert.deepEqual(ready.jobs, only("hands", "windows", "vm", "rust", "rust_extra", "policy"));
  assert.equal(ready.raw.heavy, "true");
  const draft = w.select("pull_request", pr(true));
  assert.deepEqual(draft.jobs, only("rust", "policy"));
  assert.equal(draft.raw.heavy, "false");
  // Unsupported events and unusable endpoints run everything; merge-queue
  // candidates skip only the PR package preview.
  for (const [name, event] of [["schedule", {}], ["push", { before: "0".repeat(40), after: head }]]) {
    assert.deepEqual(w.select(name, event).jobs, everything, name);
  }
  assert.deepEqual(w.select("merge_group", {}).jobs, { ...everything, preview: false });
  assert.equal(w.select("schedule", {}, { NANOCODEX_CI_TESTS: "on" }).raw.tests, "true");
});

test("ci success accepts reduced matrices and rejects failures, cancellations, and unplanned skips", t => {
  const w = workspace(t);
  const outputs = selected => ({ ...Object.fromEntries(families.map(name => [name, String(selected.includes(name))])), tests: "false" });
  const jobs = {
    test: [], "shared-hands": ["hands"], "voice-native": ["voice"], "windows-hand": ["windows"], clippy: ["rust"],
    "rust-extra": ["rust_extra"], "vm-guest": ["vm"], policy: ["policy"], "wasm-build": ["wasm"], "js-preview": ["preview"],
    "wasm-quality": ["wasm_rust"], bindings: ["bindings"], managed: [], python: ["python"], apps: ["apps"], codeql: ["codeql"],
  };
  const needs = selected => ({
    changes: { result: "success", outputs: outputs(selected) },
    ...Object.fromEntries(Object.entries(jobs).map(([job, [family]]) => [job, { result: selected.includes(family) ? "success" : "skipped" }])),
  });
  const passes = value => w.run(["verify"], { NEEDS: JSON.stringify(value) }).status === 0;
  const draft = families.filter(name => !heavy.includes(name));
  for (const selected of [families, draft, ["policy"]]) assert.ok(passes(needs(selected)), selected.join(","));
  for (const job of ["changes", "clippy", "windows-hand"]) {
    for (const result of ["failure", "cancelled", "skipped"]) {
      assert.equal(passes({ ...needs(families), [job]: { ...needs(families)[job], result } }), false, `${job}: ${result}`);
    }
  }
  assert.equal(passes({ ...needs(["policy"]), "windows-hand": { result: "success" } }), false, "unselected job ran");
  assert.equal(passes({ ...needs(families), test: { result: "success" } }), false, "paused tests ran");
  const on = selected => ({ ...needs(selected), changes: { result: "success", outputs: { ...outputs(selected), tests: "true" } } });
  assert.equal(passes(on(["hands"])), false, "Hand changes with tests on require the Docker Hand job");
  assert.ok(passes({ ...on(["hands"]), "vm-guest": { result: "success" } }), "Hand changes with tests on run vm-guest");
  assert.ok(passes({ ...on(families), test: { result: "success" }, managed: { result: "success" } }), "tests on run the workspace and managed test jobs");
  assert.equal(passes({ ...on(families), test: { result: "success" } }), false, "tests on require the managed shards");
  assert.equal(passes({ ...needs(families), changes: { result: "success", outputs: {} } }), false, "missing selection");
  assert.equal(passes({ ...needs(families), "new-job": { result: "success" } }), false, "unmapped job");
});

test("draft service changes retain their HTTP and browser consumers while general tests stay paused", t => {
  const w = workspace(t);
  for (const [path, expected] of [
    ["scripts/cloudflare/release-workers.mjs", only("bindings", "wasm", "policy")],
    ["scripts/cloudflare/deploy-workers.test.mjs", only("bindings", "wasm", "policy")],
    ["js/account/worker/index.ts", only("apps", "bindings", "wasm", "policy")],
    ["js/account/worker/managedProxy.ts", only("apps", "bindings", "wasm", "policy")],
    ["js/managed/src/browser-runtime.ts", only("apps", "bindings", "wasm", "policy")],
    ["js/managed/src/browser-vault-totp.ts", only("apps", "bindings", "wasm", "policy")],
    ["js/managed/test/browser-vault-totp.chrome.mjs", only("apps", "bindings", "wasm", "policy")],
    ["js/egress/src/phone-service.ts", only("apps", "bindings", "wasm", "policy")],
    ["js/egress/src/vault-totp.ts", only("apps", "bindings", "wasm", "policy")],
    ["js/managed/test/phone-stack-journey.test.mjs", only("bindings", "wasm", "policy")],
    ["js/nanocodex/services/index.mjs", only("apps", "bindings", "wasm", "policy")],
    ["js/account/src/PhoneService.tsx", only("apps", "wasm", "policy")],
  ]) {
    w.git("checkout", "-q", "--detach", w.initial);
    w.write(path, "// changed service consumer\n");
    const head = w.commit();
    const selected = w.select("pull_request", { pull_request: { draft: true, base: { sha: w.initial }, head: { sha: head } } }, { NANOCODEX_CI_TESTS: "paused" });
    assert.deepEqual(selected.jobs, expected, path);
    assert.equal(selected.raw.tests, "false", path);
    assert.equal(selected.raw.heavy, "false", path);
  }
});

test("a Hand daemon change selects the Linux/macOS and Windows Hand jobs", t => {
  const w = workspace(t);
  const before = w.git("rev-parse", "HEAD");
  w.write("bin/hand/src/lib.rs", "// Hand daemon edit\n");
  const hand = w.select("push", { before, after: w.commit() });
  assert.deepEqual(hand.jobs, only("hands", "windows", "rust", "rust_extra", "policy"));
  assert.equal(hand.raw.packages, "nanocodex-hand-daemon");
});
