import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, symlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = fileURLToPath(new URL("../", import.meta.url));
const outputRoot = fileURLToPath(new URL("../../../output/connect-embed-pack/", import.meta.url));
mkdirSync(outputRoot, { recursive: true });
const output = mkdtempSync(join(outputRoot, "run-"));
function run(command, args, cwd = packageRoot) {
  const result = spawnSync(command, args, { cwd, encoding: "utf8", env: process.env });
  assert.equal(result.status, 0, `${command} ${args.join(" ")}\n${result.stdout}\n${result.stderr}`);
  return result.stdout;
}
// pnpm rewrites workspace ranges. Raw npm pack is intentionally not the release path.
const pack = JSON.parse(run("pnpm", ["--config.ignore-scripts=true", "pack", "--pack-destination", output, "--json"]));
const files = new Set(pack.files.map(file => file.path));
const manifest = JSON.parse(readFileSync(join(packageRoot, "package.json")));
for (const [name, target] of Object.entries(manifest.exports)) {
  for (const path of typeof target === "string" ? [target] : Object.values(target)) {
    assert(files.has(path.replace(/^\.\//, "")), `Missing published ${name}: ${path}`);
  }
}
assert(files.has("README.md"));
assert(![...files].some(path => path.startsWith("src/") || path.startsWith("test/") || path.startsWith("scripts/")));

// Install the actual tarball, offline. Preprovision only the unpublished sibling
// dependency; this isolates archive/exports validation from registry publication.
const consumer = join(output, "consumer");
const modules = join(consumer, "node_modules");
mkdirSync(modules, { recursive: true });
writeFileSync(join(consumer, "package.json"), JSON.stringify({ private: true, type: "module" }));
function peer(name) {
  const dest = join(modules, name);
  mkdirSync(dirname(dest), { recursive: true });
  symlinkSync(realpathSync(join(packageRoot, "node_modules", name)), dest, "dir");
}
for (const name of Object.keys(manifest.dependencies ?? {})) peer(name);
run("npm", ["install", "--offline", "--ignore-scripts", "--legacy-peer-deps", "--no-audit", "--no-fund", "--package-lock=false", pack.filename], consumer);
for (const name of [...Object.keys(manifest.peerDependencies), "react-dom"]) peer(name);
const installed = JSON.parse(readFileSync(join(modules, manifest.name, "package.json")));
for (const [name, range] of Object.entries({ ...installed.dependencies, ...installed.peerDependencies })) {
  assert(!/^(?:workspace|file|link):/.test(range), `Unpublishable production dependency ${name}: ${range}`);
}
assert.equal(installed.dependencies["nanocodex-terminal"], "^0.1.0");
const smoke = join(consumer, "smoke.mjs");
writeFileSync(smoke, `import assert from 'node:assert/strict';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { AgentEmbed, AgentConversation, ConnectConversation } from 'nanocodex-connect-embed';
import { AgentTerminalView } from 'nanocodex-terminal';
import 'nanocodex-connect-embed/primitives';
import 'nanocodex-connect-embed/connect';
import 'nanocodex-connect-embed/managed';
import 'nanocodex-connect-embed/headless';
import 'nanocodex-connect-embed/composer';
import 'nanocodex-connect-embed/transcript';
import 'nanocodex-connect-embed/generated-output';
assert.equal(AgentConversation, AgentTerminalView);
assert.equal(typeof ConnectConversation, 'function');
const html = renderToStaticMarkup(createElement(AgentEmbed, {agent: undefined}));
assert.match(html, /Not connected/);
assert.match(html, /disabled/);
assert.doesNotMatch(html, /<style|style=/);
console.log('Installed archive rendered disconnected embed and loaded every JavaScript entry point.');
`);
const smokeOutput = run(process.execPath, ["--import", join(packageRoot, "test/register-peer-loader.mjs"), smoke], consumer);
const evidence = { tarball: pack.filename, exports: Object.keys(installed.exports), dependencies: installed.dependencies, files: [...files], smoke: smokeOutput.trim(), install: "offline npm install of actual pnpm tarball; unpublished sibling preprovisioned from this checkout" };
writeFileSync(join(outputRoot, "evidence.json"), JSON.stringify(evidence, null, 2) + "\n");
console.log(`Verified ${Object.keys(manifest.exports).length} published entry points, rewritten dependency ranges and installed-archive SSR. Evidence: ${join(outputRoot, "evidence.json")}`);
