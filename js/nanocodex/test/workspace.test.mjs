import assert from "node:assert/strict";
import { lstat, mkdtemp, readFile, rm, symlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { createBrowserHost } from "../browser/host.mjs";
import * as BrowserWorkspace from "../browser/workspace.mjs";
import * as NodeWorkspace from "../node/workspace.mjs";

test("browser workspaces persist files across independent opens", async () => {
  const storage = memoryOpfs();
  const first = await BrowserWorkspace.open({ name: "notebook", storage });
  await first.writeFile("notes/answer.txt", "forty two");

  const second = await BrowserWorkspace.open({ name: "notebook", storage });
  assert.equal(new TextDecoder().decode(await second.readFile("/workspace/notes/answer.txt")), "forty two");
  assert.deepEqual(await second.list(".", { recursive: true }), [
    { kind: "directory", path: "/workspace/notes" },
    { kind: "file", modifiedAt: 1, path: "/workspace/notes/answer.txt", size: 9 },
  ]);

  const isolated = await BrowserWorkspace.open({ name: "another-notebook", storage });
  assert.deepEqual(await isolated.list(), []);
});

test("workspace tools expose bounded text operations", async () => {
  const workspace = await BrowserWorkspace.open({ storage: memoryOpfs() });
  const tools = BrowserWorkspace.tools(workspace, { maxReadBytes: 4, maxWriteBytes: 16 });

  assert.deepEqual(await tools.write_file.handler({ path: "a.txt", content: "hello" }), {
    path: "/workspace/a.txt",
    bytesWritten: 5,
  });
  assert.deepEqual(await tools.read_file.handler({ path: "a.txt", limit: 4 }), {
    path: "/workspace/a.txt",
    content: "hell",
    size: 5,
    offset: 0,
    truncated: true,
  });
  await assert.rejects(
    tools.read_file.handler({ path: "a.txt", limit: 5 }),
    /read bound/,
  );
});

test("workspace tools cross the browser host into model-visible Code Mode definitions", async () => {
  const workspace = await BrowserWorkspace.open({ storage: memoryOpfs() });
  const host = createBrowserHost({
    createWebSocket() {},
    filesystem: workspace,
  });
  await host.ready();
  const names = JSON.parse(host.toolDefinitions()).map((definition) => definition.name);
  assert.deepEqual(names, [
    "list_files",
    "read_file",
    "write_file",
    "make_directory",
    "delete_file",
  ]);
});

test("a shell-owned browser workspace omits legacy filesystem functions", async () => {
  const workspace = await BrowserWorkspace.open({ storage: memoryOpfs() });
  const host = createBrowserHost({
    createWebSocket() {},
    filesystem: workspace,
    filesystemTools: false,
    tools: {
      exec_command: {
        description: "Run browser bash.",
        parameters: { type: "object", required: ["cmd"] },
        handler() {},
      },
    },
  });
  await host.ready();
  assert.deepEqual(
    JSON.parse(host.toolDefinitions()).map((definition) => definition.name),
    ["exec_command", "apply_patch"],
  );
});

test("filesystem mounting rejects ambiguous application tool names", async () => {
  const workspace = await BrowserWorkspace.open({ storage: memoryOpfs() });
  const host = createBrowserHost({
    createWebSocket() {},
    filesystem: workspace,
    tools: {
      read_file: {
        description: "ambiguous override",
        parameters: { type: "object" },
        handler() {},
      },
    },
  });
  await assert.rejects(host.ready(), /tool is already configured: read_file/);
});

test("workspace paths reject traversal and foreign absolute roots", async () => {
  const workspace = await BrowserWorkspace.open({ storage: memoryOpfs() });
  await assert.rejects(workspace.readFile("../secret"), /cannot escape/);
  await assert.rejects(workspace.readFile("/tmp/secret"), /must stay within \/workspace/);
  await assert.rejects(workspace.writeFile("/workspace", "no"), /cannot write the workspace root/);
  await assert.rejects(workspace.remove("."), /cannot remove the workspace root/);
});

test("Node workspaces reuse real files and reject symlink escapes", async () => {
  const directory = await mkdtemp(join(tmpdir(), "nanocodex-workspace-"));
  const outside = await mkdtemp(join(tmpdir(), "nanocodex-outside-"));
  try {
    const first = await NodeWorkspace.open({ path: directory });
    assert.equal(first.root, "/workspace");
    await first.writeFile("src/main.rs", "fn main() {}\n");
    assert.equal(await readFile(join(directory, "src/main.rs"), "utf8"), "fn main() {}\n");

    const second = await NodeWorkspace.open({ path: directory });
    assert.equal(new TextDecoder().decode(await second.readFile("src/main.rs")), "fn main() {}\n");

    await symlink(outside, join(directory, "escape"));
    await assert.rejects(second.writeFile("escape/stolen.txt", "no"), /unsafe/);
    await assert.rejects(second.list(".", { recursive: true }), /refuses symbolic link/);

    const logical = await NodeWorkspace.open({ path: directory, root: "/project" });
    assert.equal(logical.root, "/project");
    assert.deepEqual(await logical.list("src"), [
      {
        kind: "file",
        modifiedAt: (await lstat(join(directory, "src/main.rs"))).mtimeMs,
        path: "/project/src/main.rs",
        size: 13,
      },
    ]);
    await assert.rejects(logical.readFile("escape"), (error) => {
      assert.doesNotMatch(error.message, new RegExp(escapeRegExp(directory)));
      assert.match(error.message, /symbolic link: escape/);
      return true;
    });
    await assert.rejects(logical.readFile("missing.txt"), (error) => {
      assert.equal(error.code, "ENOENT");
      assert.doesNotMatch(error.message, new RegExp(escapeRegExp(directory)));
      assert.match(error.message, /\/project\/missing\.txt/);
      return true;
    });
  } finally {
    await rm(directory, { recursive: true, force: true });
    await rm(outside, { recursive: true, force: true });
  }
});

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function memoryOpfs() {
  const root = new MemoryDirectory();
  return { getDirectory: async () => root };
}

class MemoryDirectory {
  kind = "directory";
  #entries = new Map();

  async getDirectoryHandle(name, { create = false } = {}) {
    const existing = this.#entries.get(name);
    if (existing?.kind === "directory") return existing;
    if (existing || !create) throw domError("NotFoundError");
    const directory = new MemoryDirectory();
    this.#entries.set(name, directory);
    return directory;
  }

  async getFileHandle(name, { create = false } = {}) {
    const existing = this.#entries.get(name);
    if (existing?.kind === "file") return existing;
    if (existing || !create) throw domError("NotFoundError");
    const file = new MemoryFile();
    this.#entries.set(name, file);
    return file;
  }

  async removeEntry(name, { recursive = false } = {}) {
    const existing = this.#entries.get(name);
    if (!existing) throw domError("NotFoundError");
    if (existing.kind === "directory" && existing.size && !recursive) {
      throw domError("InvalidModificationError");
    }
    this.#entries.delete(name);
  }

  async *entries() {
    yield* [...this.#entries.entries()].sort(([left], [right]) => left.localeCompare(right));
  }

  get size() {
    return this.#entries.size;
  }
}

class MemoryFile {
  kind = "file";
  #bytes = new Uint8Array();

  async getFile() {
    const bytes = this.#bytes.slice();
    return {
      size: bytes.byteLength,
      lastModified: 1,
      async arrayBuffer() {
        return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
      },
    };
  }

  async createWritable() {
    return {
      write: async (contents) => {
        this.#bytes = contents instanceof Uint8Array
          ? contents.slice()
          : new Uint8Array(contents);
      },
      close: async () => {},
      abort: async () => {},
    };
  }
}

function domError(name) {
  const error = new Error(name);
  error.name = name;
  return error;
}
