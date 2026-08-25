import assert from "node:assert/strict";
import test from "node:test";

import { clientFailureMessage, isClientNetworkFailure } from "../src/clientFailure.ts";

test("browser transport wording never escapes into the interface", () => {
  const fallback = "The connection was interrupted. Try again.";
  for (const message of [
    "Load failed",
    "Failed to fetch",
    "Network request failed",
    "NetworkError when attempting to fetch resource.",
    "Failed to fetch dynamically imported module: /assets/managed.js",
    "Agent connection timed out",
    "Agent connection rejected with HTTP 503: agent_subject_unavailable",
    "Agent connection returned an invalid handshake",
    "WebSocket connection failed",
    "WebSocket closed during connection with code 1006",
    "Responses WebSocket handshake failed",
  ]) {
    const error = new TypeError(message);
    assert.equal(isClientNetworkFailure(error), true);
    assert.equal(clientFailureMessage(error, fallback), fallback);
  }
  assert.equal(clientFailureMessage(new Error("agent quota exceeded"), fallback), "agent quota exceeded");
});
