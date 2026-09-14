import { createExecutionContext, env } from "cloudflare:test";
import { describe, expect, it } from "vitest";

import { ContainerProxy } from "../src/sandbox-runtime";

const bucket = (env as unknown as { NANOCODEX_WORKSPACES: R2Bucket }).NANOCODEX_WORKSPACES;

function mount(readOnly = false) {
  const prefix = `sessions/${crypto.randomUUID()}`;
  const context = createExecutionContext();
  Object.defineProperty(context, "props", { value: {
    outboundByHostOverrides: {
      "r2.internal": {
        method: "r2EgressMount",
        params: { buckets: { NANOCODEX_WORKSPACES: { prefix, readOnly } } },
      },
    },
  } });
  const proxy = new ContainerProxy(context, { NANOCODEX_WORKSPACES: bucket });
  return {
    prefix,
    request: (key: string, init?: RequestInit) => proxy.fetch(new Request(
      `http://r2.internal/NANOCODEX_WORKSPACES/${key}`, init,
    )),
  };
}

describe("Sandbox SDK R2 filesystem metadata", () => {
  it("retains directory type, permissions and times across uncached HEAD/GET", async () => {
    const { prefix, request } = mount();
    expect((await request("repo/", { method: "PUT", headers: {
      "Content-Length": "0",
      "Content-Type": "application/x-directory",
      "x-amz-meta-mode": "16877",
      "x-amz-meta-mtime": "1788724000",
      "x-amz-meta-uid": "1000",
      "x-amz-meta-gid": "1000",
    } })).status).toBe(200);
    expect((await bucket.head(`${prefix}/repo/`))?.customMetadata).toEqual({
      mode: "16877", mtime: "1788724000", uid: "1000", gid: "1000",
    });
    for (const method of ["HEAD", "GET"]) {
      const response = await request("repo/", { method });
      expect(response.status).toBe(200);
      expect(response.headers.get("Content-Type")).toBe("application/x-directory");
      expect(response.headers.get("x-amz-meta-mode")).toBe("16877");
      expect(response.headers.get("x-amz-meta-mtime")).toBe("1788724000");
      await response.arrayBuffer();
    }
  });

  it("applies chmod metadata replacement and preserves metadata on ordinary copy", async () => {
    const { request } = mount();
    await request("script", { method: "PUT", body: "echo ok\n", headers: {
      "Content-Length": "8", "Content-Type": "text/plain", "x-amz-meta-mode": "33188",
    } });
    expect((await request("script", { method: "PUT", headers: {
      "x-amz-copy-source": "/NANOCODEX_WORKSPACES/script",
      "x-amz-metadata-directive": "REPLACE",
      "Content-Type": "text/plain", "x-amz-meta-mode": "33261",
    } })).status).toBe(200);
    await request("copied", { method: "PUT", headers: {
      "x-amz-copy-source": "/NANOCODEX_WORKSPACES/script",
    } });
    const response = await request("copied");
    expect(response.headers.get("x-amz-meta-mode")).toBe("33261");
    expect(await response.text()).toBe("echo ok\n");
  });

  it("retains metadata when a file is uploaded in parts", async () => {
    const { request } = mount();
    const created = await request("large?uploads", { method: "POST", headers: {
      "x-amz-meta-mode": "33188", "x-amz-meta-mtime": "1788724001",
    } });
    const uploadId = /<UploadId>(.*?)<\/UploadId>/.exec(await created.text())![1];
    const uploaded = await request(`large?uploadId=${encodeURIComponent(uploadId!)}&partNumber=1`, {
      method: "PUT", body: "part", headers: { "Content-Length": "4" },
    });
    const etag = uploaded.headers.get("ETag");
    expect((await request(`large?uploadId=${encodeURIComponent(uploadId!)}`, {
      method: "POST", body: `<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>${etag}</ETag></Part></CompleteMultipartUpload>`,
    })).status).toBe(200);
    const response = await request("large", { headers: { Range: "bytes=1-2" } });
    expect(response.status).toBe(206);
    expect(response.headers.get("x-amz-meta-mode")).toBe("33188");
    expect(response.headers.get("x-amz-meta-mtime")).toBe("1788724001");
    expect(await response.text()).toBe("ar");
  });

  it("keeps writes fenced by the mount's read-only permission", async () => {
    const { request } = mount(true);
    expect((await request("repo/", { method: "PUT", headers: {
      "Content-Length": "0", "x-amz-meta-mode": "16877",
    } })).status).toBe(403);
  });
});
