#!/usr/bin/env node
// fmt + Clippy for the Rust packages a change affects and their dependents;
// CI's Clippy job runs the same command.
//   pnpm check:fast [-- --base <ref>]   # changes since merge-base with origin/master
//   node scripts/check-fast.mjs --packages "a b" | "*"   # explicit selection (CI)
import { execFileSync, spawnSync } from "node:child_process";
import { loadGraph, selectJobs } from "./ci/select-jobs.mjs";

const args = process.argv.slice(2);
const option = name => { const i = args.indexOf(name); return i >= 0 ? args[i + 1] : undefined; };
const git = (...a) => execFileSync("git", a, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();

function localPackages() {
  let base = option("--base");
  if (!base) for (const ref of ["origin/master", "master"]) {
    try { base = git("merge-base", "HEAD", ref); break; } catch { /* try next */ }
  }
  if (!base) return "*";
  const paths = [...new Set([
    ...git("diff", "--name-only", "--no-renames", base).split("\n"),
    // Only untracked Rust inputs; other untracked files would select everything.
    ...git("ls-files", "--others", "--exclude-standard").split("\n").filter(path => /(?:\.rs|Cargo\.toml)$/.test(path)),
  ].filter(Boolean))];
  const { packages } = selectJobs(paths, loadGraph());
  return packages;
}

const packages = option("--packages") ?? localPackages();
const run = (cmd, argv) => {
  console.log(`$ ${cmd} ${argv.join(" ")}`);
  const { status } = spawnSync(cmd, argv, { stdio: "inherit" });
  if (status !== 0) process.exit(status ?? 1);
};
if (!packages) {
  console.log("check:fast: no Rust package affected");
  process.exit(0);
}
const lint = ["--", "-D", "warnings", "-A", "clippy::missing_const_for_fn"];
const selected = packages === "*" ? null : packages.split(/\s+/).filter(Boolean);
run("cargo", ["fmt", "--all", "--", "--check"]);
// Features that only compile on Linux (seccomp, landlock, netlink). CI lints on
// Linux with every feature; other hosts lint these packages without them.
const linuxOnlyFeatures = process.platform === "linux" ? {} : { "nanocodex-vm": ["guest-runtime"] };
const restricted = Object.keys(linuxOnlyFeatures).filter(p => !selected || selected.includes(p));
const library = selected
  ? selected.filter(p => p !== "nanocodex-bin" && !restricted.includes(p)).flatMap(p => ["-p", p])
  : ["--workspace", "--exclude", "nanocodex-bin", ...restricted.flatMap(p => ["--exclude", p])];
if (library.length) run("cargo", ["clippy", "--locked", ...library, "--all-targets", "--all-features", ...lint]);
if (restricted.length) {
  const metadata = JSON.parse(execFileSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], { encoding: "utf8" }));
  for (const name of restricted) {
    const features = Object.keys(metadata.packages.find(p => p.name === name).features)
      .filter(feature => !linuxOnlyFeatures[name].includes(feature));
    run("cargo", ["clippy", "--locked", "-p", name, "--all-targets", "--features", features.join(","), ...lint]);
  }
}
// The CLI crate is linted for its binary and benchmark only; it reuses the
// dependency artifacts built above.
if (!selected || selected.includes("nanocodex-bin")) {
  run("cargo", ["clippy", "--locked", "-p", "nanocodex-bin", "--all-features", "--bin", "nanocodex", "--bench", "nanocodex2_tui", ...lint]);
}
