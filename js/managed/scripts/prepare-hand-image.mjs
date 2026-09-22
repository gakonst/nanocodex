import { cp, mkdir, rm, lstat } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";
import { dirname, join } from "node:path";

const source = fileURLToPath(new URL("../../../hands/remote/", import.meta.url));
const target = fileURLToPath(new URL("../.generated/hand/", import.meta.url));
await rm(target, { recursive: true, force: true });
await mkdir(target, { recursive: true });
await cp(`${source}/image/labwc`, `${target}/labwc`, { recursive: true });

const toolkit = fileURLToPath(new URL("../../../crates/nanocodex-vm/image/toolkit/", import.meta.url));
const toolkitTarget = fileURLToPath(new URL("../.generated/toolkit/", import.meta.url));
await rm(toolkitTarget, { recursive: true, force: true });
await cp(toolkit, toolkitTarget, { recursive: true });

// Only tracked source enters the Rust build context. Never copy worktree
// credentials, caches, target directories, or local build outputs.
const root = fileURLToPath(new URL("../../../", import.meta.url));
const rustTarget = fileURLToPath(new URL("../.generated/remote-rust/", import.meta.url));
const rustRoots = ["Cargo.toml", "Cargo.lock", "bin", "crates", "examples", "js/nanocodex", "py/bindings", "third_party"];
const paths = execFileSync("git", ["ls-files", "-z", "--", ...rustRoots], { cwd: root, encoding: "utf8", maxBuffer: 16 * 1024 * 1024 }).split("\0").filter(Boolean);
await rm(rustTarget, { recursive: true, force: true });
await mkdir(rustTarget, { recursive: true });
for (const path of paths) {
  if (path.split("/").some(part => part === ".." || part === ".env" || part === "target" || part === "node_modules")) throw new Error("unsafe Rust image source path");
  const source = join(root, path);
  const info = await lstat(source);
  if (!info.isFile()) throw new Error(`Rust image source must be a regular tracked file: ${path}`);
  const destination = join(rustTarget, path);
  await mkdir(dirname(destination), { recursive: true });
  await cp(source, destination);
}
