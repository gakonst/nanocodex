import { describe, expect, it } from "vitest";

import {
  DEFAULT_AGENT_SETTINGS,
  agentSettingsQuery,
  parseAgentCreateBody,
  parseAgentSettingsPatch,
  parseAgentSettingsQuery,
  parseCompleteAgentSettings,
} from "../src/agent-settings";

describe("managed agent settings", () => {
  it("uses the canonical durable defaults", () => {
    expect(parseAgentSettingsQuery(new URLSearchParams())).toEqual({
      model: "gpt-5.6-sol",
      thinking: "high",
      reasoning_mode: "standard",
      fast_mode: false,
    });
    expect(DEFAULT_AGENT_SETTINGS).toEqual(parseAgentSettingsQuery(new URLSearchParams()));
  });

  it("strictly parses and forwards every live creation setting", () => {
    const settings = parseAgentSettingsQuery(new URLSearchParams(
      "model=gpt-6-astra&thinking=max&reasoning_mode=standard&fast_mode=true",
    ));
    expect(settings).toEqual({
      model: "gpt-6-astra",
      thinking: "max",
      reasoning_mode: "standard",
      fast_mode: true,
    });
    expect(Object.fromEntries(agentSettingsQuery(settings))).toEqual({
      model: "gpt-6-astra",
      thinking: "max",
      reasoning_mode: "standard",
      fast_mode: "true",
    });
  });

  it.each([
    "model=gpt-5.6",
    "thinking=extreme",
    "reasoning_mode=professional",
    "fast_mode=1",
    "model=gpt-5.6-sol&model=gpt-5.6-luna",
    "unknown=value",
  ])("rejects a non-canonical live query: %s", (query) => {
    expect(() => parseAgentSettingsQuery(new URLSearchParams(query))).toThrow(
      "invalid agent settings query",
    );
  });

  it("accepts only a non-empty strict settings patch", () => {
    expect(parseAgentSettingsPatch({ thinking: "xhigh", fast_mode: true })).toEqual({
      thinking: "xhigh",
      fast_mode: true,
    });
    expect(parseAgentSettingsPatch({ model: "gpt-5.6-terra", reasoning_mode: "pro" }))
      .toEqual({ model: "gpt-5.6-terra", reasoning_mode: "pro" });

    for (const invalid of [
      {},
      [],
      { thinking: "HIGH" },
      { thinking: undefined },
      { fast_mode: "true" },
      { model: "gpt-5.6-sol", extra: true },
    ]) {
      expect(() => parseAgentSettingsPatch(invalid)).toThrow();
    }
  });

  it("requires an atomic complete settings object for HTTP creation", () => {
    const settings = {
      model: "gpt-6-astra",
      thinking: "medium",
      reasoning_mode: "standard",
      fast_mode: true,
    } as const;
    expect(parseCompleteAgentSettings(settings)).toEqual(settings);
    expect(parseAgentCreateBody(JSON.stringify({ settings }))).toEqual({
      settings,
      settingsProvided: true,
    });
    expect(parseAgentCreateBody(JSON.stringify({ durability: { stateId: "state" }, settings })))
      .toEqual({
        durability: { stateId: "state" },
        settings,
        settingsProvided: true,
      });
    expect(parseAgentCreateBody("")).toEqual({
      settings: DEFAULT_AGENT_SETTINGS,
      settingsProvided: false,
    });
    expect(() => parseCompleteAgentSettings({ thinking: "medium", fast_mode: true }))
      .toThrow("must contain all four fields");
    expect(() => parseCompleteAgentSettings({ ...settings, thinking: "none" }))
      .toThrow("GPT-6 Astra requires low");
    expect(() => parseCompleteAgentSettings({ ...settings, reasoning_mode: "pro" }))
      .toThrow("does not support pro");
    for (const encoded of ["{}", "[]", JSON.stringify({ settings: { ...settings, extra: true } })]) {
      expect(() => parseAgentCreateBody(encoded)).toThrow();
    }
  });
});
