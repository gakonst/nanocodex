import { describe, expect, it } from "vitest";

import {
  connectorCompletion,
  connectorCompletionFor,
  isConnectorCompletion,
} from "nanocodex-connect-ui/connectorCompletion";

describe("connector OAuth completion", () => {
  it("accepts completion only from the exact origin, popup, and connector", () => {
    const popup = {};
    const completion = connectorCompletion("github", "connected");
    const event = { data: completion, origin: "https://connect.example", source: popup };
    const expected = { connector: "github", origin: "https://connect.example", source: popup };

    expect(connectorCompletionFor(event, expected)).toEqual(completion);
    expect(connectorCompletionFor({ ...event, origin: "https://evil.example" }, expected)).toBeUndefined();
    expect(connectorCompletionFor({ ...event, source: {} }, expected)).toBeUndefined();
    expect(connectorCompletionFor({ ...event, data: { ...completion, connector: "gmail" } }, expected)).toBeUndefined();
    expect(connectorCompletionFor({ ...event, data: { type: "nanocodex:connector-complete" } }, expected)).toBeUndefined();
  });

  it("maps account callback outcomes to the shared secret-free message contract", () => {
    expect(connectorCompletion("github", "connected")).toEqual({
      type: "nanocodex:connector-complete",
      connector: "github",
      result: "success",
    });
    expect(connectorCompletion("github", "cancelled")).toMatchObject({
      connector: "github",
      error: "connector_authorization_cancelled",
      result: "error",
    });
    expect(connectorCompletion("github", "failed")).toMatchObject({
      connector: "github",
      error: "connector_authorization_failed",
      result: "error",
    });
    expect(isConnectorCompletion({
      ...connectorCompletion("github", "connected"),
      token: "must-not-be-projected",
    })).toBe(false);
    expect(Object.keys(connectorCompletion("github", "connected"))).not.toContain("token");
  });
});
