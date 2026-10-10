import test from "node:test";
import assert from "node:assert/strict";
import { modelCapabilities } from "../node/index.mjs";
import { AGENT_MODELS, validateAgentSettings } from "../cloudflare/agent-settings.mjs";
import { Agent } from "../managed/index.mjs";

// The bundled WASM exports the shared Rust capability source; every JS table
// that admits or offers model settings must agree with it exactly.
const THINKING = ["none", "low", "medium", "high", "xhigh", "max"];
const entries = modelCapabilities();
const byTransport = (transport) => new Map(entries.filter((entry) => entry.transport === transport).map((entry) => [entry.model, entry]));
const native = byTransport("native");
const managed = byTransport("managed");
const combinations = (capability) => THINKING.flatMap((thinking) => ["standard", "pro"].flatMap((mode) => [false, true].map((fast) => ({
  thinking, mode, fast,
  supported: capability.thinking.includes(thinking) && capability.reasoningModes.includes(mode) && (!fast || capability.fastMode),
}))));

test("the canonical catalog distinguishes models and transports for both families", () => {
  assert.equal(native.size, 13);
  assert.equal(managed.size, 13);
  assert.deepEqual(native.get("claude-opus-5-5").thinking, ["low", "medium", "high", "xhigh", "max"]);
  assert.deepEqual(native.get("claude-opus-4-6").thinking, ["low", "medium", "high", "max"]);
  assert.deepEqual(native.get("claude-haiku-4-5").thinking, ["none"]);
  assert.equal(native.get("claude-opus-5-5").fastMode, true);
  assert.equal(native.get("claude-sonnet-4-6").fastMode, false);
  assert.deepEqual(native.get("gpt-6.1-sol").serviceTiers, ["standard", "fast", "ultrafast"]);
  assert.deepEqual(native.get("gpt-6-luna").serviceTiers, ["standard", "fast"]);
  assert.deepEqual(native.get("kimi-k3").reasoningModes, ["standard"]);
  assert.deepEqual(managed.get("claude-opus-5-5").thinking, ["low", "medium", "high"]);
  assert.equal(managed.get("claude-opus-5-5").fastMode, false);
  assert.deepEqual(managed.get("gpt-6.1-sol").serviceTiers, ["standard", "fast"]);
  for (const entry of entries) assert.ok(entry.thinking.includes(entry.defaultThinking), entry.model + " default must be accepted");
});

test("managed admission (validateAgentSettings) accepts exactly the managed capabilities", () => {
  for (const model of AGENT_MODELS) {
    const capability = managed.get(model);
    assert.ok(capability, model + " is missing from the canonical catalog");
    for (const { thinking, mode, fast, supported } of combinations(capability)) {
      let accepted = true;
      try { validateAgentSettings({ model, thinking, reasoning_mode: mode, fast_mode: fast }); } catch { accepted = false; }
      assert.equal(accepted, supported, model + " thinking=" + thinking + " mode=" + mode + " fast=" + fast);
    }
  }
});

test("the managed client rejects exactly the unsupported settings before any request", async () => {
  for (const model of AGENT_MODELS) {
    for (const { thinking, mode, fast, supported } of combinations(managed.get(model))) {
      let requests = 0;
      const created = Agent.create({ baseUrl: "https://managed.example", fetch: async () => { requests += 1; return Response.json({ agent_id: "0198d3f0-8844-7000-8000-000000000001" }); },
        settings: { model, thinking, reasoningMode: mode, fastMode: fast } });
      const label = model + " thinking=" + thinking + " mode=" + mode + " fast=" + fast;
      if (supported) { await created; assert.equal(requests, 1, label); }
      else { await assert.rejects(created, TypeError, label); assert.equal(requests, 0, label); }
    }
  }
});
