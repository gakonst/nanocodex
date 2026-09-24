import { describe, expect, it } from "vitest";

import { requireTerminalAuthorization } from "../lib/terminal-auth";

describe("workspace terminal authorization", () => {
  it.each([
    undefined,
    "terminal-secret",
    "Bearer wrong-secret",
    "Bearer terminal-secret-extra",
  ])("rejects a malformed or incorrect authorization header: %s", (authorization) => {
    const request = new Request("https://example.test/terminal", {
      headers: authorization ? { authorization } : {},
    });
    expect(() => requireTerminalAuthorization(request, "terminal-secret")).toThrow(
      "workspace terminal token was rejected",
    );
  });
});
