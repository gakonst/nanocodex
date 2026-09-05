import assert from "node:assert/strict";
import test from "node:test";

import { justBash } from "../tools/bash.mjs";

test("Just Bash advertises its cloud workspace execution", async () => {
  const { descriptor, instructions, tool } = await justBash({ filesystem: memoryWorkspace() });
  assert.equal(tool.provider, undefined);
  assert.equal(
    tool.description,
    "Runs a shell command, returning output or a session ID for ongoing interaction.",
  );
  assert.equal(descriptor.cwd, "/workspace");
  assert.equal(descriptor.shell, "nanocodex-just-bash");
  assert.equal(descriptor.network.enabled, false);
  assert.equal(descriptor.network.mode, "disabled");
  assert.equal(descriptor.pty, false);
  assert.equal(descriptor.sessions, false);
  assert.equal(descriptor.sandboxEscalation, false);
  assert.equal(descriptor.limits.maxFileSystemBytes, 64 * 1024 * 1024);
  assert.equal(descriptor.limits.maxTraversalEntries, 2_000);
  assert(descriptor.commands.includes("grep"));
  assert(!descriptor.commands.includes("curl"));
  assert(!descriptor.commands.includes("wget"));
  assert.match(instructions, /Available commands:/);
  assert.match(
    instructions,
    /call exec_command immediately and once with the complete command/,
  );
  assert.match(instructions, /exactly gh repo clone OWNER\/REPO DESTINATION/);
  assert.match(instructions, /git clone URL DESTINATION/);
  assert.match(instructions, /Do not add depth, filter, branch, or other flags/);
  assert.doesNotMatch(instructions, /\bwget\b/);
});

test("Just Bash mounts one persistent workspace without a process sandbox", async () => {
  const workspace = memoryWorkspace();
  const first = await justBash({ filesystem: workspace });
  const written = await first.tool.handler({
    cmd: "mkdir -p notes && printf 'forty two\\n' > notes/answer.txt && cat notes/answer.txt",
    justification: "advisory for a host that supports approvals",
    login: true,
    yield_time_ms: 10_000,
    prefix_rule: ["mkdir"],
  }, context());

  assert.equal(written.exit_code, 0);
  assert.equal(written.output, "forty two\n");
  assert.equal(
    new TextDecoder().decode(await first.filesystem.readFile("/workspace/notes/answer.txt")),
    "forty two\n",
  );

  const reopened = await justBash({ filesystem: workspace });
  const persisted = await reopened.tool.handler({ cmd: "cat notes/answer.txt" }, context());
  assert.equal(persisted.output, "forty two\n");
});

test("the returned filesystem is the authoritative bounded mutation handle", async () => {
  const source = memoryWorkspace();
  const runtime = await justBash({ filesystem: source });
  assert.notEqual(runtime.filesystem, source);
  await runtime.filesystem.writeFile("/workspace/from-rust.txt", "shared boundary\n");

  const result = await runtime.tool.handler({ cmd: "cat from-rust.txt" }, context());
  assert.equal(result.exit_code, 0);
  assert.equal(result.output, "shared boundary\n");
  await assert.rejects(
    runtime.filesystem.writeFile("/tmp/escape.txt", "no"),
    /escapes \/workspace/,
  );
});

test("workspace paths cannot escape the mounted root", async () => {
  const source = memoryWorkspace();
  const runtime = await justBash({ filesystem: source });

  await assert.rejects(
    runtime.tool.handler({ cmd: "pwd", workdir: "/tmp" }, context()),
    /path escapes \/workspace/,
  );
  await assert.rejects(
    runtime.filesystem.writeFile("../outside.txt", "no"),
    /path escapes \/workspace/,
  );
  await assert.rejects(source.readFile("/outside.txt"), { code: "ENOENT" });
});

test("network access is absent by default and an empty allow-list denies egress", async () => {
  const originalFetch = globalThis.fetch;
  let fetchCalls = 0;
  globalThis.fetch = async () => {
    fetchCalls += 1;
    throw new Error("test must not reach host fetch");
  };
  try {
    const disabled = await justBash({ filesystem: memoryWorkspace() });
    const unavailable = await disabled.tool.handler({
      cmd: "curl https://example.invalid",
    }, context());
    assert.equal(unavailable.exit_code, 127);
    assert.match(unavailable.output, /curl: command not found/);

    const restricted = await justBash({
      filesystem: memoryWorkspace(),
      network: { allowedUrlPrefixes: [] },
    });
    const denied = await restricted.tool.handler({
      cmd: "curl -sS https://example.invalid",
    }, context());
    assert.equal(denied.exit_code, 7);
    assert.match(denied.output, /Network access denied: URL not in allow-list/);
    assert.equal(fetchCalls, 0);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("the host can inject one secure fetch boundary and app-owned commands", async () => {
  const secureFetch = async (url) => ({
    status: 200,
    statusText: "OK",
    headers: { "content-type": "text/plain" },
    body: new TextEncoder().encode(`fetched ${url}`),
    url,
  });
  const runtime = await justBash({
    filesystem: memoryWorkspace(),
    fetch: secureFetch,
    customCommands: [{
      name: "mock-tool",
      trusted: true,
      async execute(args) {
        return { stdout: `${args.join("|")}\n`, stderr: "", exitCode: 0 };
      },
    }],
  });
  assert.equal(runtime.descriptor.network.enabled, true);
  assert.equal(runtime.descriptor.network.mode, "host-fetch");
  assert(runtime.descriptor.commands.includes("curl"));
  assert(runtime.descriptor.commands.includes("mock-tool"));
  assert.deepEqual(runtime.descriptor.customCommands, ["mock-tool"]);

  const custom = await runtime.tool.handler({ cmd: "mock-tool one two" }, context());
  assert.equal(custom.exit_code, 0);
  assert.equal(custom.output, "one|two\n");
  assert.equal(typeof custom.wall_time_seconds, "number");
  const fetched = await runtime.tool.handler({ cmd: "curl -s https://example.com/data" }, context());
  assert.equal(fetched.exit_code, 0);
  assert.equal(fetched.output, "fetched https://example.com/data");
});

test("caller cancellation and the runtime deadline stop execution", async () => {
  const cancellable = await justBash({
    filesystem: memoryWorkspace(),
    executionTimeoutMs: 1_000,
  });
  const cancellation = new AbortController();
  const cancelled = cancellable.tool.handler(
    { cmd: "sleep 10" },
    { sessionId: "test", signal: cancellation.signal },
  );
  queueMicrotask(() => cancellation.abort(new Error("caller cancelled")));
  const cancelledResult = await cancelled;
  assert.equal(cancelledResult.exit_code, 124);
  assert.match(cancelledResult.output, /execution aborted/);

  const timed = await justBash({
    filesystem: memoryWorkspace(),
    executionTimeoutMs: 5,
  });
  const timedResult = await timed.tool.handler({ cmd: "sleep 10" }, context());
  assert.equal(timedResult.exit_code, 124);
  assert.match(timedResult.output, /execution (?:aborted|deadline)/);
  assert.ok(timedResult.wall_time_seconds < 1);
});

test("initial metadata, mutations, and returned output stay within configured bounds", async () => {
  let defaultScan;
  await justBash({
    filesystem: memoryWorkspace({
      onList(path, options) {
        defaultScan = { path, options };
      },
    }),
  });
  assert.deepEqual(defaultScan, {
    path: ".",
    options: { recursive: true, maxEntries: 2_000 },
  });

  let initialScan;
  const source = memoryWorkspace({
    onList(path, options) {
      initialScan = { path, options };
    },
  });
  const bounded = await justBash({ filesystem: source, maxEntries: 2, maxOutputTokens: 100 });
  assert.deepEqual(initialScan, {
    path: ".",
    options: { recursive: true, maxEntries: 2 },
  });

  const entryResult = await bounded.tool.handler({ cmd: "touch one two three" }, context());
  assert.equal(entryResult.exit_code, 1);
  assert.match(entryResult.output, /workspace exceeds 2 entries/);
  assert.equal((await source.readFile("/workspace/two")).byteLength, 0);
  await assert.rejects(source.readFile("/workspace/three"), { code: "ENOENT" });

  const outputResult = await bounded.tool.handler({
    cmd: "printf 1234567890123456789012345678901234567890123456789012345678901234567890",
    max_output_tokens: 16,
  }, context());
  assert.equal(outputResult.exit_code, 0);
  assert.equal(outputResult.output.length, 64);
  assert.match(outputResult.output, /\n\[output truncated by exec_command\]$/);
  assert.equal(outputResult.original_token_count, 18);
});

function context() {
  return { sessionId: "test", signal: new AbortController().signal };
}

function memoryWorkspace({ onList } = {}) {
  const files = new Map();
  const directories = new Set(["/workspace"]);
  return {
    root: "/workspace",
    async list(path, options) {
      onList?.(path, options);
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
      files.set(path, toBytes(contents));
      const segments = path.split("/").slice(1, -1);
      let current = "";
      for (const segment of segments) {
        current += `/${segment}`;
        directories.add(current);
      }
    },
    async remove(path, options = {}) {
      files.delete(path);
      if (options.recursive) {
        for (const candidate of files.keys()) {
          if (candidate.startsWith(`${path}/`)) files.delete(candidate);
        }
        for (const candidate of directories) {
          if (candidate === path || candidate.startsWith(`${path}/`)) directories.delete(candidate);
        }
      }
    },
    async mkdir(path) {
      const segments = path.split("/").slice(1);
      let current = "";
      for (const segment of segments) {
        current += `/${segment}`;
        directories.add(current);
      }
    },
  };
}

function toBytes(value) {
  if (typeof value === "string") return new TextEncoder().encode(value);
  if (value instanceof Uint8Array) return value.slice();
  if (value instanceof ArrayBuffer) return new Uint8Array(value.slice(0));
  return new Uint8Array(value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength));
}
