import assert from "node:assert/strict";
import { readFile, stat } from "node:fs/promises";
import { pathToFileURL } from "node:url";

const root = new URL("../", import.meta.url);

export function checkDocumentedBrowserVersion(readme, packageVersion) {
  const documentedVersion = readme.match(
    /nanocodex@([^/"'\s]+)\/host\/index\.mjs/,
  )?.[1];
  assert.ok(documentedVersion, "README must pin the host CDN import");

  // pkg-pr-new rewrites package.json immediately before npm pack without
  // rewriting the source README. The README should remain pinned to the latest
  // release while that immutable, commit-addressed preview is packed.
  const isCommitPreview = /^0\.0\.0-preview-[0-9a-f]+$/.test(packageVersion);
  if (!isCommitPreview) {
    assert.equal(documentedVersion, packageVersion);
  }
}

const requiredFiles = [
  "cloud/index.mjs",
  "cloud/index.d.mts",
  "cloud/Client.mjs",
  "cloud/Client.d.mts",
  "cloud/Dialog.mjs",
  "cloud/Dialog.d.mts",
  "cloud/actions/index.mjs",
  "cloud/actions/index.d.mts",
  "browser/index.mjs",
  "browser/index.d.mts",
  "browser/InlineAgent.mjs",
  "browser/Voice.mjs",
  "browser/Voice.d.mts",
  "browser/VoiceSession.mjs",
  "browser/config.mjs",
  "browser/config.d.mts",
  "browser/engine.mjs",
  "browser/hostManagedWebSocket.mjs",
  "browser/hostManagedWebSocket.d.mts",
  "browser/WorkerAgent.mjs",
  "browser/WorkerAgent.d.mts",
  "browser/agent.worker.mjs",
  "browser/workspace.mjs",
  "browser/workspace.d.mts",
  "cloudflare/egress.mjs",
  "cloudflare/egress.d.mts",
  "cloudflare/egress-subject.mjs",
  "cloudflare/event-socket.mjs",
  "cloudflare/Agent.mjs",
  "cloudflare/Agent.d.mts",
  "cloudflare/index.mjs",
  "cloudflare/index.d.mts",
  "host/Agent.mjs",
  "host/Agent.d.mts",
  "host/index.mjs",
  "host/index.d.mts",
  "managed/Agent.mjs",
  "managed/Agent.d.mts",
  "managed/ManagedError.mjs",
  "managed/ManagedError.d.mts",
  "managed/index.mjs",
  "managed/index.d.mts",
  "next/index.mjs",
  "next/index.d.mts",
  "node/index.mjs",
  "node/index.d.mts",
  "node/workspace.mjs",
  "node/workspace.d.mts",
  "worker/index.mjs",
  "worker/index.d.mts",
  "runtime/workspace.mjs",
  "runtime/workspace.d.mts",
  "runtime/code-evaluator.worker.mjs",
  "runtime/worker-evaluator.mjs",
  "runtime/cloudflare-durability-store.mjs",
  "runtime/cloudflare-durability-store.d.mts",
  "runtime/postgres-durability-store.mjs",
  "runtime/postgres-durability-store.d.mts",
  "tools/index.mjs",
  "tools/index.d.mts",
  "tools/dataset.mjs",
  "tools/dataset.d.mts",
  "tools/datasetContract.mjs",
  "tools/datasetEngine.mjs",
  "tools/namedTool.mjs",
  "tools/artifact.mjs",
  "tools/artifact.d.mts",
  "tools/standardDescriptions.mjs",
  "tools/browser/index.mjs",
  "tools/browser/index.d.mts",
  "tools/vite.mjs",
  "tools/vite.d.mts",
  "webmcp/WebMcp.mjs",
  "webmcp/WebMcp.d.mts",
  "webmcp/generator.mjs",
  "webmcp/generator.d.mts",
  "webmcp/cli.mjs",
  "vite/index.mjs",
  "vite/index.d.mts",
  "vite/cloudflare.mjs",
  "vite/cloudflare.d.mts",
  "wasm.d.mts",
  "pkg-web/nanocodex.js",
  "pkg-web/nanocodex.d.ts",
  "pkg-web/nanocodex_bg.js",
  "pkg-web/nanocodex_bg.wasm",
  "pkg-web/nanocodex_worker.js",
  "pkg-web/nanocodex-build.json",
  "pkg-node/nanocodex.js",
  "pkg-node/nanocodex.d.ts",
];

export async function checkPackage(packageRoot = root) {
  const packageJson = JSON.parse(
    await readFile(new URL("package.json", packageRoot), "utf8"),
  );
  const readme = await readFile(new URL("README.md", packageRoot), "utf8");

  assert.equal(packageJson.name, "nanocodex");
  assert.equal(packageJson.type, "module");
  assert.equal(packageJson.engines?.node, ">=22.13.0");
  assert.equal(packageJson.publishConfig?.access, "public");
  assert.equal(packageJson.exports?.["./browser"]?.import, "./browser/index.mjs");
  assert.equal(packageJson.exports?.["./browser/client"], undefined);
  assert.equal(packageJson.exports?.["./browser/workspace"]?.import, "./browser/workspace.mjs");
  assert.equal(packageJson.exports?.["./browser/voice"]?.import, "./browser/Voice.mjs");
  assert.equal(packageJson.exports?.["./host"]?.import, "./host/index.mjs");
  assert.equal(packageJson.exports?.["./cloudflare"]?.import, "./cloudflare/index.mjs");
  assert.equal(packageJson.exports?.["./managed"]?.import, "./managed/index.mjs");
  assert.equal(packageJson.exports?.["./next"]?.import, "./next/index.mjs");
  assert.equal(packageJson.exports?.["./next"]?.require, "./next/index.mjs");
  assert.equal(packageJson.exports?.["./connect"]?.import, "./cloud/index.mjs");
  assert.equal(packageJson.exports?.["./connect"]?.types, "./cloud/index.d.mts");
  assert.equal(packageJson.exports?.["./connect/actions"]?.import, "./cloud/actions/index.mjs");
  assert.equal(packageJson.exports?.["./durability"]?.import, "./runtime/durability-store.mjs");
  assert.equal(
    packageJson.exports?.["./durability/cloudflare"]?.import,
    "./runtime/cloudflare-durability-store.mjs",
  );
  assert.equal(
    packageJson.exports?.["./durability/postgres"]?.import,
    "./runtime/postgres-durability-store.mjs",
  );
  assert.equal(
    packageJson.exports?.["./durability/postgres"]?.types,
    "./runtime/postgres-durability-store.d.mts",
  );
  assert.equal(packageJson.exports?.["./node"]?.import, "./node/index.mjs");
  assert.equal(packageJson.exports?.["./node/workspace"]?.import, "./node/workspace.mjs");
  assert.equal(packageJson.exports?.["./worker"]?.import, "./worker/index.mjs");
  assert.equal(packageJson.exports?.["./tools"]?.import, "./tools/index.mjs");
  assert.equal(packageJson.exports?.["./tools/dataset"]?.import, "./tools/dataset.mjs");
  assert.equal(packageJson.exports?.["./tools/artifact"]?.import, "./tools/artifact.mjs");
  assert.equal(packageJson.exports?.["./tools/browser"]?.import, "./tools/browser/index.mjs");
  assert.equal(packageJson.exports?.["./tools/vite"]?.import, "./tools/vite.mjs");
  assert.equal(packageJson.exports?.["./webmcp"]?.import, "./webmcp/WebMcp.mjs");
  assert.equal(packageJson.exports?.["./webmcp/generator"]?.import, "./webmcp/generator.mjs");
  assert.equal(packageJson.bin?.["nanocodex-webmcp"], "webmcp/cli.mjs");
  assert.equal(packageJson.exports?.["./vite"]?.import, "./vite/index.mjs");
  assert.equal(packageJson.exports?.["./vite/cloudflare"]?.import, "./vite/cloudflare.mjs");
  assert.equal(packageJson.exports?.["./wasm"]?.import, "./pkg-web/nanocodex_bg.wasm");
  checkDocumentedBrowserVersion(readme, packageJson.version);

  for (const file of requiredFiles) {
    const metadata = await stat(new URL(file, packageRoot));
    assert(metadata.isFile(), `${file} must be a file`);
    assert(metadata.size > 0, `${file} must not be empty`);
  }

  const wasm = await readFile(new URL("pkg-web/nanocodex_bg.wasm", packageRoot));
  assert(wasm.byteLength > 100_000, "shared WASM is unexpectedly small");
  assert.deepEqual([...wasm.subarray(0, 4)], [0x00, 0x61, 0x73, 0x6d]);

  const nodeGlue = await readFile(new URL("pkg-node/nanocodex.js", packageRoot), "utf8");
  assert.match(nodeGlue, /__dirname\}\/\.\.\/pkg-web\/nanocodex_bg\.wasm/);

  console.log(`nanocodex@${packageJson.version} package artifacts are complete`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await checkPackage();
}
