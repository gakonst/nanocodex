import assert from "node:assert/strict";
import test from "node:test";
import { parseHostedToolsHostFrame, parseHostedToolsManagedFrame } from "../dist/hosted/index.js";

const call = {
  type: "call", session_id: "session:1", call_id: "call:1", model: "gpt-6-astra",
  name: "lookup", input: {}, output_token_budget: 100, output_byte_budget: 1024,
  deadline_at: 1,
};

test("model routing metadata survives parsing without relaxing identities", () => {
  for (const model of ["@cf/zai-org/glm-5.3", "kimi-k3", "mimo-v2.6-pro", "auto", "openrouter/auto", "gpt-6-astra", "openai/gpt-6-astra", "@openai/gpt-6-astra", "auto(openai/gpt-6-astra,anthropic/claude)", "", "x y", "x\n", "x\t", "x\0", "x\x7f", "模型🦙", "x".repeat(8192)]) {
    assert.deepEqual(parseHostedToolsManagedFrame(JSON.stringify({ ...call, model })), { ...call, model });
  }
  for (const model of [undefined, null, 1, {}, [], true]) {
    assert.throws(() => parseHostedToolsManagedFrame(JSON.stringify({ ...call, model })), /model must/);
  }
  for (const field of ["session_id", "call_id", "name"]) {
    assert.throws(() => parseHostedToolsManagedFrame(JSON.stringify({ ...call, [field]: "provider/name" })), /safe ASCII/);
  }
});

test("optional local receipt timing preserves old Hosts and rejects unbounded or extra telemetry", () => {
  const receipt = { type: "result", call_id: "call:1", outcome: { status: "unavailable", message: "fixture" } };
  assert.deepEqual(parseHostedToolsHostFrame(JSON.stringify(receipt)), receipt);
  const timing = { scheduler_ms: 1, execution_gate_ms: 2, execution_ms: 3,
    result_encode_ms: 4, result_queue_ms: 5, host_elapsed_ms: 15 };
  assert.deepEqual(parseHostedToolsHostFrame(JSON.stringify({ ...receipt, timing })), { ...receipt, timing });
  for (const invalid of [null, {}, { ...timing, path: "/private" }, { ...timing, execution_ms: -1 },
    { ...timing, execution_ms: "3" }, { ...timing, execution_ms: Number.MAX_SAFE_INTEGER + 1 },
    { ...timing, host_elapsed_ms: 14 }, { ...timing, execution_ms: null }]) {
    assert.throws(() => parseHostedToolsHostFrame(JSON.stringify({ ...receipt, timing: invalid })));
  }
  assert.throws(() => parseHostedToolsManagedFrame(JSON.stringify({ ...call, thread_id: "thread:1" })), /unsupported fields/);
});

test("diagnostics require strict advertised fields while legacy catalogs remain unchanged", () => {
  const legacy = { type: "catalog", capabilities: ["turn_metadata"], tools: [] };
  assert.deepEqual(parseHostedToolsHostFrame(JSON.stringify(legacy)), legacy);
  const connection_id = "00000000-0000-4000-8000-000000000007";
  for (const optional of [{ diagnostics: true }, { connection_id }, { diagnostics: true, connection_id }]) {
    assert.deepEqual(parseHostedToolsHostFrame(JSON.stringify({ ...legacy, ...optional })), { ...legacy, ...optional });
  }
  for (const optional of [{ diagnostics: false }, { diagnostics: null }, { diagnostics: "true" },
    { diagnostics: {} }, { connection_id: "host/path" }, { connection_id: null },
    { connection_id: "00000000-0000-1000-8000-000000000007" },
    { connection_id: "00000000-0000-4000-7000-000000000007" }, { diagnostics: true, private_path: "/fixture" }]) {
    assert.throws(() => parseHostedToolsHostFrame(JSON.stringify({ ...legacy, ...optional })));
  }
  for (const stage of ["received", "execution_started", "execution_finished", "result_prepared"]) {
    for (const elapsed_ms of [0, 0.25, Number.MAX_SAFE_INTEGER]) {
      const diagnostic = { type: "diagnostic", call_id: "call:1", stage, elapsed_ms };
      assert.deepEqual(parseHostedToolsHostFrame(JSON.stringify(diagnostic)), diagnostic);
      assert.throws(() => parseHostedToolsManagedFrame(JSON.stringify(diagnostic)), /host-to-managed|managed-to-host/);
    }
  }
  const diagnostic = { type: "diagnostic", call_id: "call:1", stage: "received", elapsed_ms: 0 };
  for (const fields of [{ elapsed_ms: -1 }, { elapsed_ms: Number.MAX_SAFE_INTEGER + 1 }, { elapsed_ms: null },
    { elapsed_ms: "0" }, { stage: "unknown" }, { call_id: "private/path" }, { runtime_id: "fixture" }]) {
    assert.throws(() => parseHostedToolsHostFrame(JSON.stringify({ ...diagnostic, ...fields })));
  }
  for (const field of ["call_id", "stage", "elapsed_ms"]) {
    const missing = { ...diagnostic };
    delete missing[field];
    assert.throws(() => parseHostedToolsHostFrame(JSON.stringify(missing)));
  }
});


test("command journal recovery is explicit, bounded per frame, and directional", () => {
  const catalog = { type: "catalog", capabilities: ["turn_metadata"], tools: [], runtime_id: "runtime:1", command_recovery: true };
  assert.deepEqual(parseHostedToolsHostFrame(JSON.stringify(catalog)), catalog);
  for (const fields of [{ runtime_id: undefined }, { command_recovery: false }, { command_recovery: null }]) {
    assert.throws(() => parseHostedToolsHostFrame(JSON.stringify({ ...catalog, ...fields })));
  }
  const recover = { type: "recover", call_ids: Array.from({ length: 100 }, (_, index) => `call:${index}`) };
  assert.deepEqual(parseHostedToolsManagedFrame(JSON.stringify(recover)), recover);
  for (const call_ids of [[], [...recover.call_ids, "extra"], ["duplicate", "duplicate"], ["private/path"], null]) {
    assert.throws(() => parseHostedToolsManagedFrame(JSON.stringify({ ...recover, call_ids })));
  }
  assert.throws(() => parseHostedToolsHostFrame(JSON.stringify(recover)), /not a host-to-managed/);
  for (const state of ["running", "missing"]) {
    const status = { type: "status", call_id: "call:1", state };
    assert.deepEqual(parseHostedToolsHostFrame(JSON.stringify(status)), status);
    assert.throws(() => parseHostedToolsManagedFrame(JSON.stringify(status)), /not a managed-to-host/);
  }
  assert.throws(() => parseHostedToolsHostFrame(JSON.stringify({ type: "status", call_id: "call:1", state: "completed" })));
});

test("trusted turn lifecycle is opt-in and never accepts provider arguments", () => {
  const catalog = { type: "catalog", capabilities: ["turn_metadata"], tools: [], turn_lifecycle: true };
  assert.deepEqual(parseHostedToolsHostFrame(JSON.stringify(catalog)), catalog);
  for (const value of [false, null, "true", {}]) {
    assert.throws(() => parseHostedToolsHostFrame(JSON.stringify({ ...catalog, turn_lifecycle: value })));
  }
  const ended = { type: "turn_ended", session_id: "session:one", turn_id: "turn:cancelled", hook_event_name: "Interrupt" };
  assert.deepEqual(parseHostedToolsManagedFrame(JSON.stringify(ended)), ended);
  assert.throws(() => parseHostedToolsHostFrame(JSON.stringify(ended)), /managed-to-host|host-to-managed/);
  for (const extra of [{ hook_event_name: "invalid" }, { hook_event_name: null }, { input: {} }, { turn_id: "" }, { turn_id: null }, { session_id: "other/session" }]) {
    assert.throws(() => parseHostedToolsManagedFrame(JSON.stringify({ ...ended, ...extra })));
  }
});
