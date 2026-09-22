// OpenRouter can mirror visible reasoning in both its legacy string fields and
// reasoning_details. Project one representation per chunk; retain the original
// details separately for tool-history replay, including opaque signatures/data.
export function chatReasoningText(message) {
  const text = message.reasoning_content ?? message.reasoning;
  if (text != null && typeof text !== "string") throw new Error("Responses: invalid reasoning text");
  const details = message.reasoning_details;
  if (details != null && (!Array.isArray(details)
    || details.some(detail => !detail || typeof detail !== "object" || Array.isArray(detail)))) {
    throw new Error("Responses: invalid reasoning details");
  }
  let visible = "";
  for (const detail of details ?? []) {
    const value = detail.type === "reasoning.text" ? detail.text
      : detail.type === "reasoning.summary" ? detail.summary : undefined;
    if (value != null && typeof value !== "string") throw new Error("Responses: invalid reasoning detail text");
    visible += value ?? "";
  }
  return text || visible;
}
