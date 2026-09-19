import { describe, expect, it } from "vitest";
import { codexModelMessage, codexPrompt, renderCodexPrompt, renderCodexTemplate } from "../src/codex-prompts";
import { goalContinuation, type ThreadGoal } from "../src/goals";

describe("canonical prompt rendering", () => {
  it("retains exact model text and selects sparse catalog overrides", () => {
    expect(codexPrompt("models/gpt-6-astra.md")).toContain("You are Codex, an agent based on GPT-6.");
    expect(codexModelMessage("gpt-6-astra", "persistent_instructions", "models/gpt-6-astra.md")).toMatch(/^## Overview\n/);
    expect(codexModelMessage("missing", "persistent_instructions", "models/gpt-6-astra.md")).toBe(codexPrompt("models/gpt-6-astra.md"));
    expect(() => codexPrompt("toString")).toThrow("unknown canonical prompt");
  });

  it("renders adjacent and repeated values, escaped delimiters, and Unicode literally", () => {
    expect(renderCodexTemplate("{{{{ {{a}}{{ b }} {{a}} }}}}", { a: "😀", b: "{{ never_render }}" }))
      .toBe("{{ 😀{{ never_render }} 😀 }}");
    // Rust str::trim uses Unicode White_Space, unlike JavaScript trim (NEL/FEFF).
    expect(renderCodexTemplate("{{\u0085name\u0085}}", { name: "value" })).toBe("value");
    expect(renderCodexTemplate("{{\ufeffname\ufeff}}", { "\ufeffname\ufeff": "value" })).toBe("value");
  });

  it.each(["{{ }}", "{{ missing", "{{ outer {{ inner }} }}", "unmatched }}"])("rejects malformed source %s", source => {
    expect(() => renderCodexTemplate(source, {})).toThrow();
  });

  it("rejects missing, inherited, and extra template values", () => {
    expect(() => renderCodexTemplate("{{ name }}", {})).toThrow("missing");
    expect(() => renderCodexTemplate("{{ toString }}", {})).toThrow("missing");
    expect(() => renderCodexTemplate("{{ name }}", { name: "value", extra: "value" })).toThrow("extra");
    expect(renderCodexPrompt("prompts/templates/review/exit_success.xml", { results: "{{ literal }} <x>" })).toContain("{{ literal }} <x>");
  });
});

describe("canonical goal continuation", () => {
  const goal: ThreadGoal = {
    goalId: "synthetic-goal", threadId: "synthetic-thread", objective: "Ship <x> & {{ objective }}", status: "active",
    tokensUsed: 12, timeUsedSeconds: 3, createdAt: 1000, updatedAt: 1000,
  };
  it("renders host accounting and escaped objectives without recursive interpolation", () => {
    const text = goalContinuation({ ...goal, tokenBudget: 20 });
    expect(text).toContain("Ship &lt;x&gt; &amp; {{ objective }}");
    expect(text).toContain("Tokens used: 12");
    expect(text).toContain("Token budget: 20");
    expect(text).toContain("Tokens remaining: 8");
  });
  it("uses upstream unbudgeted values and suppresses inactive goals", () => {
    expect(goalContinuation(goal)).toContain("Token budget: none");
    expect(goalContinuation(goal)).toContain("Tokens remaining: unbounded");
    expect(goalContinuation({ ...goal, tokenBudget: 5 })).toContain("Tokens remaining: 0");
    expect(goalContinuation({ ...goal, status: "paused" })).toBeNull();
    expect(goalContinuation(null)).toBeNull();
  });
});
