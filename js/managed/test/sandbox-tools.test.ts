import { describe, expect, it, vi } from "vitest";

import {
  createCloudflareSandboxTools,
  deleteCloudflareSandboxWorkspace,
} from "../src/sandbox-tools";

const context = {
  callId: "call",
  model: "gpt-5.6-sol",
  parentCallId: "parent",
  sessionId: "session",
  signal: new AbortController().signal,
};

describe("Cloudflare sandbox tools", () => {
  it("creates one sandbox lazily across explicit tool calls", async () => {
    const sandbox = fakeSandbox();
    const create = vi.fn(async () => sandbox);
    const tools = createCloudflareSandboxTools(create);

    await tools.sandbox_exec!.handler({ command: "uname -a" }, context);
    await tools.sandbox_write_file!.handler({ path: "proof.txt", content: "ok" }, context);

    expect(create).toHaveBeenCalledTimes(1);
    expect(sandbox.exec).toHaveBeenCalledWith("uname -a", {
      cwd: "/workspace",
      timeout: 60_000,
    });
  });

  it("deletes every persisted object owned by a removed sandbox", async () => {
    const pages = [
      { objects: [{ key: "/sessions/session/a" }, { key: "/sessions/session/b" }] },
      { objects: [{ key: "/sessions/session/c" }] },
      { objects: [] },
    ];
    const bucket = {
      list: vi.fn(async () => pages.shift()!),
      delete: vi.fn(async () => {}),
    } as unknown as R2Bucket;

    await deleteCloudflareSandboxWorkspace(bucket, "session");

    expect(bucket.list).toHaveBeenCalledTimes(3);
    expect(bucket.list).toHaveBeenCalledWith({ prefix: "/sessions/session/", limit: 1_000 });
    expect(bucket.delete).toHaveBeenCalledWith([
      "/sessions/session/a",
      "/sessions/session/b",
    ]);
    expect(bucket.delete).toHaveBeenCalledWith(["/sessions/session/c"]);
  });
});

function fakeSandbox() {
  return {
    exec: vi.fn(async () => ({
      success: true,
      exitCode: 0,
      stdout: "",
      stderr: "",
      duration: 1,
    })),
    startProcess: vi.fn(async (command: string) => ({
      id: "process",
      pid: 1,
      command,
      status: "running",
      getStatus: vi.fn(async () => "running"),
      waitForPort: vi.fn(async () => {}),
    })),
    readFile: vi.fn(async () => ({
      size: 0,
      content: new ReadableStream<Uint8Array>({ start(controller) { controller.close(); } }),
    })),
    writeFile: vi.fn(async () => {}),
    listFiles: vi.fn(async () => ({ files: [] })),
    tunnels: { get: vi.fn(async () => ({ url: "https://preview.example" })) },
  };
}
