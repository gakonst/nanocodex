export type MarkdownBlock =
  | { type: "heading"; depth: number; text: string; id: string }
  | { type: "paragraph"; text: string }
  | { type: "code"; language: string; code: string }
  | { type: "list"; ordered: boolean; items: string[] }
  | { type: "table"; headers: string[]; rows: string[][] };

export type ParsedDoc = {
  title: string;
  blocks: MarkdownBlock[];
};

export function parseDocument(source: string): ParsedDoc {
  const { title, body } = splitFrontmatter(source);
  const lines = body.replaceAll("\r\n", "\n").split("\n");
  const blocks: MarkdownBlock[] = [];
  const headingIds = new Map<string, number>();
  let index = 0;

  while (index < lines.length) {
    const line = lines[index];
    if (!line.trim()) {
      index += 1;
      continue;
    }

    const fence = line.match(/^```([^\s]*)\s*$/);
    if (fence) {
      const code: string[] = [];
      index += 1;
      while (index < lines.length && !lines[index].startsWith("```")) {
        code.push(lines[index]);
        index += 1;
      }
      if (index < lines.length) index += 1;
      blocks.push({ type: "code", language: fence[1], code: code.join("\n") });
      continue;
    }

    const heading = line.match(/^(#{1,3})\s+(.+)$/);
    if (heading) {
      const text = heading[2].trim();
      const baseId = slugify(stripInlineMarkdown(text));
      const duplicate = headingIds.get(baseId) ?? 0;
      headingIds.set(baseId, duplicate + 1);
      blocks.push({
        type: "heading",
        depth: heading[1].length,
        text,
        id: duplicate ? `${baseId}-${duplicate + 1}` : baseId,
      });
      index += 1;
      continue;
    }

    if (isTableStart(lines, index)) {
      const headers = tableCells(lines[index]);
      const rows: string[][] = [];
      index += 2;
      while (index < lines.length && /^\s*\|/.test(lines[index])) {
        rows.push(tableCells(lines[index]));
        index += 1;
      }
      blocks.push({ type: "table", headers, rows });
      continue;
    }

    const list = line.match(/^\s*(-|\d+\.)\s+(.+)$/);
    if (list) {
      const ordered = list[1] !== "-";
      const items: string[] = [];
      while (index < lines.length) {
        const item = lines[index].match(/^\s*(-|\d+\.)\s+(.+)$/);
        if (!item || (item[1] !== "-") !== ordered) break;
        const parts = [item[2].trim()];
        index += 1;
        while (
          index < lines.length &&
          lines[index].trim() &&
          !isBlockStart(lines, index)
        ) {
          parts.push(lines[index].trim());
          index += 1;
        }
        items.push(parts.join(" "));
      }
      blocks.push({ type: "list", ordered, items });
      continue;
    }

    const paragraph = [line.trim()];
    index += 1;
    while (
      index < lines.length &&
      lines[index].trim() &&
      !isBlockStart(lines, index)
    ) {
      paragraph.push(lines[index].trim());
      index += 1;
    }
    blocks.push({ type: "paragraph", text: paragraph.join(" ") });
  }

  return { title, blocks };
}

function splitFrontmatter(source: string) {
  if (!source.startsWith("---\n")) return { title: "Nanocodex", body: source };
  const end = source.indexOf("\n---\n", 4);
  if (end < 0) return { title: "Nanocodex", body: source };
  const frontmatter = source.slice(4, end);
  const rawTitle = frontmatter.match(/^title:\s*(.+)$/m)?.[1]?.trim();
  const title = rawTitle?.replace(/^(["'])(.*)\1$/, "$2") ?? "Nanocodex";
  return { title, body: source.slice(end + 5) };
}

function isBlockStart(lines: string[], index: number) {
  const line = lines[index];
  return (
    /^```/.test(line) ||
    /^(#{1,3})\s+/.test(line) ||
    /^\s*(-|\d+\.)\s+/.test(line) ||
    isTableStart(lines, index)
  );
}

function isTableStart(lines: string[], index: number) {
  return (
    /^\s*\|/.test(lines[index] ?? "") &&
    /^\s*\|?(?:\s*:?-{3,}:?\s*\|)+\s*$/.test(lines[index + 1] ?? "")
  );
}

function tableCells(line: string) {
  return line
    .trim()
    .replace(/^\||\|$/g, "")
    .split("|")
    .map((cell) => cell.trim());
}

function stripInlineMarkdown(value: string) {
  return value
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .replace(/[`*_]/g, "");
}

function slugify(value: string) {
  return value
    .toLowerCase()
    .replace(/[^a-z0-9\s-]/g, "")
    .trim()
    .replace(/\s+/g, "-") || "section";
}
