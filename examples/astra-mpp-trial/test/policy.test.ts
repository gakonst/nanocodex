import assert from "node:assert/strict";
import test from "node:test";
import {
  ASTRA_SETTINGS,
  paymentAmount,
  publicTrialState,
  reservePrompt,
  type TrialState,
} from "../src/policy.ts";

test("locks the sponsor model policy to supported Astra max settings", () => {
  assert.deepEqual(ASTRA_SETTINGS, {
    model: "gpt-6-astra",
    thinking: "max",
    reasoningMode: "standard",
    fastMode: false,
  });
  assert.equal(Object.isFrozen(ASTRA_SETTINGS), true);
});

test("charges only a wallet proof outside production", () => {
  assert.equal(paymentAmount("development"), "0");
  assert.equal(paymentAmount("LOCAL"), "0");
  assert.equal(paymentAmount("test"), "0");
  assert.equal(paymentAmount("production"), "0.1");
  assert.equal(paymentAmount(undefined), "0.1");
});

test("reserves one exact prompt and rejects every different claim", () => {
  const reserved = reservePrompt(undefined, "hash-a", "request-a", 10);
  assert.deepEqual(reserved, {
    phase: "payment_pending",
    promptHash: "hash-a",
    requestKey: "request-a",
    updatedAt: 10,
  });
  assert.notEqual(reserved, "conflict");
  if (reserved === "conflict") return;
  assert.deepEqual(reservePrompt(reserved, "hash-a", "request-a", 20), reserved);
  assert.equal(reservePrompt(reserved, "hash-b", "request-a", 20), "conflict");
  assert.equal(reservePrompt(reserved, "hash-a", "request-b", 20), "conflict");
});

test("public state never exposes sponsor managed identifiers", () => {
  const internal: TrialState = {
    agentId: "agent-secret",
    finalMessage: "hello",
    paymentReference: "0xreceipt",
    phase: "completed",
    promptHash: "hash",
    requestKey: "request",
    turnId: "turn-secret",
    updatedAt: 123,
  };
  assert.deepEqual(publicTrialState(internal), {
    final_message: "hello",
    payment_reference: "0xreceipt",
    phase: "completed",
  });
});
