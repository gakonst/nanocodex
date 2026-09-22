import { env } from "cloudflare:test";
import { expect, it } from "vitest";
import worker from "../src/index";

it("returns 404 for retired public versioned and file-memory routes", async () => {
  const token = `ncx_live_${"k".repeat(12)}_${"s".repeat(43)}`;
  const digest = btoa(String.fromCharCode(...new Uint8Array(
    await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token)),
  ))).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
  const record = {
    id: "k".repeat(12), prefix: `ncx_live_${"k".repeat(12)}`, digest,
    label: "memory", createdAt: 1, userId: crypto.randomUUID(),
    organizationId: crypto.randomUUID(), teamId: crypto.randomUUID(), role: "writer",
    authorizationEpoch: 1, capabilities: ["memory:read", "memory:write"],
  };
  const testEnv = { ...env, NANOCODEX_API_KEYS: { getByName: () => ({ resolveAuthorizedKey: async () => record }) } };
  const request = (method: string, path: string, body?: unknown) => worker.fetch(new Request(`https://test.example/v1/memory${path}`, {
    method, headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  }), testEnv as unknown as Parameters<typeof worker.fetch>[1], { waitUntil: () => {} });
  for (const [method, path] of [["GET", ""], ["POST", ""], ["DELETE", "/1?version=1"], ["GET", "?scope=personal"]]) {
    expect((await request(method!, path!, method === "POST" ? { operation: "scan", query: "retired" } : undefined)).status).toBe(404);
  }
  for (const operation of ["list", "read", "search", "add_ad_hoc_note"]) {
    const response = await worker.fetch(new Request(`https://test.example/v1/memories/${operation}`, {
      method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, body: "{}",
    }), testEnv as unknown as Parameters<typeof worker.fetch>[1], { waitUntil: () => {} });
    expect(response.status).toBe(404);
  }
});
