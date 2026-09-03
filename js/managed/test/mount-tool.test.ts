import { describe, expect, it, vi } from "vitest";

import {
  managedMountRoot,
  managedMountTool,
  parseManagedMountRequest,
  type ManagedMountRequest,
} from "../src/mount-tool";

const context = () => ({
  callId: "call",
  model: "gpt-5.6-sol",
  parentCallId: "cell",
  sessionId: "session",
  signal: new AbortController().signal,
});

describe("managed mount protocol", () => {
  it("keeps a provider-neutral strict schema and dispatches the current provider", async () => {
    const handler = vi.fn(async (request: ManagedMountRequest) => ({
      id: "mount-id",
      name: request.name,
      provider: request.provider,
      mount: "/mnt-repo-test-01234567",
      status: "mounted" as const,
      created: true,
    }));
    const tool = managedMountTool(handler);

    expect(tool.name).toBe("mount");
    expect(tool.parameters).toMatchObject({
      required: ["provider", "name"],
      additionalProperties: false,
      properties: { provider: { enum: ["cloudflare"] } },
    });
    expect(tool.outputSchema).toMatchObject({
      required: ["id", "name", "provider", "mount", "status", "created"],
      additionalProperties: false,
    });
    await expect(tool.handler(
      { provider: "cloudflare", name: "repo-test" },
      context(),
    )).resolves.toMatchObject({
      provider: "cloudflare",
      mount: "/mnt-repo-test-01234567",
      status: "mounted",
    });
    expect(handler).toHaveBeenCalledWith(
      { provider: "cloudflare", name: "repo-test" },
      expect.objectContaining({ callId: "call" }),
    );
  });

  it("rejects unknown providers, unsafe names, and extra fields", () => {
    expect(() => parseManagedMountRequest({ provider: "future", name: "build" }))
      .toThrow("provider must be cloudflare");
    expect(() => parseManagedMountRequest({ provider: "cloudflare", name: "Build Box" }))
      .toThrow("lowercase portable identifier");
    expect(() => parseManagedMountRequest({ provider: "cloudflare", name: "build", region: "auto" }))
      .toThrow("unsupported field region");
  });

  it("derives distinct portable roots without treating display names as authority", () => {
    const first = managedMountRoot("repo-test", "01234567-89ab-7def-8123-456789abcdef");
    const second = managedMountRoot("repo-test", "fedcba98-7654-7def-8123-456776543210");

    expect(first).toBe("/mnt-repo-test-89abcdef");
    expect(second).toBe("/mnt-repo-test-76543210");
    expect(first).not.toBe(second);
    expect(managedMountRoot(
      "a".repeat(63),
      "01234567-89ab-7def-8123-456789abcdef",
    ).length).toBeLessThanOrEqual(64);
  });
});
