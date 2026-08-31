import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { createCodeRuntime } from "../runtime/code-runtime.mjs";

import {
  dataset,
  ArtifactStore,
  artifact,
  imageGeneration,
  updatePlan,
  viewImage,
  web,
} from "../tools/index.mjs";

const context = Object.freeze({
  callId: "call-1",
  parentCallId: "",
  sessionId: "session-1",
  model: "gpt-5.6-terra",
  signal: new AbortController().signal,
});

test("standard tool descriptions stay identical to the Rust-owned Codex contracts", async () => {
  const [webDescription, imageDescription] = await Promise.all([
    readFile(new URL("../../../crates/nanocodex-tools/src/web_search/web_run_description.md", import.meta.url), "utf8"),
    readFile(new URL("../../../crates/nanocodex-tools/src/image_generation/imagegen_description.md", import.meta.url), "utf8"),
  ]);
  assert.equal(web().description, webDescription.trimEnd());
  assert.equal(imageGeneration().description, imageDescription.trimEnd());
});

test("web forwards the complete command object through a caller-owned host adapter", async () => {
  const requests = [];
  const tool = web({
    url: "https://host.test/tools/web",
    headers: { authorization: "Bearer host" },
    async fetch(url, init) {
      requests.push({ url, init });
      return Response.json({ output: "searched" });
    },
  });

  assert.equal(tool.name, "web__run");
  assert.deepEqual(await tool.handler({ search_query: [{ q: "nanocodex" }] }, context), "searched");
  assert.deepEqual(JSON.parse(requests[0].init.body), {
    commands: { search_query: [{ q: "nanocodex" }] },
    session_id: "session-1",
    model: "gpt-5.6-terra",
  });
  assert.equal(requests[0].init.headers.authorization, "Bearer host");
  assert.equal(requests[0].init.redirect, "manual");
  assert.equal(requests[0].init.signal, context.signal);
  assert.deepEqual(tool.parameters.properties.search_query.items.required, ["q"]);
  assert.deepEqual(tool.parameters.properties.response_length.enum, ["short", "medium", "long"]);
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

test("web repairs common non-array model argument shapes before host dispatch", async () => {
  const bodies = [];
  const tool = web({
    url: "https://host.test/web",
    fetch: async (_url, init) => {
      bodies.push(JSON.parse(init.body));
      return Response.json({ output: "ok" });
    },
  });
  await tool.handler({ commands: { search_query: { q: "nanocodex" } } }, context);
  await tool.handler({ image_query: "rust wasm", open: "turn1search0" }, context);
  assert.deepEqual(bodies.map(({ commands }) => commands), [
    { search_query: [{ q: "nanocodex" }] },
    { image_query: [{ q: "rust wasm" }], open: [{ ref_id: "turn1search0" }] },
  ]);
});

test("web and image generation default to the standard same-origin host routes", async () => {
  const urls = [];
  const fetch = async (url) => {
    urls.push(url);
    return Response.json(url.includes("web-search")
      ? { output: "ok" }
      : { image_url: "data:image/png;base64,image" });
  };
  await web({ fetch }).handler({ search_query: [{ q: "nanocodex" }] }, context);
  await imageGeneration({ fetch }).handler({ prompt: "draw it" }, context);
  assert.deepEqual(urls, ["/api/tools/web-search", "/api/tools/image-generation"]);
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

test("each factory returns an immutable named tool for direct array composition", () => {
  assert(Object.isFrozen(updatePlan()));
  assert.equal(dataset().name, "dataset");
  const image = viewImage({ workspace: { readFile: async () => new Uint8Array() } });
  assert.equal(image.name, "view_image");
  assert.equal(image.supportsParallelToolCalls, true);
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
  assert.deepEqual(await tool.handler(plan, context), { updated: true });
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

test("the code runtime forwards session and host lifecycle to stateful tools", async () => {
  const released = [];
  let disposals = 0;
  const runtime = createCodeRuntime({
    stateful: {
      description: "stateful test tool",
      handler: () => null,
      releaseSession: (sessionId) => released.push(sessionId),
      dispose: () => disposals++,
    },
  });
  runtime.releaseSession("session-1");
  await runtime.reset();
  assert.deepEqual(released, ["session-1"]);
  assert.equal(disposals, 1);
});

test("artifact is a named typed tool, not a shell command", async () => {
  const workspace = memoryWorkspace();
  const rendered = [];
  const tool = artifact({ workspace, onArtifact: (document) => rendered.push(document) });

  assert.equal(tool.name, "render_artifact");
  assert(Object.isFrozen(tool));
  assert.deepEqual(tool.parameters.required, ["title", "source"]);
  assert.deepEqual(tool.outputSchema.required, ["artifactId", "path", "title", "runtime"]);
  assert.deepEqual(await tool.handler({
    id: "answer",
    title: "Answer",
    source: "function App() { return html`<main>42</main>`; }",
  }, context), {
    artifactId: "answer",
    path: "/workspace/.nanocodex/artifacts/answer.json",
    title: "Answer",
    runtime: "react",
  });
  assert.equal(rendered.length, 1);
  assert.equal((await new ArtifactStore(workspace).read("answer")).title, "Answer");
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
