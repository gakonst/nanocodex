import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import {
  readFile,
  writeFile,
  mkdir,
  mkdtemp,
  readdir,
  copyFile,
} from "node:fs/promises";
import { createRequire } from "node:module";
import { dirname, join, relative, resolve } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";

const here = dirname(fileURLToPath(import.meta.url));
const managed = resolve(here, "../..");
const root = resolve(managed, "../..");
const require = createRequire(import.meta.url);
// The explicit test dependency supports the production bundle compatibility date.
const installedWorkerd = require("workerd");
const workerd = process.env.WORKERD || installedWorkerd.default;
const command = "corepack pnpm --filter nanocodex-managed-service test:obscura";
const outputRoot = join(root, "output/obscura");
await mkdir(outputRoot, { recursive: true });
const output = await mkdtemp(join(outputRoot, "run-"));
console.log(`Obscura evidence: ${relative(root, output)}`);
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
const manifest = {
  command,
  workerd,
  installedWorkerdVersion: installedWorkerd.version,
  inputs: {},
  expected: { checks: 62, allPass: true },
};
let child,
  exited,
  log = "",
  controlLog = "",
  result;

async function prepareSdk() {
  const pin = JSON.parse(
    await readFile(join(here, "sdk-sources.json"), "utf8"),
  );
  const packageRoot = dirname(dirname(require.resolve("agents")));
  const pkg = JSON.parse(
    await readFile(join(packageRoot, "package.json"), "utf8"),
  );
  assert.equal(
    pkg.version,
    pin.version,
    "Review and repin the SDK journey when upgrading agents",
  );
  const sdk = join(output, "sdk");
  await mkdir(sdk);
  const provenance = { package: pkg.name, version: pkg.version, files: [] };
  const maps = (await readdir(join(packageRoot, "dist"))).filter((name) =>
    name.endsWith(".js.map"),
  );
  // agents currently ships the private helpers only as sourcesContent in its
  // published source maps. Prefer actual source files if a later package has them.
  for (const entry of pin.files) {
    let source,
      from = entry.source.replace(/^\.\.\//, "");
    try {
      source = await readFile(join(packageRoot, from), "utf8");
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    if (source === undefined) {
      for (const name of maps) {
        const map = JSON.parse(
          await readFile(join(packageRoot, "dist", name), "utf8"),
        );
        const index = map.sources.indexOf(entry.source);
        if (index !== -1 && map.sourcesContent?.[index] != null) {
          source = map.sourcesContent[index];
          from = `dist/${name}#${entry.source}`;
          break;
        }
      }
    }
    assert.equal(
      typeof source,
      "string",
      `Installed agents is missing ${entry.source}`,
    );
    assert.equal(
      hash(source),
      entry.sha256,
      `SDK source changed: ${entry.source}; review before repinning`,
    );
    await writeFile(join(sdk, entry.file), source);
    provenance.files.push({ file: entry.file, from, sha256: hash(source) });
  }
  await copyFile(join(packageRoot, "LICENSE"), join(sdk, "LICENSE"));
  await writeFile(
    join(sdk, "provenance.json"),
    JSON.stringify(provenance, null, 2) + "\n",
  );
  manifest.sdk = provenance;
  return sdk;
}

function waitForListener() {
  return new Promise((resolvePort, reject) => {
    const timer = setTimeout(
      () => finish(new Error("workerd startup timed out")),
      20_000,
    );
    const finish = (error, port) => {
      clearTimeout(timer);
      child.off("error", fail);
      child.off("exit", onExit);
      lines.close();
      error ? reject(error) : resolvePort(port);
    };
    const fail = (error) => finish(error);
    const onExit = (code, signal) =>
      finish(
        new Error(`workerd exited during startup: ${code ?? signal}\n${log}`),
      );
    const lines = createInterface({ input: child.stdio[3] });
    lines.on("line", (line) => {
      controlLog += line + "\n";
      try {
        const event = JSON.parse(line);
        if (event.event === "listen" && event.socket === "http")
          finish(null, event.port);
      } catch (error) {
        finish(error);
      }
    });
    child.once("error", fail);
    child.once("exit", onExit);
  });
}

try {
  const sdk = await prepareSdk();
  const assets = ["worker.txt", "bootstrap.txt", "quickjs.bin", "dom.bin"];
  const modules = [];
  for (const name of assets) {
    const bytes = await readFile(join(managed, "src/obscura-assets", name));
    manifest.inputs[`src/obscura-assets/${name}`] = {
      bytes: bytes.length,
      sha256: hash(bytes),
    };
    // Snapshot the current shipped assets so each retained config reproduces
    // its run even after prepare:obscura updates the source tree.
    await mkdir(join(output, "obscura-assets"), { recursive: true });
    await writeFile(join(output, "obscura-assets", name), bytes);
    modules.push(
      `(name="obscura-assets/${name}", ${name.endsWith(".bin") ? "data" : "text"}=embed "obscura-assets/${name}")`,
    );
  }
  await build({
    entryPoints: [join(here, "worker.mjs")],
    outfile: join(output, "worker.js"),
    bundle: true,
    platform: "browser",
    format: "esm",
    conditions: ["workerd"],
    plugins: [
      {
        name: "obscura-proof-inputs",
        setup(build) {
          build.onResolve({ filter: /^#obscura-sdk\// }, (args) => ({
            path: join(sdk, args.path.slice("#obscura-sdk/".length) + ".ts"),
          }));
          build.onResolve(
            { filter: /obscura-assets\/[^/]+\.(txt|bin)$/ },
            (args) => ({
              path: "obscura-assets/" + args.path.split("/").at(-1),
              external: true,
            }),
          );
        },
      },
    ],
  });
  await build({
    entryPoints: [join(here, "fixture-worker.mjs")],
    outfile: join(output, "fixture-worker.js"),
    bundle: true,
    platform: "browser",
    format: "esm",
  });
  const config = `using Workerd = import "/workerd/workerd.capnp";
const config :Workerd.Config = (
  services = [
    (name="network", worker=(compatibilityDate="2026-07-30", globalOutbound="network", modules=[(name="worker.js", esModule=embed "fixture-worker.js")])),
    (name="main", worker=(compatibilityDate="2026-07-30", globalOutbound="network",
      bindings=[(name="LOADER", workerLoader=()), (name="NETWORK", service="network")],
      modules=[(name="worker.js", esModule=embed "worker.js"), ${modules.join(",\n        ")}]))
  ],
  sockets=[(name="http", address="127.0.0.1:0", http=(), service="main")]
);
`;
  await writeFile(join(output, "config.capnp"), config);
  for (const file of ["worker.js", "fixture-worker.js", "config.capnp"])
    manifest.inputs[file] = {
      sha256: hash(await readFile(join(output, file))),
    };
  child = spawn(
    workerd,
    ["serve", "--experimental", "--control-fd=3", join(output, "config.capnp")],
    { cwd: output, stdio: ["ignore", "pipe", "pipe", "pipe"] },
  );
  child.stdout.on("data", (chunk) => {
    log += chunk;
  });
  child.stderr.on("data", (chunk) => {
    log += chunk;
  });
  exited = new Promise((resolveExit) => {
    child.once("exit", resolveExit);
    child.once("error", resolveExit);
  });
  const port = await waitForListener();
  const response = await fetch(`http://127.0.0.1:${port}/`, {
    signal: AbortSignal.timeout(60_000),
  });
  result = await response.json();
  await writeFile(
    join(output, "result.json"),
    JSON.stringify(result, null, 2) + "\n",
  );
  assert.equal(response.status, 200, result.error || "Journey HTTP status");
  assert.equal(
    result.ok,
    true,
    JSON.stringify(result.checks.filter((check) => !check.pass)),
  );
  assert.equal(
    result.checks.length,
    62,
    "Expected the complete SDK/CDP/storage journey",
  );
  assert.ok(
    result.checks.every((check) => check.pass),
    "Every observable check must pass",
  );
  manifest.observed = {
    status: response.status,
    checks: result.checks.length,
    allPass: true,
  };
  console.log(
    `PASS: ${result.checks.length} checks through agents SDK -> Obscura binding -> WorkerLoader -> packaged Wasm -> fixture network`,
  );
} catch (error) {
  manifest.error = String(error.stack || error);
  console.error(manifest.error);
  process.exitCode = 1;
} finally {
  if (child && child.exitCode === null && child.signalCode === null) {
    child.kill("SIGTERM");
    const timer = setTimeout(() => child.kill("SIGKILL"), 2_000);
    await exited;
    clearTimeout(timer);
  }
  await writeFile(join(output, "workerd.log"), log);
  await writeFile(join(output, "control.jsonl"), controlLog);
  await writeFile(
    join(output, "manifest.json"),
    JSON.stringify(manifest, null, 2) + "\n",
  );
  console.log(`Evidence retained in ${relative(root, output)}`);
}
