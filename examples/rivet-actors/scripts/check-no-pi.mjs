import { readdir, readFile } from "node:fs/promises";

const root = new URL("../", import.meta.url);
const manifest = JSON.parse(await readFile(new URL("package.json", root), "utf8"));
const forbidden = [
  "@agentos-software/pi",
  "@mariozechner/pi-agent-core",
  "@mariozechner/pi-ai",
  "@mariozechner/pi-coding-agent",
  "@mariozechner/pi-tui",
  "pi-acp",
];
const declared = Object.keys({
  ...manifest.dependencies,
  ...manifest.devDependencies,
  ...manifest.optionalDependencies,
}).filter((name) => forbidden.includes(name));

if (declared.length > 0) {
  throw new Error(`forbidden direct Pi dependencies: ${declared.join(", ")}`);
}

const sourceFiles = await collectSourceFiles(["src", "scripts", "test", "web"]);
const imported = [];
for (const file of sourceFiles) {
  if (file.pathname.endsWith("/check-no-pi.mjs")) continue;
  const source = await readFile(file, "utf8");
  for (const name of forbidden) {
    if (source.includes(`from "${name}"`)
      || source.includes(`from '${name}'`)
      || source.includes(`import("${name}")`)
      || source.includes(`import('${name}')`)
      || source.includes(`require("${name}")`)
      || source.includes(`require('${name}')`)) {
      imported.push(`${file.pathname}:${name}`);
    }
  }
}

if (imported.length > 0) {
  throw new Error(`forbidden Pi imports: ${imported.join(", ")}`);
}

console.log("dependency check: no direct Pi dependencies or imports");

async function collectSourceFiles(directories) {
  const files = [];
  for (const directory of directories) {
    const url = new URL(`${directory}/`, root);
    for (const entry of await readdir(url, { withFileTypes: true })) {
      const child = new URL(entry.name + (entry.isDirectory() ? "/" : ""), url);
      if (entry.isDirectory()) files.push(...await collectDirectory(child));
      else if (/\.(?:[cm]?[jt]sx?)$/.test(entry.name)) files.push(child);
    }
  }
  return files;
}

async function collectDirectory(directory) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const child = new URL(entry.name + (entry.isDirectory() ? "/" : ""), directory);
    if (entry.isDirectory()) files.push(...await collectDirectory(child));
    else if (/\.(?:[cm]?[jt]sx?)$/.test(entry.name)) files.push(child);
  }
  return files;
}
