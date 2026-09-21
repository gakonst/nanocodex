import assert from "node:assert/strict";
import test from "node:test";
import { createMediaCommands, createComputerRuntime, createWorkspaceFilesystem } from "../dist/index.js";
import { createWorkspace } from "../tools/workspace.mjs";

const bytes = (text) => new TextEncoder().encode(text);
const LIMIT = 16 * 1024 * 1024;
function fixture(execute = async (request) => ({ stdout: "", stderr: "", exitCode: 0,
  files: request.command === "ffmpeg" && request.files.length ? [{ path: request.args.at(-1), data: bytes("converted") }] : [] })) {
  const entries = new Map([["", null], ["media", null], ["media/input.mov", bytes("movie")]]);
  const reads = [], writes = [], requests = [];
  const backend = {
    async list(path, options) {
      if (!entries.has(path) || entries.get(path) !== null) throw Object.assign(new Error("not a directory"), { code: "ENOENT" });
      return [...entries].filter(([name]) => name !== path && (options.recursive
        ? name.startsWith(path ? `${path}/` : "")
        : name.slice(0, Math.max(0, name.lastIndexOf("/"))) === path))
        .map(([path, data]) => ({ path, kind: data === null ? "directory" : "file", ...(data === null ? {} : { size: data.byteLength }) }));
    },
    async readFile(path) { reads.push(path); return entries.get(path).slice(); },
    async writeFile(path, data) { writes.push(path); entries.set(path, data.slice()); },
    async mkdir(path) { entries.set(path, null); },
    async remove(path) { entries.delete(path); },
  };
  const workspace = createWorkspace({ backend, root: "/brain" });
  const options = { filesystem: () => workspace, execute: async (request, signal) => {
    requests.push(request); return execute(request, signal);
  } };
  const [ffmpeg, ffprobe] = createMediaCommands(options);
  return { entries, backend, workspace, options, ffmpeg, ffprobe, requests, reads, writes };
}
const context = { cwd: "/brain/media" };
const convert = ["-i", "input.mov", "output.wav"];

test("probe forwards the requested fields and only the rewritten input bytes", async () => {
  const f = fixture(async () => ({ stdout: '{"format":{"duration":"2"}}\n', stderr: "", exitCode: 0, files: [] }));
  const args = ["-v", "error", "-show_entries", "format=duration,size:stream=index,codec_type,codec_name,width,height,r_frame_rate,sample_rate,channels", "-of", "json", "input.mov"];
  assert.equal((await f.ffprobe.execute(args, context)).exitCode, 0);
  assert.deepEqual(f.requests[0], { command: "ffprobe", args: [...args.slice(0, -1), "-protocol_whitelist", "file", "/input.mov"], files: [{ path: "/input.mov", data: bytes("movie") }] });
  assert.deepEqual(f.writes, []);
});

for (const [name, args, rewritten] of [
  ["contact sheet", ["-hide_banner", "-loglevel", "error", "-i", "input.mov", "-vf", "fps=1/2,scale=643:-1,tile=3x2", "-frames:v", "1", "output.jpg"], "/output.jpg"],
  ["mono audio", ["-hide_banner", "-loglevel", "error", "-ss", "00:00:01.5", "-i", "input.mov", "-t", "2", "-vn", "-ac", "1", "-ar", "16000", "output.wav"], "/output.wav"],
]) test(`${name} preserves supported options and rewrites workspace paths`, async () => {
  const f = fixture();
  assert.equal((await f.ffmpeg.execute(args, context)).exitCode, 0);
  assert.equal(f.requests[0].args.at(-1), rewritten);
  assert.deepEqual(f.requests[0].args, ["-nostdin", "-y", ...args.flatMap((arg) => arg === "-i" ? ["-protocol_whitelist", "file", "-i"] : [arg === "input.mov" ? "/input.mov" : arg.startsWith("output.") ? rewritten : arg])]);
  assert.equal(new TextDecoder().decode(f.entries.get(`media/${args.at(-1)}`)), "converted");
});

for (const args of [
  ["-i", "https://example.test/a.mov", "output.wav"], ["-i", "file:input.mov", "output.wav"],
  ["-i", "../../outside.mov", "output.wav"], ["-i", "/brain-other/input.mov", "output.wav"],
  ["-i", "input.mov", "/outside.wav"], ["-i", "input.mov", "pipe:1"],
  ["-i", "input.mov", "out.m3u8"], ["-i", "input.mov", "out.mpd"],
  ["-i", "input.mov", "out%02d.jpg"], ["-i", "input.mov", "-i", "input.mov", "out.wav"],
  ["-i", "input.mov", "a.wav", "b.wav"], ["-i", "input.mov", "out.wav", "-y"],
  ["-i", "input.mov", "-filter_script", "/brain/secret", "out.wav"],
  ["-i", "input.mov", "-vf", "movie=/brain/secret", "out.wav"],
  ["-i", "input.mov", "-vf", "scale=10:10;movie=http://example.test", "out.wav"],
  ["-i", "input.mov", "-vf", "drawtext=textfile=/brain/secret", "out.wav"],
  ["-i", "input.mov", "-vf", "scale=10:10[out]", "out.wav"],
  ["-i", "input.mov", "-af", "amovie=secret.wav", "out.wav"],
  ["-i", "input.mov", "-f", "hls", "out.m3u8"],
  ["-f", "concat", "-i", "input.mov", "out.wav"],
  ["-report", "-i", "input.mov", "out.wav"],
  ["-i", "input.mov", "-pass", "1", "out.wav"],
  ["-i", "input.mov", "-protocol_whitelist", "http,file", "out.wav"],
  ["-i", "input.mov", "-c:v", "-report", "out.wav"],
  ["-i", "input.mov", "-attach", "secret", "out.wav"],
  ["-i", "input.mov", "-y", "-n", "out.wav"],
  ["-i"], ["-version", "-i", "input.mov", "out.wav"],
  ["-i", "input.mov\u0000", "out.wav"], ["-i", "input.mov", "-vf", "x".repeat(4097), "out.wav"],
  Array(97).fill("-y"), Array(20).fill("x".repeat(1000)),
]) test(`rejects unsafe or malformed arguments ${JSON.stringify(args).slice(0, 100)}`, async () => {
  const f = fixture();
  assert.equal((await f.ffmpeg.execute(args, context)).exitCode, 1);
  assert.deepEqual(f.requests, []);
  assert.deepEqual(f.reads, []);
  assert.deepEqual(f.writes, []);
});

test("probe rejects output files, script writers, and input option injection", async () => {
  const f = fixture();
  for (const args of [["input.mov", "output.json"], ["-of", "json=filename=secret", "input.mov"], ["-i", "input.mov"], ["-show_entries", "-report", "input.mov"]]) {
    assert.equal((await f.ffprobe.execute(args, context)).exitCode, 1);
  }
  assert.deepEqual(f.requests, []);
});

test("requires an existing output parent and explicit overwrite; cannot overwrite input", async () => {
  const f = fixture();
  f.entries.set("media/output.wav", bytes("original"));
  for (const args of [convert, ["-n", ...convert], ["-i", "input.mov", "missing/out.wav"], ["-y", "-i", "input.mov", "./input.mov"]]) {
    assert.equal((await f.ffmpeg.execute(args, context)).exitCode, 1);
  }
  assert.deepEqual(f.requests, []);
  assert.equal((await f.ffmpeg.execute(["-y", ...convert], context)).exitCode, 0);
  assert.equal(new TextDecoder().decode(f.entries.get("media/output.wav")), "converted");
});

test("checks stat size before read and checks bytes again after read", async () => {
  for (const size of [LIMIT + 1, undefined, -1, NaN]) {
    const f = fixture();
    const list = f.backend.list;
    f.backend.list = async (...args) => (await list(...args)).map((entry) => entry.kind === "file" ? { ...entry, size } : entry);
    assert.equal((await f.ffmpeg.execute(convert, context)).exitCode, 1);
    assert.deepEqual(f.reads, []);
    assert.deepEqual(f.requests, []);
  }
  const f = fixture();
  f.backend.readFile = async () => new Uint8Array(LIMIT + 1);
  assert.equal((await f.ffmpeg.execute(convert, context)).exitCode, 1);
  assert.deepEqual(f.requests, []);
});

for (const [name, execute] of [
  ["backend failure", async () => ({ stdout: "", stderr: "failed", exitCode: 3, files: [{ path: "/output.wav", data: bytes("partial") }] })],
  ["backend exception", async () => { throw new Error("backend failed"); }],
  ["oversized output", async () => ({ stdout: "", stderr: "", exitCode: 0, files: [{ path: "/output.wav", data: new Uint8Array(LIMIT + 1) }] })],
  ["empty output", async () => ({ stdout: "", stderr: "", exitCode: 0, files: [{ path: "/output.wav", data: new Uint8Array() }] })],
  ["missing output", async () => ({ stdout: "", stderr: "", exitCode: 0, files: [] })],
  ["wrong output", async () => ({ stdout: "", stderr: "", exitCode: 0, files: [{ path: "/brain/other.wav", data: bytes("bad") }] })],
  ["duplicate output", async () => ({ stdout: "", stderr: "", exitCode: 0, files: Array(2).fill({ path: "/output.wav", data: bytes("bad") }) })],
  ["malformed result", async () => ({ stdout: {}, stderr: "", exitCode: 0, files: [] })],
]) test(`${name} preserves existing output and creates no failed file`, async () => {
  for (const existing of [false, true]) {
    const f = fixture(execute);
    if (existing) f.entries.set("media/output.wav", bytes("original"));
    assert.notEqual((await f.ffmpeg.execute(["-y", ...convert], context)).exitCode, 0);
    assert.deepEqual(f.writes, []);
    assert.deepEqual(f.entries.get("media/output.wav"), existing ? bytes("original") : undefined);
  }
});

test("passes cancellation to executor and checks it before committing output", async () => {
  const controller = new AbortController();
  const f = fixture(async (request, signal) => {
    assert.equal(signal, controller.signal);
    controller.abort(new Error("cancelled"));
    return { stdout: "", stderr: "", exitCode: 0, files: [{ path: "/output.wav", data: bytes("partial") }] };
  });
  f.entries.set("media/output.wav", bytes("original"));
  assert.equal((await f.ffmpeg.execute(["-y", ...convert], { ...context, signal: controller.signal })).exitCode, 130);
  assert.deepEqual(f.writes, []);
  assert.deepEqual(f.entries.get("media/output.wav"), bytes("original"));
  assert.equal((await f.ffmpeg.execute(convert, { ...context, signal: controller.signal })).exitCode, 130);
  assert.equal(f.requests.length, 1);
});

test("rechecks no-clobber after backend execution", async () => {
  const f = fixture(async () => {
    f.entries.set("media/output.wav", bytes("concurrent"));
    return { stdout: "", stderr: "", exitCode: 0, files: [{ path: "/output.wav", data: bytes("converted") }] };
  });
  assert.equal((await f.ffmpeg.execute(convert, context)).exitCode, 1);
  assert.deepEqual(f.writes, []);
  assert.deepEqual(f.entries.get("media/output.wav"), bytes("concurrent"));
});

test("help is local; version and formats use a file-free executor; text is bounded", async () => {
  const f = fixture(async () => ({ stdout: "😀".repeat(100_000), stderr: "x".repeat(100_000), exitCode: 0, files: [] }));
  assert.match((await f.ffmpeg.execute(["-help"])).stdout, /16 MiB/);
  assert.deepEqual(f.requests, []);
  for (const arg of ["-version", "-formats"]) {
    const result = await f.ffprobe.execute([arg]);
    assert.equal(result.exitCode, 0);
    assert.ok(Buffer.byteLength(result.stdout) <= 65536);
    assert.ok(Buffer.byteLength(result.stderr) <= 65536);
    assert.deepEqual(f.requests.at(-1).files, []);
  }
  assert.deepEqual(f.reads, []);
});

test("real Just Bash integration handles cwd, quoting, and persistent output", async () => {
  const f = fixture();
  const runtime = await createComputerRuntime({ filesystem: f.workspace, networkMode: "disabled", fetch: async () => { throw new Error("unexpected fetch"); },
    commands: ({ filesystem }) => createMediaCommands({ ...f.options, filesystem }) });
  const result = await runtime.tool.handler({ cmd: "ffmpeg -i input.mov -vf 'fps=1/2,scale=643:-1,tile=3x2' -frames:v 1 sheet.jpg", workdir: "/brain/media" }, { signal: new AbortController().signal });
  assert.equal(result.exit_code, 0, JSON.stringify(result));
  assert.deepEqual(await runtime.filesystem.readFile("/brain/media/sheet.jpg"), bytes("converted"));
});

test("storage workspace rejects symlink ancestors before handing bytes to executor", async () => {
  const directory = { size: 0, isDirectory: true, isFile: false, isSymbolicLink: false };
  const workspace = await createWorkspaceFilesystem({ fs: {
    async lstat(path) { return path === "/workspace/link" ? { ...directory, isSymbolicLink: true } : directory; },
    async readdir() { throw new Error("must reject symlink before listing"); },
    async readFile() { throw new Error("must not read symlink"); },
    async writeFile() { throw new Error("must not write symlink"); },
    async mkdir() {}, async rm() {},
  } });
  const [, ffprobe] = createMediaCommands({ filesystem: () => workspace, execute: async () => { assert.fail("must not execute"); } });
  const result = await ffprobe.execute(["link/input.mov"]);
  assert.equal(result.exitCode, 1);
  assert.match(result.stderr, /symbolic link/);
});


test("ephemeral output always overwrites while workspace -n/-y policy stays local", async () => {
  for (const overwriteArgs of [[], ["-n"], ["-y"]]) {
    const f = fixture();
    assert.equal((await f.ffmpeg.execute([...overwriteArgs, ...convert], context)).exitCode, 0);
    assert.equal(f.requests[0].args.filter(arg => arg === "-y").length, 1);
    assert.equal(f.requests[0].args.includes("-n"), false);
  }
  const f = fixture();
  f.entries.set("media/output.wav", bytes("original"));
  assert.equal((await f.ffmpeg.execute(["-n", ...convert], context)).exitCode, 1);
  assert.deepEqual(f.requests, []);
  assert.deepEqual(f.entries.get("media/output.wav"), bytes("original"));
});
