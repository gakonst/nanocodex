import assert from "node:assert/strict";
import test from "node:test";

import { homeTerminalWelcome } from "./homeTerminalWelcome.ts";

test("waits for the model session before rendering the homepage welcome", () => {
  assert.equal(homeTerminalWelcome(undefined, null), undefined);
});

test("projects account-specific homepage copy only after the model session resolves", () => {
  assertWelcomeContains(homeTerminalWelcome(null, null), /Verify your phone by SMS/);
  assertWelcomeContains(homeTerminalWelcome("sponsored", 2), /2 of 3 free Luna prompts remain/);
  assertWelcomeContains(homeTerminalWelcome("brokered", null), /connected model account/);
});

function assertWelcomeContains(welcome: string | undefined, pattern: RegExp) {
  assert.ok(welcome);
  assert.match(welcome, pattern);
}
