import { describe, expect, it } from "vitest";

import {
  managedMountRoot,
  parseManagedMountRequest,
} from "../src/mount-tool";

describe("managed mount protocol", () => {
  it("names new VM and sandbox roots by provider and purpose with collision handling", () => {
    const id = "01234567-89ab-7def-8123-456789abcdef";
    expect(managedMountRoot("demo", id, "omarchy-desktop")).toBe("/vm-omarchy-desktop-demo");
    expect(managedMountRoot("demo", id, "linux-paradigm")).toBe("/vm-linux-paradigm-demo");
    expect(managedMountRoot("demo", id, "cf_sandbox")).toBe("/cloudflare-demo");
    expect(managedMountRoot("demo", id, "cf_sandbox", ["/cloudflare-demo"])).toBe("/cloudflare-demo-2");
    expect(managedMountRoot("x".repeat(63), id, "y".repeat(63)).length).toBeLessThanOrEqual(64);
  });

  it("rejects reserved generic hosts, unsafe names, and extra fields", () => {
    expect(() => parseManagedMountRequest({ provider: "host", name: "build" }))
      .toThrow("exact non-reserved VM factory name");
    expect(() => parseManagedMountRequest({ provider: "Build Box", name: "build" }))
      .toThrow("lowercase portable identifier");
    expect(() => parseManagedMountRequest({ provider: "cf_sandbox", name: "Build Box" }))
      .toThrow("lowercase portable identifier");
    expect(() => parseManagedMountRequest({ provider: "cf_sandbox", name: "build", region: "auto" }))
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
    expect(() => managedMountRoot(
      "Build Box",
      "01234567-89ab-7def-8123-456789abcdef",
    )).toThrow("mount name must be a lowercase portable identifier");
  });
});
