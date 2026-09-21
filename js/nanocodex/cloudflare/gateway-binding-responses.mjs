// The portable adapter owns tool aliases and full-history validation. This
// bridge only changes wire formats: Cloudflare's OpenAI binding takes Responses,
// including for models that do not offer Chat Completions.
const fail = () => { throw new Error("Gateway Responses: invalid binding response"); };

export function toBindingResponsesInput(chat, effort) {
  const input = [];
  for (const message of chat.messages) {
    if (message.role === "tool") {
      input.push({ type: "function_call_output", call_id: message.tool_call_id, output: message.content });
      continue;
    }
    // Portable reasoning is plaintext history, never a provider-issued opaque
    // reasoning item or encrypted state. Replay it as assistant text.
    if (message.reasoning_content) input.push({ role: "assistant", content: message.reasoning_content });
    if (message.content != null) input.push({ role: message.role, content: message.content });
    for (const call of message.tool_calls ?? []) input.push({ type: "function_call", call_id: call.id,
      name: call.function.name, arguments: call.function.arguments });
  }
  const payload = { input, stream: false, store: false, reasoning: { effort } };
  if (chat.tools) payload.tools = chat.tools.map(tool => ({ type: "function", ...tool.function,
    // Preserve optional/loose managed schemas; Responses otherwise normalizes
    // omitted strict to true, unlike the existing Chat transport.
    strict: false }));
  if (chat.tool_choice !== undefined) payload.tool_choice = typeof chat.tool_choice === "string"
    ? chat.tool_choice : { type: "function", name: chat.tool_choice.function.name };
  if (chat.max_completion_tokens !== undefined) payload.max_output_tokens = chat.max_completion_tokens;
  for (const field of ["temperature", "top_p", "parallel_tool_calls"]) {
    if (chat[field] !== undefined) payload[field] = chat[field];
  }
  return payload;
}

export function fromBindingResponsesResult(result, parallelToolCalls) {
  if (!result || typeof result !== "object" || result.error || result.object !== "response"
    || !["completed", "incomplete"].includes(result.status) || !Array.isArray(result.output)) fail();
  let finish_reason = "stop";
  if (result.status === "incomplete") {
    const reason = result.incomplete_details?.reason;
    if (!["max_output_tokens", "content_filter"].includes(reason)) fail();
    finish_reason = reason === "max_output_tokens" ? "length" : "content_filter";
  }
  const texts = [], reasoning = [], tool_calls = [];
  for (const item of result.output) {
    if (!item || typeof item !== "object") fail();
    if (item.status !== undefined && item.status !== "completed"
      && !(result.status === "incomplete" && item.status === "incomplete")) fail();
    if (item.type === "message") {
      if (item.role !== "assistant" || !Array.isArray(item.content)) fail();
      for (const part of item.content) {
        if (part?.type !== "output_text" || typeof part.text !== "string") fail();
        texts.push(part.text);
      }
    } else if (item.type === "reasoning") {
      for (const [field, type] of [["summary", "summary_text"], ["content", "reasoning_text"]]) {
        if (item[field] !== undefined && !Array.isArray(item[field])) fail();
        for (const part of item[field] ?? []) {
          if (part?.type !== type || typeof part.text !== "string") fail();
          reasoning.push(part.text);
        }
      }
      // encrypted_content intentionally stays upstream: full history is portable.
    } else if (item.type === "function_call") {
      if (result.status !== "completed" || typeof item.call_id !== "string" || !item.call_id
        || typeof item.name !== "string" || typeof item.arguments !== "string") fail();
      tool_calls.push({ id: item.call_id, type: "function", function: { name: item.name, arguments: item.arguments } });
    } else fail(); // Never accept provider-hosted tools or unsupported modalities.
  }
  if (parallelToolCalls === false && tool_calls.length > 1) fail();
  if (tool_calls.length) finish_reason = "tool_calls";
  const message = { content: texts.join("\n"), reasoning_content: reasoning.join("\n"), tool_calls };
  let usage;
  if (result.usage != null) {
    const count = value => { if (value !== undefined && (!Number.isSafeInteger(value) || value < 0)) fail(); return value; };
    usage = { prompt_tokens: count(result.usage.input_tokens), completion_tokens: count(result.usage.output_tokens),
      total_tokens: count(result.usage.total_tokens),
      prompt_tokens_details: { cached_tokens: count(result.usage.input_tokens_details?.cached_tokens) },
      completion_tokens_details: { reasoning_tokens: count(result.usage.output_tokens_details?.reasoning_tokens) } };
  }
  return { choices: [{ message, finish_reason }], ...(usage ? { usage } : {}) };
}
