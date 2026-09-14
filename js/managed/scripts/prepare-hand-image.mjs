import { cp, mkdir, readdir, rm } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const source = fileURLToPath(new URL("../../../hands/remote/", import.meta.url));
const target = fileURLToPath(new URL("../.generated/hand/", import.meta.url));
await rm(target, { recursive: true, force: true });
await mkdir(target, { recursive: true });
for (const name of await readdir(source)) {
  if (name === "go.mod" || name === "go.sum" || (name.endsWith(".go") && !name.endsWith("_test.go"))) {
    await cp(`${source}/${name}`, `${target}/${name}`);
  }
}
await cp(`${source}/image/labwc`, `${target}/labwc`, { recursive: true });

const toolkit = fileURLToPath(new URL("../../../crates/experimental/nanocodex-vm/image/toolkit/", import.meta.url));
const toolkitTarget = fileURLToPath(new URL("../.generated/toolkit/", import.meta.url));
await rm(toolkitTarget, { recursive: true, force: true });
await cp(toolkit, toolkitTarget, { recursive: true });
