import assert from "node:assert/strict";
import test from "node:test";
import { pdf } from "./fixtures/pdf.mjs";
import { createPdfTextCommand, createComputerRuntime } from "../dist/index.js";
import { createWorkspace } from "../tools/workspace.mjs";


function fixture() {
  const files = new Map([["", null], ["docs", null], ["docs/sample file.pdf", pdf()]]);
  const writes = [];
  const backend = {
    async list(path) {
      if (!files.has(path) || files.get(path) !== null) throw new Error("not a directory");
      return [...files].filter(([name]) => name !== path && name.slice(0, Math.max(0, name.lastIndexOf("/"))) === path)
        .map(([path, bytes]) => ({ path, kind: bytes === null ? "directory" : "file" }));
    },
    async readFile(path) { if (!files.get(path)) throw new Error("missing file"); return new Uint8Array(files.get(path)); },
    async writeFile(path, data) { writes.push(path); files.set(path, new Uint8Array(data)); },
    async mkdir(path) { files.set(path, null); },
    async remove(path) { files.delete(path); },
  };
  const workspace = createWorkspace({ backend, root: "/brain" });
  return { files, writes, workspace, command: createPdfTextCommand(() => workspace) };
}
const context = { cwd: "/brain/docs" };

test("extracts compressed pages and Unicode to stdout without writing files", async () => {
  const f = fixture();
  const result = await f.command.execute(["sample file.pdf", "-"], context);
  assert.equal(result.exitCode, 0, result.stderr);
  assert.match(result.stdout, /Alpha Beta\nAmount: 100\n\fPage two: Ω\n\f/);
  assert.deepEqual(f.writes, []);
});

test("page ranges, form feeds, layout and default output", async () => {
  const f = fixture();
  let result = await f.command.execute(["-f", "2", "-l", "99", "-nopgbrk", "sample file.pdf", "-"], context);
  assert.equal(result.stdout, "Page two: Ω\n", result.stderr);
  result = await f.command.execute(["-layout", "-f", "1", "-l", "1", "sample file.pdf", "-"], context);
  assert.match(result.stdout, /Alpha {2,}Beta/);
  result = await f.command.execute(["sample file.pdf"], context);
  assert.equal(result.exitCode, 0, result.stderr);
  assert.equal(result.stdout, "");
  assert.match(new TextDecoder().decode(f.files.get("docs/sample file.txt")), /Page two: Ω/);
});

for (const args of [
  ["-f", "0", "sample file.pdf", "-"], ["-l", "-1", "sample file.pdf", "-"],
  ["-f", "3", "sample file.pdf", "-"], ["-f", "2", "-l", "1", "sample file.pdf", "-"],
  ["-enc", "Latin1", "sample file.pdf", "-"], ["-htmlmeta", "sample file.pdf", "-"],
  ["-layout", "-raw", "sample file.pdf", "-"], ["-", "-"],
  ["sample file.pdf", "sample file.pdf"], ["sample file.pdf", "./sample file.pdf"],
  ["../../secret.pdf", "-"], ["https://example.test/test.pdf", "-"],
  ["sample file.pdf", "/outside.txt"], ["sample file.pdf", "missing/out.txt"],
]) test(`rejects invalid options/paths ${JSON.stringify(args)}`, async () => {
  const f = fixture();
  const result = await f.command.execute(args, context);
  assert.notEqual(result.exitCode, 0);
  assert.deepEqual(f.writes, []);
});

test("malformed PDF preserves existing output", async () => {
  const f = fixture();
  f.files.set("docs/sample file.pdf", new TextEncoder().encode("not a PDF"));
  f.files.set("docs/out.txt", new TextEncoder().encode("keep me"));
  const result = await f.command.execute(["sample file.pdf", "out.txt"], context);
  assert.notEqual(result.exitCode, 0);
  assert.deepEqual(f.writes, []);
  assert.equal(new TextDecoder().decode(f.files.get("docs/out.txt")), "keep me");
});

test("abort and help do not read a document", async () => {
  const f = fixture(), controller = new AbortController();
  controller.abort();
  assert.equal((await f.command.execute(["missing.pdf", "-"], { ...context, signal: controller.signal })).exitCode, 130);
  assert.equal((await f.command.execute(["--help"])).exitCode, 0);
  assert.deepEqual(f.writes, []);
});

test("Just Bash command is installed by default, with quoting, pipes and redirection", async () => {
  const f = fixture();
  const runtime = await createComputerRuntime({ filesystem: f.workspace, networkMode: "disabled", fetch: async () => { throw new Error("unexpected network"); } });
  assert.ok(runtime.commandNames.includes("pdftotext"));
  const result = await runtime.tool.handler({ cmd: "pdftotext -f 2 -nopgbrk 'sample file.pdf' - | cat > extracted.txt", workdir: "/brain/docs" }, { signal: new AbortController().signal });
  assert.equal(result.exit_code, 0, JSON.stringify(result));
  assert.equal(new TextDecoder().decode(await runtime.filesystem.readFile("/brain/docs/extracted.txt")), "Page two: Ω\n");
});
