import assert from "node:assert/strict";
import { test } from "node:test";

import { Actions } from "../index.mjs";
import * as Host from "../host/index.mjs";
import { functionCallOutputCapability } from "../host/internal-Agent.mjs";
import { createAgentClient, defineRuntime } from "../internal.mjs";

async function makeAgent(raw) {
  return createAgentClient(defineRuntime({
    key: "late-output-test",
    reserveSessions: false,
    create: async () => raw,
  }), {});
}

test("only a host-issued call-bound capability forwards typed output and its stable operation ID", async () => {
  const calls = [];
  const agent = await makeAgent({
    agentId: "agent", sessionId: "session",
    prompt() { throw new Error("not a prompt"); },
    async submitFunctionCallOutput(...args) {
      calls.push(args);
      return JSON.stringify({ callId: args[0], operationId: args[2], status: "accepted" });
    },
    free() {},
  });
  const enabled = agent.extend(Actions.agentActions());
  assert.equal(enabled.turn.submitFunctionCallOutput, undefined);
  assert.equal(Actions.turn.submitFunctionCallOutput, undefined);
  assert.equal(Host.Agent.functionCallOutputCapability, undefined);
  assert.equal(Host.Agent.submitFunctionCallOutput, undefined);
  await assert.rejects(import("nanocodex/host/internal-Agent"), { code: "ERR_PACKAGE_PATH_NOT_EXPORTED" });
  const capability = functionCallOutputCapability(agent, "call-original");
  assert.ok(Object.isFrozen(capability));
  const receipt = await capability.submit({
    output: [{ type: "input_text", text: "job done" }],
    operationId: "job-immutable",
  });
  assert.deepEqual(calls, [["call-original", '[{"type":"input_text","text":"job done"}]', "job-immutable"]]);
  assert.deepEqual(receipt, { callId: "call-original", operationId: "job-immutable", status: "accepted" });
  assert.ok(Object.isFrozen(receipt));
  agent.dispose();
});

test("capability rejects forged call IDs, malformed content and unsupported runtimes without a prompt", async () => {
  let prompts = 0;
  const agent = await makeAgent({
    agentId: "agent", sessionId: "session",
    prompt() { prompts++; }, free() {},
  });
  assert.throws(() => functionCallOutputCapability(agent, ""), /callId/);
  const send = functionCallOutputCapability(agent, "call-1").submit;
  await assert.rejects(send(null), /options must be an object/);
  await assert.rejects(send({ output: "done" }), /operationId/);
  await assert.rejects(send({ output: "done", operationId: 12 }), /operationId/);
  await assert.rejects(send({ output: "done", operationId: "job-1", role: "tool" }), /unknown fields/);
  await assert.rejects(send({ output: [] , operationId: "job-1" }), /non-empty typed content/);
  await assert.rejects(send({ output: [{ type: "input_audio", audio_url: 1 }], operationId: "job-1" }), /invalid function-call output content/);
  await assert.rejects(send({ output: [{ type: "input_image", image_url: "data:image/png;base64,Zg==", detail: "ultra" }], operationId: "job-1" }), /invalid function-call output content/);
  await assert.rejects(send({ output: { type: "function_call_output", call_id: "forged", output: "done" }, operationId: "job-1" }), /typed content/);
  await assert.rejects(send({ output: [{ type: "input_text", text: "done", role: "user" }], operationId: "job-1" }), /invalid function-call output content/);
  await assert.rejects(send({ output: "done", operationId: "job-1", callId: "forged" }), /callId cannot be supplied/);
  await assert.rejects(send({ output: "done", operationId: "job-1" }), /does not support late/);
  assert.equal(prompts, 0);
  agent.dispose();
});

test("typed text, image and audio content preserve the original call ID without a prompt", async () => {
  const calls = [];
  const agent = await makeAgent({
    agentId: "agent", sessionId: "session",
    prompt() { throw new Error("never prompt"); },
    async submitFunctionCallOutput(...args) {
      calls.push(args);
      return JSON.stringify({ status: "accepted" });
    },
    free() {},
  });
  const submit = functionCallOutputCapability(agent, "call-original").submit;
  await submit({ output: "plain text", operationId: "job-1" });
  await submit({ output: [
    { type: "input_text", text: "ok" },
    { type: "input_image", image_url: "data:image/png;base64,Zg==", detail: "low" },
    { type: "input_audio", audio_url: "data:audio/wav;base64,Zg==" },
  ], operationId: "job-2" });
  assert.deepEqual(calls, [
    ["call-original", '"plain text"', "job-1"],
    ["call-original", JSON.stringify([
      { type: "input_text", text: "ok" },
      { type: "input_image", image_url: "data:image/png;base64,Zg==", detail: "low" },
      { type: "input_audio", audio_url: "data:audio/wav;base64,Zg==" },
    ]), "job-2"],
  ]);
  agent.dispose();
});

test("malformed native acknowledgement fails closed", async () => {
  let prompts = 0;
  let response = "[]";
  const agent = await makeAgent({
    agentId: "agent", sessionId: "session",
    prompt() { prompts++; },
    async submitFunctionCallOutput() { return response; }, free() {},
  });
  const submit = functionCallOutputCapability(agent, "call-original").submit;
  await assert.rejects(submit({ output: "done", operationId: "job-1" }), /invalid function-call output receipt/);
  response = "not-json";
  await assert.rejects(submit({ output: "done", operationId: "job-2" }), SyntaxError);
  response = 12;
  await assert.rejects(submit({ output: "done", operationId: "job-3" }), /invalid function-call output receipt/);
  assert.equal(prompts, 0);
  agent.dispose();
});

test("Rust rejection is propagated without retrying as a user prompt", async () => {
  let prompts = 0;
  let nativeCalls = 0;
  const agent = await makeAgent({
    agentId: "agent", sessionId: "session",
    prompt() { prompts++; },
    async submitFunctionCallOutput() {
      nativeCalls++;
      throw new Error("unknown or completed call ID");
    },
    free() {},
  });
  await assert.rejects(
    functionCallOutputCapability(agent, "forged-call").submit({ output: "done", operationId: "job-1" }),
    /unknown or completed call ID/,
  );
  assert.equal(nativeCalls, 1);
  assert.equal(prompts, 0);
  agent.dispose();
});

test("host-only active status reads the exact durable turn/job/call receipt without a prompt", async () => {
  const calls = [];
  const agent = await makeAgent({
    agentId: "agent", sessionId: "session",
    prompt() { throw new Error("not a prompt"); },
    async activeFunctionOutputStatus(...args) {
      calls.push(args);
      return JSON.stringify({ state: "confirmed", model_call_index: 2, response_id: "resp-2" });
    },
    free() {},
  });
  const capability = functionCallOutputCapability(agent, "original-call");
  assert.equal(agent.extend(Actions.agentActions()).turn.activeStatus, undefined);
  const status = await capability.activeStatus({ originalTurnId: "source-turn", operationId: "job-immutable" });
  assert.deepEqual(calls, [["source-turn", "job-immutable", "original-call"]]);
  assert.deepEqual(status, { state: "confirmed", model_call_index: 2, response_id: "resp-2" });
  assert.ok(Object.isFrozen(status));
  await assert.rejects(capability.activeStatus({ originalTurnId: "", operationId: "job" }), /originalTurnId/);
  await assert.rejects(capability.activeStatus({ originalTurnId: "source-turn", operationId: "job", callId: "forged" }), /requires/);
  agent.dispose();
});

test("active status rejects missing or false model-uptake receipts", async () => {
  const agent = await makeAgent({ agentId: "agent", sessionId: "session", free() {}, prompt() {},
    async activeFunctionOutputStatus() { return JSON.stringify({ state: "confirmed" }); },
  });
  await assert.rejects(functionCallOutputCapability(agent, "call").activeStatus({
    originalTurnId: "turn", operationId: "job",
  }), /invalid active output status/);
  agent.dispose();
  const older = await makeAgent({ agentId: "agent", sessionId: "session", free() {}, prompt() {} });
  await assert.rejects(functionCallOutputCapability(older, "call").activeStatus({
    originalTurnId: "turn", operationId: "job",
  }), /does not support active/);
  older.dispose();
});


test("host-only idle status requires the exact job/call and a completed wake model step", async () => {
  const calls = [];
  const agent = await makeAgent({ agentId: "agent", sessionId: "session", free() {},
    prompt() { throw new Error("not a prompt"); },
    async idleFunctionOutputStatus(...args) {
      calls.push(args);
      return JSON.stringify({ state: "confirmed", model_call_index: 1, response_id: "wake-resp" });
    },
  });
  const capability = functionCallOutputCapability(agent, "call-original");
  assert.equal(agent.extend(Actions.agentActions()).turn.idleStatus, undefined);
  assert.deepEqual(await capability.idleStatus({ operationId: "job-immutable" }),
    { state: "confirmed", model_call_index: 1, response_id: "wake-resp" });
  assert.deepEqual(calls, [["job-immutable", "call-original"]]);
  await assert.rejects(capability.idleStatus({ operationId: "", callId: "forged" }), /requires/);
  agent.dispose();
  const falseReceipt = await makeAgent({ agentId: "agent", sessionId: "session", free() {}, prompt() {},
    async idleFunctionOutputStatus() { return JSON.stringify({ state: "confirmed" }); },
  });
  await assert.rejects(functionCallOutputCapability(falseReceipt, "call").idleStatus({ operationId: "job" }),
    /invalid idle output status/);
  falseReceipt.dispose();
  const older = await makeAgent({ agentId: "agent", sessionId: "session", free() {}, prompt() {} });
  await assert.rejects(functionCallOutputCapability(older, "call").idleStatus({ operationId: "job" }),
    /does not support idle/);
  older.dispose();
});
