import { env } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import type { ToolContext } from "nanocodex";
import { viewImage } from "nanocodex/tools";
import type { WorkspaceStorageClient } from "nanocodex-tools";
import { createBrainWorkspace } from "../src/brain-workspace";
import { createManagedComputerRuntime } from "../src/computer-runtime";
import { createManagedNamespaceTools, createSharedBrainReadWorkspace } from "../src/index";

const bucket = (env as unknown as { NANOCODEX_HISTORY: R2Bucket }).NANOCODEX_HISTORY;
const context = (sessionId = "root"): ToolContext => ({
  sessionId, callId: crypto.randomUUID(), parentCallId: "", model: "test",
  signal: new AbortController().signal,
});
const computer = () => ({
  get fs(): WorkspaceStorageClient["fs"] { throw new Error("brain must not open a computer filesystem"); },
  [Symbol.dispose]: vi.fn(),
});

describe("durable brain without hands", () => {
  it("fences the retained R2 handle after disposal", async () => {
    const workspace = createBrainWorkspace(bucket, crypto.randomUUID());
    const runtime = await createManagedComputerRuntime({
      computer: computer(), filesystem: workspace, egress: { fetch: vi.fn() } as unknown as Fetcher,
    });
    runtime.dispose();
    expect(() => runtime.tool.handler({ cmd: "echo stale > stale" }, context())).toThrow("runtime is disposed");
    expect(await workspace.list()).toEqual([]);
  });

  it("cancels a stalled HTTP body when its tool call is aborted", async () => {
    let started!: () => void;
    const requestStarted = new Promise<void>((resolve) => { started = resolve; });
    const cancel = vi.fn();
    const runtime = await createManagedComputerRuntime({
      computer: computer(), filesystem: createBrainWorkspace(bucket, crypto.randomUUID()),
      subject: "s".repeat(43), connectorAllowed: () => true,
      egress: { async fetch() {
        started();
        return new Response(new ReadableStream<Uint8Array>({ cancel }));
      } } as unknown as Fetcher,
    });
    try {
      const controller = new AbortController();
      const pending = runtime.tool.handler({ cmd: "gh api user" }, { ...context(), signal: controller.signal });
      await requestStarted;
      controller.abort(new Error("caller cancelled"));
      await expect(pending).resolves.toMatchObject({ exit_code: 124 });
      expect(cancel).toHaveBeenCalledOnce();
    } finally { runtime.dispose(); }
  });

  it("cancels oversized chunked HTTP responses before buffering them in full", async () => {
    let chunks = 0;
    const cancel = vi.fn();
    const runtime = await createManagedComputerRuntime({
      computer: computer(), filesystem: createBrainWorkspace(bucket, crypto.randomUUID()),
      subject: "s".repeat(43), connectorAllowed: () => true,
      egress: { async fetch() {
        return new Response(new ReadableStream<Uint8Array>({
          pull(controller) { chunks++; controller.enqueue(new Uint8Array(1024 * 1024)); }, cancel,
        }));
      } } as unknown as Fetcher,
    });
    try {
      await expect(runtime.tool.handler({ cmd: "gh api user" }, context()))
        .resolves.toMatchObject({ exit_code: 1, output: expect.stringContaining("exceeds 16 MiB") });
      expect(cancel).toHaveBeenCalledOnce();
      expect(chunks).toBeLessThanOrEqual(18);
      await expect(runtime.tool.handler({ cmd: "echo still-working" }, context()))
        .resolves.toMatchObject({ exit_code: 0, output: "still-working\n" });
    } finally { runtime.dispose(); }
  });

  it("executes default, dot, and /brain cwd; reopens files; sees native R2 changes", async () => {
    const id = crypto.randomUUID();
    const filesystem = createBrainWorkspace(bucket, id);
    const runtime = await createManagedComputerRuntime({
      computer: computer(), filesystem, egress: { fetch: vi.fn() } as unknown as Fetcher,
    });
    const prepareHand = vi.fn(async () => { throw new Error("hand unavailable"); });
    const listHands = vi.fn(() => { throw new Error("hand discovery unavailable"); });
    const resolveHand = vi.fn();
    const tools = createManagedNamespaceTools(() => false, listHands, resolveHand, prepareHand, {
      tool: runtime.tool, allowed: () => true,
    });
    const exec = tools.find((tool) => tool.name === "exec_command")!;
    try {
      const denied = createManagedNamespaceTools(() => false, () => [], resolveHand, prepareHand, {
        tool: runtime.tool, allowed: () => false,
      }).find((tool) => tool.name === "exec_command")!;
      await expect(denied.handler({ cmd: "echo denied > denied" }, context()))
        .rejects.toThrow("cannot use brain tools");
      expect(await filesystem.list()).toEqual([]);
      for (const workdir of [undefined, ".", "/brain"]) {
        await expect(exec.handler({ cmd: "pwd && echo no && true", workdir }, context()))
          .resolves.toMatchObject({ exit_code: 0, output: "/brain\nno\n" });
      }
      await expect(exec.handler({ cmd: "mkdir -p notes && printf 'persistent\\n' > notes/result" }, context()))
        .resolves.toMatchObject({ exit_code: 0 });
      expect(await (await bucket.get(`brains/${id}/notes/result`))!.text()).toBe("persistent\n");
      await expect(exec.handler({ cmd: "printf iVBORw0KGgo= | base64 -d > notes/pixel.png" }, context()))
        .resolves.toMatchObject({ exit_code: 0 });
      const image = viewImage({ workspace: createSharedBrainReadWorkspace(bucket, id, filesystem) });
      for (const path of ["notes/pixel.png", "/brain/notes/pixel.png"]) {
        await expect(image.handler({ path }, context())).resolves.toMatchObject({
          output: [{ type: "input_image", image_url: "data:image/png;base64,iVBORw0KGgo=" }],
        });
      }
      await bucket.put(`brains/${id}/notes/native`, "written by a hand\n");
      await bucket.delete(`brains/${id}/notes/result`);
      await expect(exec.handler({ cmd: "cat native && test ! -e result", workdir: "notes" }, context()))
        .resolves.toMatchObject({ exit_code: 0, output: "written by a hand\n" });
      await expect(exec.handler({ cmd: "pwd", workdir: "/brain/../hand" }, context()))
        .rejects.toThrow("cannot use execution hands");
      expect(prepareHand).not.toHaveBeenCalled();
      expect(listHands).not.toHaveBeenCalled();
      expect(resolveHand).not.toHaveBeenCalled();
      const reopened = await createManagedComputerRuntime({
        computer: computer(), filesystem: createBrainWorkspace(bucket, id),
        egress: { fetch: vi.fn() } as unknown as Fetcher,
      });
      try {
        await expect(reopened.tool.handler({ cmd: "cat notes/native" }, context()))
          .resolves.toMatchObject({ exit_code: 0, output: "written by a hand\n" });
      } finally { reopened.dispose(); }
    } finally { runtime.dispose(); }
  });

  it("preserves empty directories and fences paths, file ancestors, bounds, and other agents", async () => {
    const id = crypto.randomUUID();
    const workspace = createBrainWorkspace(bucket, id);
    const other = createBrainWorkspace(bucket, crypto.randomUUID());
    await workspace.writeFile("a/b/file", "hello");
    await workspace.remove("a/b/file");
    expect(await workspace.list("a", { recursive: true })).toMatchObject([{ path: "/brain/a/b", kind: "directory" }]);
    await workspace.writeFile("a/b/file", "hello");
    await expect(workspace.remove("a")).rejects.toMatchObject({ code: "ENOTEMPTY" });
    await expect(workspace.list(".", { recursive: true, maxEntries: 2 })).rejects.toThrow("exceeds 2 entries");
    await expect(other.readFile("a/b/file")).rejects.toMatchObject({ code: "ENOENT" });
    await expect(workspace.writeFile("../outside", "no")).rejects.toMatchObject({ code: "EPERM" });
    await expect(workspace.writeFile("a/b/file/child", "no")).rejects.toMatchObject({ code: "ENOTDIR" });
    await expect(workspace.writeFile("a", "no")).rejects.toMatchObject({ code: "EISDIR" });
    await workspace.remove("a", { recursive: true });
    expect(await workspace.list()).toEqual([]);
    expect((await bucket.list({ prefix: `brains/${id}/` })).objects).toEqual([]);
  });

  it("keeps queued connector calls scoped to the calling agent and never exposes gateway headers", async () => {
    const seen: Request[] = [];
    const allowed = vi.fn((_connector, _connection, call?: ToolContext) => call?.sessionId === "allowed");
    const runtime = await createManagedComputerRuntime({
      computer: computer(), filesystem: createBrainWorkspace(bucket, crypto.randomUUID()),
      subject: "s".repeat(43), connectorAllowed: allowed,
      vaultAllowed: () => false,
      egress: { async fetch(request: Request) {
        seen.push(request);
        return Response.json({ login: "fixture" }, { headers: { "x-nanocodex-subject": "private-subject" } });
      } } as unknown as Fetcher,
    });
    try {
      const [permitted, denied] = await Promise.all([
        runtime.tool.handler({ cmd: "gh auth status" }, context("allowed")),
        runtime.tool.handler({ cmd: "gh auth status" }, context("denied")),
      ]);
      expect(permitted).toMatchObject({ exit_code: 0, output: expect.stringContaining("fixture") });
      expect(denied).toMatchObject({ exit_code: 1, output: expect.stringContaining("connector_forbidden") });
      expect(seen).toHaveLength(1);
      expect(allowed.mock.calls.map((call) => call[2]?.sessionId)).toEqual(["allowed", "denied"]);
      expect(JSON.stringify([permitted, denied])).not.toMatch(/private-subject|NANOCODEX_PROVIDER_CREDENTIAL/);
      await expect(runtime.tool.handler({
        cmd: `curl -sS -H 'x-nanocodex-vault-id: ${"v".repeat(22)}' https://example.com`,
      }, context("allowed"))).resolves.toMatchObject({
        output: expect.stringContaining("cannot use Vault items"),
      });
      expect(seen).toHaveLength(1);
    } finally { runtime.dispose(); }
  });
});
