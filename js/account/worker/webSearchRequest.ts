/** Preserve the standalone Codex search request across the authenticated host boundary. */
export function webSearchRequest(decoded: Record<string, unknown>): Record<string, unknown> {
  const sessionId = decoded.session_id;
  if (typeof sessionId !== "string" || !/^[A-Za-z0-9._:-]{1,200}$/.test(sessionId)) {
    throw new Error("invalid session");
  }
  const commands = decoded.commands;
  if (!commands || typeof commands !== "object" || Array.isArray(commands)) {
    throw new Error("commands must be an object");
  }
  const model = decoded.model ?? "gpt-5.6-sol";
  if (typeof model !== "string" || !/^[A-Za-z0-9._:-]{1,200}$/.test(model)) {
    throw new Error("invalid model");
  }
  if (decoded.input !== undefined && !Array.isArray(decoded.input)) {
    throw new Error("input must be an array");
  }
  const maxOutputTokens = decoded.max_output_tokens ?? 10_000;
  if (typeof maxOutputTokens !== "number" || !Number.isSafeInteger(maxOutputTokens) || maxOutputTokens < 0) {
    throw new Error("invalid output token budget");
  }
  return {
    id: sessionId, model, input: decoded.input, commands,
    settings: { allowed_callers: ["direct"], external_web_access: true },
    max_output_tokens: maxOutputTokens,
  };
}
