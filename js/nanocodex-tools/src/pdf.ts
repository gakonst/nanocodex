import type { Workspace } from "../tools/types.mjs";
import { createPdfTextCommandWithExtractor } from "./pdf-command.js";
import { extractPdfText } from "./pdf-extract.js";

/** Standalone local PDF command; managed Workers use the private media service instead. */
export function createPdfTextCommand(filesystem: () => Workspace) {
  return createPdfTextCommandWithExtractor(filesystem, extractPdfText);
}
