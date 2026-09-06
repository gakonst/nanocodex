import { createExecutionContext, env, runInDurableObject } from "cloudflare:test";
import { describe, expect, it, vi } from "vitest";
import { createBrainBucket } from "../src/brain-bucket";
import { createBrainWorkspace } from "../src/brain-workspace";
import { ContainerProxy, serveBrainFilesystem } from "../src/sandbox-runtime";
import type { DurableAgentSession } from "../src/index";

const bindings = env as unknown as {
  NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession>;
  NANOCODEX_WORKSPACES: R2Bucket;
};

describe("brain storage local to its agent", () => {
  it("persists an archive-sized file tree without a remote write per entry and reopens it", async () => {
    const id = crypto.randomUUID();
    await runInDurableObject(bindings.NANOCODEX_SESSIONS.getByName(id), async (_instance, state) => {
      const backing = bindings.NANOCODEX_WORKSPACES;
      const put = vi.fn(backing.put.bind(backing));
      const head = vi.fn(backing.head.bind(backing));
      const list = vi.fn(backing.list.bind(backing));
      const bucket = createBrainBucket(state.storage, { ...backing, put, head, list } as unknown as R2Bucket, id);
      const workspace = createBrainWorkspace(bucket, id);
      await workspace.list(".", { recursive: true });
      for (let dir = 0; dir < 100; dir++) {
        await workspace.mkdir(`repo/dir-${dir}`);
        await Promise.all(Array.from({ length: 20 }, (_, file) =>
          workspace.writeFile(`repo/dir-${dir}/file-${file}`, `contents-${dir}-${file}`)));
      }
      expect(put).not.toHaveBeenCalled();
      expect(head).not.toHaveBeenCalled();
      expect(list).toHaveBeenCalledTimes(1);
      const reopened = createBrainWorkspace(createBrainBucket(state.storage, backing, id), id);
      expect(await reopened.list(".", { recursive: true })).toHaveLength(2101);
      expect(new TextDecoder().decode(await reopened.readFile("repo/dir-99/file-19"))).toBe("contents-99-19");
      const first = await bucket.list({ prefix: `brains/${id}/repo/dir-`, delimiter: "/", limit: 2 });
      expect(first.delimitedPrefixes).toHaveLength(2);
      expect(first.truncated).toBe(true);
      const second = await bucket.list({ prefix: `brains/${id}/repo/dir-`, delimiter: "/", limit: 2,
        cursor: first.truncated ? first.cursor : undefined });
      expect(second.delimitedPrefixes).toHaveLength(2);
      expect(new Set([...first.delimitedPrefixes, ...second.delimitedPrefixes]).size).toBe(4);
    });
  });

  it("serves host files through the native S3 protocol and retains native changes", async () => {
    const id = crypto.randomUUID();
    await runInDurableObject(bindings.NANOCODEX_SESSIONS.getByName(id), async (_instance, state) => {
      const bucket = createBrainBucket(state.storage, bindings.NANOCODEX_WORKSPACES, id);
      const workspace = createBrainWorkspace(bucket, id);
      await workspace.writeFile("repo/file", "host bytes");
      const request = (path: string, init?: RequestInit, readOnly = false) => serveBrainFilesystem(
        new Request(`http://r2.internal/NANOCODEX_BRAIN/${path}`, init), bucket, id, readOnly);
      const get = await request("repo/file");
      expect(get.status).toBe(200);
      expect(await get.text()).toBe("host bytes");
      const range = await request("repo/file", { headers: { range: "bytes=1-3" } });
      expect(range.status).toBe(206);
      expect(await range.text()).toBe("ost");
      expect((await request("repo/file", { method: "HEAD" })).headers.get("Content-Length")).toBe("10");
      const listed = await request("?list-type=2&prefix=repo/&delimiter=/");
      expect(await listed.text()).toContain("<Key>repo/file</Key>");
      expect((await request("repo/native", { method: "PUT", body: "native bytes", headers: {
        "Content-Length": "12", "x-amz-meta-mode": "33261",
      } })).status).toBe(200);
      expect(new TextDecoder().decode(await workspace.readFile("repo/native"))).toBe("native bytes");
      expect((await request("repo/native", { method: "HEAD" })).headers.get("x-amz-meta-mode")).toBe("33261");
      expect((await request("repo/copied", { method: "PUT", headers: {
        "x-amz-copy-source": "/NANOCODEX_BRAIN/repo/file",
        "x-amz-metadata-directive": "REPLACE", "x-amz-meta-mode": "33261",
      } })).status).toBe(200);
      expect((await request("repo/copied", { method: "HEAD" })).headers.get("x-amz-meta-mode")).toBe("33261");
      expect(await (await request("repo/copied")).text()).toBe("host bytes");
      const started = await request("repo/multipart?uploads", { method: "POST", headers: { "x-amz-meta-mode": "33188" } });
      const uploadId = /<UploadId>(.*?)<\/UploadId>/.exec(await started.text())![1];
      const part = await request(`repo/multipart?uploadId=${encodeURIComponent(uploadId!)}&partNumber=1`, {
        method: "PUT", body: "part", headers: { "Content-Length": "4" },
      });
      expect(part.status).toBe(200);
      expect((await request(`repo/multipart?uploadId=${encodeURIComponent(uploadId!)}`, { method: "POST",
        body: `<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>${part.headers.get("ETag")}</ETag></Part></CompleteMultipartUpload>`,
      })).status).toBe(200);
      expect(await (await request("repo/multipart")).text()).toBe("part");
      expect((await request("repo/multipart", { method: "HEAD" })).headers.get("x-amz-meta-mode")).toBe("33188");
      expect((await request("repo/native", { method: "DELETE" }, true)).status).toBe(403);
      expect((await request("repo/native", { method: "DELETE" })).status).toBe(204);
      expect((await request("repo/native", { method: "HEAD" })).status).toBe(404);
      await expect(bucket.head(`brains/another-agent/secret`)).rejects.toThrow("owning agent");
    });
  });

  it("adopts legacy R2 files without copying them and keeps large files in R2", async () => {
    const id = crypto.randomUUID();
    const backing = bindings.NANOCODEX_WORKSPACES;
    await backing.put(`brains/${id}/legacy`, "legacy bytes", { customMetadata: { mode: "33261" } });
    await runInDurableObject(bindings.NANOCODEX_SESSIONS.getByName(id), async (_instance, state) => {
      const bucket = createBrainBucket(state.storage, backing, id);
      expect(await (await bucket.get(`brains/${id}/legacy`))!.text()).toBe("legacy bytes");
      expect((await bucket.head(`brains/${id}/legacy`))!.customMetadata?.mode).toBe("33261");
      const large = new Uint8Array(2 * 1024 * 1024 + 17).fill(73);
      await bucket.put(`brains/${id}/large`, large);
      expect((await backing.head(`brains/${id}/large`))!.size).toBe(large.length);
      const actual = await (await bucket.get(`brains/${id}/large`))!.arrayBuffer();
      expect(actual.byteLength).toBe(large.length);
      const hash = async (bytes: ArrayBuffer | Uint8Array) => Array.from(new Uint8Array(
        await crypto.subtle.digest("SHA-256", bytes),
      )).join(",");
      expect(await hash(actual)).toBe(await hash(large));
      await bucket.put(`brains/${id}/legacy`, "replacement");
      expect(await (await bucket.get(`brains/${id}/legacy`))!.text()).toBe("replacement");
      await bucket.delete(`brains/${id}/legacy`);
      expect(await backing.head(`brains/${id}/legacy`)).toBeNull();
      expect(await createBrainBucket(state.storage, backing, id).head(`brains/${id}/legacy`)).toBeNull();
    });
  });

  it("routes the trusted native brain mount to its owning actor and rejects forged bindings", async () => {
    const id = crypto.randomUUID();
    await runInDurableObject(bindings.NANOCODEX_SESSIONS.getByName(id), async (_instance, state) => {
      state.storage.sql.exec(`INSERT INTO session_state (
        singleton, session_id, owner_id, organization_id, team_id, authorization_epoch,
        public_origin, runtime_profile, completed_turns, last_active
      ) VALUES (1, ?, 'owner', 'org', 'team', 1, 'https://example.test', 'managed', 0, ?)`, id, Date.now());
      const bucket = createBrainBucket(state.storage, bindings.NANOCODEX_WORKSPACES, id);
      await bucket.put(`brains/${id}/owned`, "owned bytes");
    });
    const context = createExecutionContext();
    Object.defineProperty(context, "props", { value: { outboundByHostOverrides: {
      "r2.internal": { method: "r2EgressMount", params: {
        buckets: { NANOCODEX_BRAIN: { prefix: `brains/${id}`, readOnly: true } },
      } },
    } } });
    const proxy = new ContainerProxy(context, { NANOCODEX_SESSIONS: bindings.NANOCODEX_SESSIONS });
    const response = await proxy.fetch(new Request("http://r2.internal/NANOCODEX_BRAIN/owned"));
    expect(response.status).toBe(200);
    expect(await response.text()).toBe("owned bytes");
    expect((await proxy.fetch(new Request("http://r2.internal/NANOCODEX_BRAIN/owned", { method: "DELETE" }))).status).toBe(403);
    const unmounted = createExecutionContext();
    Object.defineProperty(unmounted, "props", { value: {} });
    expect((await new ContainerProxy(unmounted, { NANOCODEX_SESSIONS: bindings.NANOCODEX_SESSIONS })
      .fetch(new Request("http://r2.internal/NANOCODEX_BRAIN/owned", { headers: { "x-brain-id": id } }))).status).toBe(403);
  });
});
