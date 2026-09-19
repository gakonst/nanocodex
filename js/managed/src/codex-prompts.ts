/** Port of codex-utils-template: strict variables, escaped delimiters, single-pass interpolation. */
export function renderCodexTemplate(source: string, values: Readonly<Record<string, string>>): string {
  const parts: { literal?: string; name?: string }[] = [];
  const names = new Set<string>();
  let cursor = 0;
  while (cursor < source.length) {
    if (source.startsWith("{{{{", cursor) || source.startsWith("}}}}", cursor)) {
      parts.push({ literal: source.slice(cursor, cursor + 2) }); cursor += 4; continue;
    }
    if (source.startsWith("{{", cursor)) {
      const end = source.indexOf("}}", cursor + 2);
      if (end < 0) throw new Error("unterminated placeholder");
      const name = source.slice(cursor + 2, end).replace(/^\p{White_Space}+|\p{White_Space}+$/gu, "");
      if (!name || name.includes("{{")) throw new Error("empty or nested placeholder");
      names.add(name); parts.push({ name }); cursor = end + 2; continue;
    }
    if (source.startsWith("}}", cursor)) throw new Error("unmatched closing delimiter");
    parts.push({ literal: source[cursor]! }); cursor++;
  }
  for (const name of names) if (!Object.hasOwn(values, name)) throw new Error(`missing template value: ${name}`);
  for (const name of Object.keys(values)) if (!names.has(name)) throw new Error(`extra template value: ${name}`);
  return parts.map(part => part.name === undefined ? part.literal : values[part.name]).join("");
}
