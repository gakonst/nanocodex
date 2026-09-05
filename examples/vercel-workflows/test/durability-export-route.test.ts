import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  exportDurabilityStatePage: vi.fn(),
  postgresDurabilityStore: vi.fn(),
}));

vi.mock("nanocodex/durability", async (importOriginal) => ({
  ...await importOriginal<typeof import("nanocodex/durability")>(),
  exportDurabilityStatePage: mocks.exportDurabilityStatePage,
}));
vi.mock("@/workflows/postgres-durability", () => ({
  postgresDurabilityStore: mocks.postgresDurabilityStore,
}));

import { POST } from "../app/api/durability/export/route";

const FROM_DIGEST = `sha256:${"1".repeat(64)}`;
const ZERO_DIGEST = `sha256:${"2".repeat(64)}`;
const store = { acquire: vi.fn() };

describe("durability export route", () => {
  beforeEach(() => {
    delete process.env.NANOCODEX_ADMIN_TOKEN;
    mocks.exportDurabilityStatePage.mockReset();
    mocks.postgresDurabilityStore.mockReset();
    mocks.postgresDurabilityStore.mockReturnValue(store);
  });

  it("consumes and exposes the exact nonzero from-state lineage digest", async () => {
    const page = exportPage("7", FROM_DIGEST);
    mocks.exportDurabilityStatePage.mockResolvedValue(page);

    const response = await post({
      state_id: "portable-agent",
      from: "7",
      fromDigest: FROM_DIGEST,
      to: "9",
      cursor: "v1:12",
      limit: 97,
    });

    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    await expect(response.json()).resolves.toEqual(page);
    expect(mocks.exportDurabilityStatePage).toHaveBeenCalledWith(store, "portable-agent", {
      from: "7",
      fromDigest: FROM_DIGEST,
      to: "9",
      cursor: "v1:12",
      limit: 97,
    });
  });

  it("preserves revision-zero export requests without a supplied digest", async () => {
    const page = exportPage("0", ZERO_DIGEST);
    mocks.exportDurabilityStatePage.mockResolvedValue(page);

    const response = await post({ state_id: "portable-agent", from: 0 });

    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toEqual(page);
    expect(mocks.exportDurabilityStatePage).toHaveBeenCalledWith(store, "portable-agent", {
      from: "0",
      to: undefined,
      cursor: undefined,
      limit: undefined,
    });
  });

  it.each([
    [{ state_id: "portable-agent", from: "7" }, "fromDigest is required"],
    [{ state_id: "portable-agent", from: "7", fromDigest: "sha256:not-a-digest" }, "fromDigest must be"],
    [{ state_id: "portable-agent", from: "nope", fromDigest: FROM_DIGEST }, "from must be"],
    [{ state_id: "portable-agent", from: "0", extra: true }, "unknown field"],
  ])("rejects invalid lineage requests before opening the store", async (body, message) => {
    const response = await post(body);

    expect(response.status).toBe(400);
    await expect(response.json()).resolves.toEqual({
      error: { code: "invalid_request", message: expect.stringContaining(message) },
    });
    expect(mocks.postgresDurabilityStore).not.toHaveBeenCalled();
    expect(mocks.exportDurabilityStatePage).not.toHaveBeenCalled();
  });
});

function post(body: unknown): Promise<Response> {
  return POST(new Request("https://example.test/api/durability/export", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  }));
}

function exportPage(from: string, fromDigest: string) {
  return {
    format: "nanocodex-durability-state-page-v1",
    stateId: "portable-agent",
    from,
    fromDigest,
    to: "9",
    cursor: "v1:0",
    nextCursor: null,
    payloadLength: 6,
    payload: "opaque",
  } as const;
}
