import { describe, expect, it } from "vitest";
import { renderCodexTemplate } from "../src/codex-prompts";

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
