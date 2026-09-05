import assert from "node:assert/strict";
import { test } from "node:test";

import { compileInput } from "../tools/browser/compiler.worker.mjs";

test("browser compiler releases every successful run tree while preserving output", async () => {
  const api = new FakeCompilerApi();
  const largeSource = new Uint8Array(2 * 1024 * 1024).fill(7);
  const input = {
    compileOnly: true,
    files: [{ path: "src/main.cpp", contents: largeSource }],
    optimize: "2",
    sources: ["/workspace/src/main.cpp"],
  };

  for (let iteration = 0; iteration < 24; iteration += 1) {
    const output = await compileInput(api, iteration % 2, input);
    assert.deepEqual(output, new Uint8Array([0, 1, 2, 3]));
    assert.equal(api.memfs.runEntries().length, 0);
    assert.equal(api.memfs.retainedRunBytes(), 0);
  }
});

test("browser compiler releases its run tree after compilation fails", async () => {
  const api = new FakeCompilerApi({ failCompilation: true });
  const input = {
    compileOnly: true,
    files: [{ path: "main.cpp", contents: new Uint8Array(1024 * 1024) }],
    optimize: "2",
    sources: ["/workspace/main.cpp"],
  };

  await assert.rejects(compileInput(api, 7, input), /fixture compile failed/);
  assert.equal(api.memfs.runEntries().length, 0);
  assert.equal(api.memfs.retainedRunBytes(), 0);

  api.failCompilation = false;
  assert.deepEqual(await compileInput(api, 7, input), new Uint8Array([0, 1, 2, 3]));
});

class FakeCompilerApi {
  constructor({ failCompilation = false } = {}) {
    this.failCompilation = failCompilation;
    this.memfs = new FakeMemfs();
  }

  async compile({ input, contents, obj }) {
    this.memfs.addFile(input, contents);
    this.memfs.addFile(obj, new Uint8Array([0, 1, 2, 3]));
    if (this.failCompilation) throw new Error("fixture compile failed");
  }
}

class FakeMemfs {
  #entries = new Map();
  #path = "";

  constructor() {
    this.hostMem_ = { name: "compiler" };
    this.mem = {
      check() {},
      write: (_pointer, path) => { this.#path = path; },
    };
    this.exports = {
      GetPathBuf: () => 1,
      path_unlink_file: () => {
        const removed = [...this.#entries].filter(([path]) =>
          path === this.#path || path.startsWith(`${this.#path}/`));
        for (const [path] of removed) this.#entries.delete(path);
        return removed.length ? 0 : 44;
      },
    };
  }

  set hostMem(memory) { this.hostMem_ = memory; }

  addDirectory(path) {
    this.#requireParent(path);
    this.#entries.set(path, { directory: true, bytes: 0 });
  }

  addFile(path, contents) {
    this.#requireParent(path);
    this.#entries.set(path, { directory: false, bytes: contents.byteLength });
  }

  getFileContents(path) {
    if (!this.#entries.has(path)) throw new Error(`missing fixture file: ${path}`);
    return new Uint8Array([0, 1, 2, 3]);
  }

  runEntries() {
    return [...this.#entries.keys()].filter((path) => path.startsWith(".nanocodex-runs/run-"));
  }

  retainedRunBytes() {
    return [...this.#entries]
      .filter(([path]) => path.startsWith(".nanocodex-runs/run-"))
      .reduce((total, [, entry]) => total + entry.bytes, 0);
  }

  #requireParent(path) {
    const separator = path.lastIndexOf("/");
    if (separator < 0) return;
    const parent = path.slice(0, separator);
    if (!this.#entries.get(parent)?.directory) {
      throw new Error(`missing fixture directory: ${parent}`);
    }
  }
}
