import { getDocumentProxy } from "unpdf";
import type { PdfExtractionOptions } from "./pdf-command.js";

type TextItem = { str: string; transform: number[]; width: number; height: number; hasEOL?: boolean };

/** In-process PDF.js extraction. Import this module only in the isolated media service or local tools. */
export async function extractPdfText(data: Uint8Array, options: PdfExtractionOptions, signal?: AbortSignal): Promise<string> {
  let document: Awaited<ReturnType<typeof getDocumentProxy>> | undefined;
  try {
    signal?.throwIfAborted();
    document = await getDocumentProxy(data.slice(), {
      useSystemFonts: false,
      disableFontFace: true,
      useWorkerFetch: false,
      isOffscreenCanvasSupported: false,
      verbosity: 0,
    });
    const last = Math.min(options.last ?? document.numPages, document.numPages);
    if (options.first > last) throw new Error("page range is outside the document");
    const pages: string[] = [];
    for (let number = options.first; number <= last; number++) {
      signal?.throwIfAborted();
      const page = await document.getPage(number);
      try {
        const content = await page.getTextContent();
        const items = content.items.filter(item => "str" in item) as TextItem[];
        // Normalize positions to displayed page coordinates, including /Rotate.
        const [a, b, c, d, e, f] = page.getViewport({ scale: 1 }).transform;
        const displayed = items.map(item => {
          const [x, y] = item.transform.slice(4);
          return { ...item, transform: [...item.transform.slice(0, 4),
            a * x + c * y + e, -(b * x + d * y + f)] };
        });
        pages.push(formatPage(displayed, options));
      } finally { page.cleanup(); }
    }
    const text = pages.map(page => page + (options.pageBreaks ? "\f" : "")).join("");
    return text;
  } finally { await document?.loadingTask.destroy(); }
}

function formatPage(items: TextItem[], options: PdfExtractionOptions): string {
  if (options.raw) {
    return items.map(item => item.str + (item.hasEOL ? "\n" : "")).join("").trimEnd() + "\n";
  }
  const lines: { y: number; height: number; items: TextItem[] }[] = [];
  for (const item of items) {
    if (!item.str) continue;
    const y = item.transform[5] ?? 0;
    const height = Math.abs(item.height) || Math.hypot(item.transform[2] ?? 0, item.transform[3] ?? 0) || 12;
    let line = lines.find(line => Math.abs(line.y - y) <= Math.min(line.height, height) * 0.35);
    if (!line) { line = { y, height, items: [] }; lines.push(line); }
    line.items.push(item);
  }
  lines.sort((a, b) => b.y - a.y);
  const widths = items.filter(item => item.str.trim() && item.width > 0).map(item => item.width / item.str.length).sort((a, b) => a - b);
  const cell = Math.max(1, widths[Math.floor(widths.length / 2)] ?? 6);
  const left = Math.min(...items.filter(item => item.str).map(item => item.transform[4] ?? 0));
  return lines.map(line => {
    line.items.sort((a, b) => (a.transform[4] ?? 0) - (b.transform[4] ?? 0));
    let text = "", right = left;
    for (const item of line.items) {
      const x = item.transform[4] ?? 0;
      const spaces = options.layout
        ? Math.max(0, Math.round((x - left) / cell) - text.length)
        : text && x - right > cell * 0.2 && !text.endsWith(" ") && !item.str.startsWith(" ") ? 1 : 0;
      // Bad coordinates must not allocate unbounded padding.
      text += " ".repeat(Math.min(spaces, 10_000)) + item.str;
      right = x + item.width;
    }
    return text.trimEnd();
  }).join("\n") + "\n";
}
