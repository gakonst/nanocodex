import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { changedPaths, selectJobs, selectionForEvent } from "./select-jobs.mjs";

const keys = ["native", "voice", "python", "rust", "wasm", "bindings", "apps", "preview", "policy", "codeql"];
const expected = (...selected) => Object.fromEntries(keys.map(key => [key, selected.includes(key)]));
const full = expected(...keys);
const policy = expected("policy");
const native = expected("native", "policy");

const cua489 = [
  "crates/experimental/nanocodex-computer/src/openai-cua-app-server.mjs",
  "docs/computer/official-app-server-bridge.md", "docs/computer/upstream-provider.md",
  "js/managed/src/namespace-tools.ts", "js/managed/test/namespace-tools.test.ts",
  "scripts/tests/openai-cua-app-server.test.mjs",
];

test("shared configuration, Rust sources, and unknown paths fail open in either order", () => {
  for (const path of [
    "crates/nanocodex/src/lib.rs", "bin/tool/main.rs", "Cargo.toml", "Cargo.lock",
    "py/bindings/Cargo.toml", ".cargo/config.toml", "rust-toolchain", "rust-toolchain.toml",
    "scripts/ci/select-jobs.mjs", ".github/workflows/ci.yml", "third_party/code/file",
    "package.json", "pnpm-lock.yaml", "pnpm-workspace.yaml", "turbo.json", ".npmrc",
    "js/account/package.json", "js/desktop-runtime/package.json", "js/account/package-lock.json",
    "js/nanocodex/src/lib.rs", "js/nanocodex/build.rs", "js/new-package/file.ts",
    "js/account/scripts/new-build.sh", "js/managed/Dockerfile", "apple/new.rs",
    "crates/experimental/nanocodex-computer/src/provision.rs",
    "crates/experimental/nanocodex-computer/src/openai-cua-new.mjs",
    "scripts/tests/openai-cua-new.test.mjs", "scripts/tests/../build-voice-native.py",
    "web/new-file.ts", "unknown.txt", "docs/../crates/a", "docs/Cargo.toml", "js//account/file.ts",
  ]) {
    assert.deepEqual(selectJobs([...cua489, path]), full, path);
    assert.deepEqual(selectJobs([path, ...cua489]), full, path);
  }
});

function repo(t) {
  const cwd = mkdtempSync(join(tmpdir(), "ci-selector-"));
  t.after(() => rmSync(cwd, { recursive: true, force: true }));
  const git = (...args) => execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
  git("init");
  git("config", "user.name", "CI selector test");
  git("config", "user.email", "ci@example.invalid");
  const write = (path, content = path) => {
    mkdirSync(dirname(join(cwd, path)), { recursive: true });
    writeFileSync(join(cwd, path), content);
  };
  const commit = () => { git("add", "-A"); git("commit", "-m", "fixture"); return git("rev-parse", "HEAD"); };
  write("README.md");
  const initial = commit();
  return { cwd, git, write, commit, initial };
}

test("push uses event endpoints, includes deletions and rename source, preserves NUL filenames", t => {
  const r = repo(t);
  r.write("crates/old.rs");
  r.write("windows/deleted.ps1");
  const before = r.commit();
  r.git("mv", "crates/old.rs", "docs-moved.md");
  r.git("rm", "windows/deleted.ps1");
  r.write("docs/with space\nand newline.md");
  const after = r.commit();
  r.write("unrelated-new-file"); r.commit();
  const paths = changedPaths("push", { before, after }, r.cwd);
  assert.deepEqual(paths.sort(), ["crates/old.rs", "docs-moved.md", "docs/with space\nand newline.md", "windows/deleted.ps1"].sort());
  assert.deepEqual(selectJobs(paths), full);
});

test("PR compares event head to merge base even when base and checkout have advanced", t => {
  const r = repo(t);
  r.git("checkout", "-b", "feature");
  r.write("docs/feature.md");
  const head = r.commit();
  r.git("checkout", "-b", "base", r.initial);
  r.write("crates/base-only.rs");
  const base = r.commit();
  const event = { pull_request: { base: { sha: base }, head: { sha: head } } };
  assert.deepEqual(changedPaths("pull_request", event, r.cwd), ["docs/feature.md"]);
  assert.deepEqual(selectionForEvent("pull_request", event, r.cwd).jobs, policy);
});

test("CUA deletions keep native checks and renames into unknown inputs run full CI", t => {
  const r = repo(t);
  const source = "crates/experimental/nanocodex-computer/src/openai-cua-app-server.mjs";
  r.write(source);
  const before = r.commit();
  r.git("rm", source);
  const deleted = r.commit();
  assert.deepEqual(selectionForEvent("push", { before, after: deleted }, r.cwd).jobs, native);
  r.git("checkout", "-b", "rename", before);
  r.git("mv", source, "crates/experimental/nanocodex-computer/src/new-bridge.mjs");
  const renamed = r.commit();
  assert.deepEqual(selectionForEvent("push", { before, after: renamed }, r.cwd).jobs, full);
});

test("unsupported events, absent endpoints, zero SHAs and unavailable history fail open", t => {
  const r = repo(t);
  for (const [name, event] of [
    ["schedule", {}], ["workflow_dispatch", {}], ["unknown", {}], ["push", {}],
    ["pull_request", {}], ["push", { before: "0".repeat(40), after: r.initial }],
    ["push", { before: "1".repeat(40), after: r.initial }],
    ["push", { before: "--bad", after: r.initial }],
    ["push", { before: r.initial, after: "0".repeat(40) }],
  ]) assert.deepEqual(selectionForEvent(name, event, r.cwd).jobs, full);
  r.git("checkout", "--orphan", "unrelated");
  r.git("rm", "-rf", "."); r.write("other");
  const unrelated = r.commit();
  assert.deepEqual(selectionForEvent("pull_request", { pull_request: { base: { sha: r.initial }, head: { sha: unrelated } } }, r.cwd).jobs, full);
});

test("CLI appends boolean output strings and readable summary; malformed payload runs full", t => {
  const r = repo(t);
  r.write("py/example.py"); const after = r.commit();
  const eventPath = join(r.cwd, "event.json");
  const outputPath = join(r.cwd, "output");
  const summaryPath = join(r.cwd, "summary");
  writeFileSync(outputPath, "existing=value\n");
  writeFileSync(eventPath, JSON.stringify({ before: r.initial, after }));
  const run = () => execFileSync(process.execPath, [fileURLToPath(new URL("./select-jobs.mjs", import.meta.url))], {
    cwd: r.cwd, encoding: "utf8", env: { ...process.env, GITHUB_REPOSITORY: "gakonst/nanocodex", GITHUB_EVENT_NAME: "push", GITHUB_EVENT_PATH: eventPath, GITHUB_OUTPUT: outputPath, GITHUB_STEP_SUMMARY: summaryPath },
  });
  assert.match(run(), /classified 1 changed path/);
  assert.equal(readFileSync(outputPath, "utf8"), "existing=value\n" + Object.entries(expected("python", "policy")).map(([key, value]) => `${key}=${value}\n`).join(""));
  assert.match(readFileSync(summaryPath, "utf8"), /python=true/);
  writeFileSync(eventPath, "invalid json");
  assert.match(run(), /full CI/);
  assert.ok(readFileSync(outputPath, "utf8").endsWith(Object.entries(full).map(([key, value]) => `${key}=${value}\n`).join("")));
  rmSync(eventPath);
  assert.match(run(), /full CI/);
});

test("fork CI does not require the upstream-only package publisher", t => {
  const r = repo(t);
  const eventPath = join(r.cwd, "event.json");
  const outputPath = join(r.cwd, "output");
  writeFileSync(eventPath, "{}");
  execFileSync(process.execPath, [fileURLToPath(new URL("./select-jobs.mjs", import.meta.url))], {
    cwd: r.cwd, env: { ...process.env, GITHUB_REPOSITORY: "example/fork", GITHUB_EVENT_NAME: "workflow_dispatch",
      GITHUB_EVENT_PATH: eventPath, GITHUB_OUTPUT: outputPath, GITHUB_STEP_SUMMARY: "" },
  });
  const actual = Object.fromEntries(readFileSync(outputPath, "utf8").trim().split("\n").map(line => {
    const [key, value] = line.split("="); return [key, value === "true"];
  }));
  assert.deepEqual(actual, { ...full, preview: false });
});
