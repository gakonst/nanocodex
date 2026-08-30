import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

import { withWebMcp } from "../next/index.mjs";

test("the Next.js wrapper generates WebMCP before returning the application config", async () => {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-next-webmcp-"));
  try {
    const routeDirectory = join(directory, "app", "api", "checkout");
    await mkdir(routeDirectory, { recursive: true });
    await writeFile(
      join(routeDirectory, "route.ts"),
      "export async function POST() { return Response.json({ ok: true }); }\n",
    );
    const configure = withWebMcp({ reactStrictMode: true }, { root: directory });
    const config = await configure("phase-production-build", {});
    assert.deepEqual(config, { reactStrictMode: true });
    const manifest = JSON.parse(await readFile(join(directory, "webmcp.manifest.json"), "utf8"));
    assert.equal(manifest.tools.length, 1);
    assert.equal(manifest.tools[0].name, "post_api_checkout");
    assert.equal(manifest.tools[0].approved, false);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("the Next.js wrapper composes async application config functions", async () => {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-next-config-"));
  try {
    await writeFile(join(directory, "page.tsx"), "export default function Page() { return null; }\n");
    const configure = withWebMcp(async (phase) => ({ phase }), {
      root: directory,
      output: "generated/webmcp.json",
    });
    assert.deepEqual(await configure("phase-production-build", {}), {
      phase: "phase-production-build",
    });
    const manifest = JSON.parse(await readFile(join(directory, "generated", "webmcp.json"), "utf8"));
    assert.deepEqual(manifest.tools, []);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
