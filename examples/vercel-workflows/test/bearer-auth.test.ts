import { describe, expect, it } from "vitest";

import { hasBearerToken } from "../lib/bearer-auth";

describe("bearer authorization", () => {
  it("accepts only an exact bearer token", () => {
    const request = new Request("https://example.test", {
      headers: { authorization: "Bearer expected-secret" },
    });
    expect(hasBearerToken(request, "expected-secret")).toBe(true);
  });

  it.each([
    undefined,
    "expected-secret",
    "Basic expected-secret",
    "Bearer wrong-secret",
    "Bearer expected-secret-extra",
  ])("rejects a missing or mismatched header: %s", (authorization) => {
    const request = new Request("https://example.test", {
      headers: authorization ? { authorization } : {},
    });
    expect(hasBearerToken(request, "expected-secret")).toBe(false);
  });
});
