import assert from "node:assert/strict";
import { readFile, readdir, stat } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const clientDirectory = fileURLToPath(new URL("../dist/client/", import.meta.url));
const manifest = JSON.parse(
  await readFile(new URL(".vite/manifest.json", `file://${clientDirectory}/`), "utf8"),
);
const entry = manifest["index.html"];
const mpp = manifest["src/MppControls.tsx"];

assert(entry?.isEntry, "the browser entry is missing from the Vite manifest");
assert(mpp?.isDynamicEntry, "the MPP controls must remain a dynamic entry");
assert(
  entry.dynamicImports?.includes("src/MppControls.tsx"),
  "the default app must load MPP controls through import()",
);

const staticEntries = staticClosure("index.html");
assert(
  !staticEntries.has("src/MppControls.tsx"),
  "the default OpenAI graph must not statically import the MPP controls",
);

const entryPath = new URL(entry.file, `file://${clientDirectory}/`);
const entrySource = await readFile(entryPath, "utf8");
const entryBytes = (await stat(entryPath)).size;
assert(
  entryBytes <= 220 * 1024,
  `default OpenAI entry is ${entryBytes} bytes; expected at most 220 KiB`,
);
assert(
  !entrySource.includes("Tempo Wallet connector is unavailable"),
  "the default OpenAI entry initialized the Tempo wallet integration",
);

const html = await readFile(
  new URL("index.html", `file://${clientDirectory}/`),
  "utf8",
);
assert(
  !html.includes(mpp.file),
  "index.html must not preload the opt-in MPP entry",
);

const mppSource = await readFile(
  new URL(mpp.file, `file://${clientDirectory}/`),
  "utf8",
);
assert(
  mppSource.includes("Tempo Wallet connector is unavailable"),
  "the explicit MPP entry no longer contains the wallet integration",
);

const assetsDirectory = new URL("assets/", `file://${clientDirectory}/`);
const assets = await readdir(assetsDirectory);
const workerFiles = assets.filter((file) => /^worker-.*\.js$/.test(file));
assert.equal(workerFiles.length, 1, "expected one browser Agent Worker entry");
const workerSource = await readFile(new URL(workerFiles[0], assetsDirectory), "utf8");
const tempoImport = workerSource.match(/import\(`\.\/(tempo-[^`]+\.js)`\)/);
assert(tempoImport, "the Agent Worker must retain an explicit lazy MPP path");
assert(
  assets.includes(tempoImport[1]),
  `the lazy Worker MPP chunk ${tempoImport[1]} is missing`,
);
const mcpImport = workerSource.match(/import\(`\.\/(mcp-runtime-[^`]+\.js)`\)/);
assert(mcpImport, "the Agent Worker must retain an explicit lazy MCP path");
assert(
  assets.includes(mcpImport[1]),
  `the lazy Worker MCP chunk ${mcpImport[1]} is missing`,
);

console.log(JSON.stringify({
  defaultEntryBytes: entryBytes,
  defaultStaticChunks: [...staticEntries],
  mppEntry: mpp.file,
  workerMcpEntry: mcpImport[1],
  workerMppEntry: tempoImport[1],
}));

function staticClosure(root) {
  const seen = new Set();
  const visit = (key) => {
    if (seen.has(key)) return;
    seen.add(key);
    for (const imported of manifest[key]?.imports ?? []) visit(imported);
  };
  visit(root);
  return seen;
}
