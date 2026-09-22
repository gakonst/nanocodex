import { describe, expect, it } from "vitest";
import { renderCodexTemplate } from "../src/codex-prompts";
import { goalContinuation, type ThreadGoal } from "../src/goals";
import { GOAL_CONTINUATION_TEMPLATE } from "../src/goal-continuation";

describe("canonical prompt rendering", () => {
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

  });
});

describe("canonical goal continuation", () => {
  it("decodes to the exact pinned upstream source bytes", async () => {
    const hash = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(GOAL_CONTINUATION_TEMPLATE));
    const hex = Array.from(new Uint8Array(hash), byte => byte.toString(16).padStart(2, "0")).join("");
    // codex-rs/ext/goal/templates/goals/continuation.md at 36430b3688.
    expect(hex).toBe("764b9c26b36013a21b687d74597e303be9bbf13776c359ec1fb185dbc0e7eac7");
  });
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
