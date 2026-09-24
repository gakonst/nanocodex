import assert from "node:assert/strict";
import { test } from "node:test";

import { ArtifactStore, artifact, imageGeneration, updatePlan, viewImage, web } from "../tools/index.mjs";

const context = Object.freeze({
  callId: "call-1",
  parentCallId: "",
  sessionId: "session-1",
  model: "gpt-6-luna",
  signal: new AbortController().signal,
});

test("web rejects host redirects without forwarding credentials", async () => {
  const requests = [];
  const tool = web({
    url: "https://host.test/tools/web",
    headers: { authorization: "Bearer host" },
    async fetch(url, init) {
      requests.push({ url, init });
      return new Response(null, { status: 302, headers: { location: "https://other.test" } });
    },
  });

  await assert.rejects(
    tool.handler({ search_query: [{ q: "nanocodex" }] }, context),
    /redirects are not allowed/,
  );
  assert.equal(requests.length, 1);
  assert.equal(requests[0].init.redirect, "manual");
});

test("web follows Codex command decoding without repairing or splitting requests", async () => {
  const bodies = [];
  const tool = web({
    url: "https://host.test/web",
    fetch: async (_url, init) => {
      bodies.push(JSON.parse(init.body));
      return Response.json({ output: "" });
    },
  });
  await assert.rejects(() => tool.handler({ search_query: { q: "query" } }, context), /array/);
  await assert.rejects(() => tool.handler({ image_query: "rust wasm" }, context), /array/);
  await assert.rejects(() => tool.handler({ screenshot: [{ ref_id: "pdf", pageno: -1 }] }, context), /unsigned/);
  const commands = {
    screenshot: [{ ref_id: "pdf", pageno: 2 }],
    sports: [{ tool: "sports", fn: "schedule", league: "nba" }, { fn: "standings", league: "nfl" }],
    search_query: Array.from({ length: 5 }, (_, i) => ({ q: String(i) })),
  };
  assert.equal(await tool.handler(commands, context), "");
  await tool.handler({ unknown: true, open: null }, context);
  assert.deepEqual(bodies.map(({ commands }) => commands), [commands, {}]);
});

test("image generation resolves recent session images without owning conversation state", async () => {
  const remembered = [];
  const tool = imageGeneration({
    url: new URL("https://host.test/tools/images"),
    recentImages: (_sessionId, count) => ["data:image/png;base64,one"].slice(0, count),
    rememberImage: (sessionId, imageUrl) => remembered.push({ sessionId, imageUrl }),
    async fetch(_url, init) {
      assert.deepEqual(JSON.parse(init.body), {
        images: ["data:image/png;base64,one"],
        prompt: "edit it",
      });
      return Response.json({ image_url: "data:image/png;base64,two" });
    },
  });

  assert.deepEqual(await tool.handler({ prompt: "edit it", num_last_images_to_include: 1 }, context), {
    image_url: "data:image/png;base64,two",
  });
  assert.deepEqual(remembered, [{
    sessionId: "session-1",
    imageUrl: "data:image/png;base64,two",
  }]);
});

test("update_plan validates active work and releases session-owned state", async () => {
  const tool = updatePlan();
  const plan = {
    explanation: "ship the browser",
    plan: [
      { step: "wire", status: "completed" },
      { step: "verify", status: "in_progress" },
    ],
  };
  const result = await tool.handler(plan, context);
  assert.equal(result.output, "Plan updated");
  assert.deepEqual(result.structuredResult, {});
  assert.deepEqual(result.value, {});
  plan.plan[1].step = "mutated after publish";
  await assert.rejects(
    tool.handler({
      plan: [
        { step: "one", status: "in_progress" },
        { step: "two", status: "in_progress" },
      ],
    }, context),
    /at most one plan step/,
  );
  tool.releaseSession?.(context.sessionId);
  tool.dispose?.();
});

test("view_image rejects unsupported and oversized workspace files", async () => {
  for (const [path, contents] of [
    ["/workspace/readme.txt", "plain text"],
    ["/workspace/vector.svg", '<svg xmlns="http://www.w3.org/2000/svg"/>'],
  ]) {
    const unsupported = viewImage({
      workspace: { readFile: async () => new TextEncoder().encode(contents) },
    });
    await assert.rejects(
      unsupported.handler({ path }, context),
      /supports PNG, JPEG, GIF, and WebP/,
    );
  }

  const oversized = viewImage({
    workspace: { readFile: async () => new Uint8Array(10 * 1024 * 1024 + 1) },
  });
  await assert.rejects(
    oversized.handler({ path: "/workspace/huge.png" }, context),
    /exceeds 10 MiB/,
  );
});

test("artifact persistence does not impose binding-specific size or count limits", async () => {
  const workspace = memoryWorkspace();
  const store = new ArtifactStore(workspace);
  const largeSource = `function App() { return ${JSON.stringify("x".repeat(600 * 1024))}; }`;
  const document = await store.save({ title: "Large", source: largeSource });
  assert.equal((await store.read(document.id)).source, largeSource);
});

test("artifact source validation is host-owned and runs before persistence", async () => {
  const workspace = memoryWorkspace();
  const tool = artifact({
    workspace,
    validateSource(source) {
      if (source.includes("<main")) throw new SyntaxError("JSX is unavailable");
    },
  });
  await assert.rejects(
    tool.handler({ title: "Invalid", source: "function App() { return <main />; }" }, context),
    /JSX is unavailable/,
  );
  assert.deepEqual(await new ArtifactStore(workspace).list(), []);
});

test("image generation implements the canonical workspace-path edit mode", async () => {
  const tool = imageGeneration({
    url: "https://host.test/tools/images",
    workspace: {
      readFile: async () => Uint8Array.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    },
    fetch: async (_url, init) => {
      const body = JSON.parse(init.body);
      assert.equal(body.images[0], "data:image/png;base64,iVBORw0KGgo=");
      return Response.json({ image_url: "data:image/png;base64,result" });
    },
  });
  assert.deepEqual(tool.parameters.properties.referenced_image_paths.type, ["array", "null"]);
  await tool.handler({ prompt: "edit", referenced_image_paths: ["/workspace/input.png"] }, context);
  await assert.rejects(
    tool.handler({
      prompt: "edit",
      referenced_image_paths: ["/workspace/input.png"],
      num_last_images_to_include: 1,
    }, context),
    /not both/,
  );
});

function memoryWorkspace() {
  const files = new Map();
  const directories = new Set(["/workspace"]);
  return {
    root: "/workspace",
    async list() {
      return [
        ...[...directories].filter((path) => path !== "/workspace")
          .map((path) => ({ kind: "directory", path })),
        ...[...files].map(([path, contents]) => ({ kind: "file", path, size: contents.byteLength })),
      ];
    },
    async readFile(path) {
      const contents = files.get(path);
      if (!contents) throw Object.assign(new Error("not found"), { code: "ENOENT" });
      return contents;
    },
    async writeFile(path, contents) {
      files.set(path, typeof contents === "string"
        ? new TextEncoder().encode(contents)
        : contents instanceof ArrayBuffer
          ? new Uint8Array(contents)
          : new Uint8Array(contents.buffer, contents.byteOffset, contents.byteLength));
    },
    async remove(path) { files.delete(path); },
    async mkdir(path) { directories.add(path); },
  };
}

test("view_image loader receives per-call detail and retains its bounded output contract", async () => {
  const bytes = new Uint8Array([255, 216, 255, 224, 0]);
  const seen = [];
  const tool = viewImage({
    workspace: { readFile: async () => { throw new Error("unexpected workspace read"); } },
    loadImage: async (path, detail) => { seen.push([path, detail]); return { bytes }; },
  });
  const [high, original] = await Promise.all([
    tool.handler({ path: "/high.jpg" }, context),
    tool.handler({ path: "/original.jpg", detail: "original" }, context),
  ]);
  assert.deepEqual(seen, [["/high.jpg", "high"], ["/original.jpg", "original"]]);
  assert.equal(high.value.detail, "high");
  assert.equal(original.value.detail, "original");
  const oversized = viewImage({ workspace: {}, loadImage: async () => ({ bytes: new Uint8Array(10 * 1024 * 1024 + 1) }) });
  await assert.rejects(oversized.handler({ path: "/large.jpg" }, context), /exceeds 10 MiB/);
});
