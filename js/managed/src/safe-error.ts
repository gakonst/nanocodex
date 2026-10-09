/**
 * Content-free failure diagnostics for background retry logs. SDK error codes
 * are stable identifiers; messages are bounded with quoted values, URLs and
 * long identifiers removed so user or account content never reaches logs.
 */
export function errorDiagnostics(error: unknown): { error_kind: string; reason: string; error_message: string } {
  const kind = error instanceof Error ? error.name : typeof error;
  const code = typeof error === "object" && error !== null ? (error as { code?: unknown }).code : undefined;
  const reason = typeof code === "string" && /^[A-Za-z0-9_.:-]{1,64}$/u.test(code) ? code : kind;
  const message = error instanceof Error ? error.message : "";
  return {
    error_kind: kind,
    reason,
    error_message: message
      .replace(/(["'`]).*?\1/gsu, "$1…$1")
      .replace(/[a-z][a-z0-9+.-]*:\/\/\S+/giu, "<url>")
      .replace(/[A-Za-z0-9_-]{24,}/gu, "<id>")
      .replace(/\s+/gu, " ")
      .slice(0, 160),
  };
}
