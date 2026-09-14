import { describe, expect, it } from "vitest";
import { validRealtimeSession } from "../src/managed-realtime-transport";

const session = () => ({
  model: "gpt-live-1-codex", instructions: "Use the user's ChatGPT subscription.",
  audio: { output: { voice: "maple" } }, delegation: { type: "client" },
});

describe("ChatGPT subscription voice call boundary", () => {
  it("accepts the Codex acknowledgement option and preserves provider defaults", () => {
    expect(validRealtimeSession(session())).toBe(true);
    for (const ack_filler of [true, false]) {
      expect(validRealtimeSession({ ...session(), delegation: { type: "client", ack_filler } })).toBe(true);
    }
  });
  it("accepts full instructions beyond the former 32 KiB cutoff", () => {
    expect(validRealtimeSession({ ...session(), instructions: "x".repeat(96 * 1024) })).toBe(true);
  });
  it("rejects malformed acknowledgements and arbitrary provider fields", () => {
    for (const ack_filler of [null, "false", 0, {}, []]) {
      expect(validRealtimeSession({ ...session(), delegation: { type: "client", ack_filler } })).toBe(false);
    }
    expect(validRealtimeSession({ ...session(), delegation: { type: "server", ack_filler: true } })).toBe(false);
    expect(validRealtimeSession({ ...session(), delegation: { type: "client", ack_filler: true, instructions: "override" } })).toBe(false);
  });
  it("rejects Platform audio options, custom voices, and other models", () => {
    expect(validRealtimeSession({ ...session(), audio: { input: { turn_detection: { type: "semantic_vad" } }, output: { voice: "maple" } } })).toBe(false);
    expect(validRealtimeSession({ ...session(), audio: { output: { voice: "maple", speed: 1.5 } } })).toBe(false);
    for (const voice of ["alloy", "voice_custom", { id: "voice_custom" }]) {
      expect(validRealtimeSession({ ...session(), audio: { output: { voice } } })).toBe(false);
    }
    expect(validRealtimeSession({ ...session(), model: "gpt-realtime" })).toBe(false);
  });
});
