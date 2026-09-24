import assert from "node:assert/strict";
import { mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { execFile } from "node:child_process";

const exec = promisify(execFile);
const packageRoot = new URL("../", import.meta.url);
const packageJson = JSON.parse(await readFile(new URL("package.json", packageRoot), "utf8"));

test("the package packs public exports and boots Node and browser agents", async () => {
  const temporary = await mkdtemp(join(tmpdir(), "nanocodex-package-"));
  try {
    const { stdout } = await exec("npm", [
      "pack",
      "--json",
      "--ignore-scripts",
      "--pack-destination",
      temporary,
      new URL(".", packageRoot).pathname,
    ]);
    const [packed] = JSON.parse(stdout);
    assert.equal(packed.name, packageJson.name);
    assert.equal(packed.version, packageJson.version);
    assert.equal(
      packed.files.some(({ path }) => path.startsWith("scripts/")),
      false,
      "development-only package checks must not ship",
    );
    const packedFiles = new Set(packed.files.map(({ path }) => path));
    for (const conditions of Object.values(packageJson.exports)) {
      for (const target of Object.values(conditions)) {
        assert.equal(
          packedFiles.has(target.replace(/^\.\//, "")),
          true,
          `packed package omitted exported file ${target}`,
        );
      }
    }

    const temporaryModules = join(temporary, "node_modules");
    await mkdir(temporaryModules);
    await symlink(
      fileURLToPath(packageRoot),
      join(temporaryModules, packageJson.name),
      process.platform === "win32" ? "junction" : "dir",
    );
    await writeFile(join(temporary, "package-smoke.mjs"), `
      import assert from "node:assert/strict";
      import { readFile } from "node:fs/promises";
      import { dirname, resolve } from "node:path";
      import { fileURLToPath } from "node:url";
      import { Agent as HostAgent, Transport as HostTransport } from "nanocodex/host";
      import { Agent as NodeAgent, Subagents as NodeSubagents, Transport as NodeTransport } from "nanocodex/node";
      import { Subagents as BrowserSubagents } from "nanocodex/browser";

      const nodeAgent = await NodeAgent.create({
        transport: NodeTransport.openAi({ apiKey: "package-test" }),
        tools: [...NodeSubagents.create({ maxConcurrency: 2 })],
      });
      assert.equal(nodeAgent.type, "node");
      await nodeAgent.session.shutdown();
      await nodeAgent.session.shutdown();

      const browserEntry = fileURLToPath(import.meta.resolve("nanocodex/browser"));
      const wasm = await readFile(resolve(
        dirname(browserEntry),
        "../pkg-web/nanocodex_bg.wasm",
      ));
      const browserAgent = await HostAgent.create({
        transport: HostTransport.openAi({
          apiKey: "package-test",
          WebSocketImpl: class {},
        }),
        module: wasm,
        tools: [...BrowserSubagents.create({ maxConcurrency: 2 })],
      });
      assert.equal(browserAgent.type, "browser");
      await browserAgent.session.shutdown();

      await assert.rejects(
        import("nanocodex/internal.mjs"),
        (error) => error.code === "ERR_PACKAGE_PATH_NOT_EXPORTED",
      );
      await assert.rejects(
        import("nanocodex/tools/datasetEngine"),
        (error) => error.code === "ERR_PACKAGE_PATH_NOT_EXPORTED",
      );
    `);
    await exec(process.execPath, [join(temporary, "package-smoke.mjs")], {
      cwd: temporary,
    });
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
});
