import type { Workspace } from "../tools/types.mjs";
import { createPdfTextCommandWithExtractor } from "./pdf-command.js";

/** Standalone local PDF command; managed Workers use the private media service instead. */
export function createPdfTextCommand(filesystem: () => Workspace) {
  return createPdfTextCommandWithExtractor(filesystem, async (data, options, signal) => {
    const { extractPdfText } = await import("./pdf-extract.js");
    return extractPdfText(data, options, signal);
  });
}
