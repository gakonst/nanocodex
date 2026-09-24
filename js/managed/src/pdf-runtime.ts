import type { PdfExtractionOptions } from "nanocodex-tools/pdf-command";

/** PDF bytes never enter the managed Worker bundle's PDF.js module graph. */
export async function extractPdfTextFromMediaService(
  service: Fetcher, data: Uint8Array, options: PdfExtractionOptions, signal?: AbortSignal,
): Promise<string> {
  signal?.throwIfAborted();
  const body = new FormData();
  body.set("options", JSON.stringify({ first: options.first, last: options.last,
    layout: options.layout, raw: options.raw, pageBreaks: options.pageBreaks }));
  body.set("input", new Blob([data as Uint8Array<ArrayBuffer>]), "input.pdf");
  const response = await service.fetch(new Request("https://media.internal/pdf-text", { method: "POST", body, signal }));
  signal?.throwIfAborted();
  if (!response.ok) throw new Error(`PDF media extraction failed (${response.status}): ${await response.text()}`);
  return response.text();
}
