const MODEL = "@cf/zai-org/glm-5.3";
const MODELS = [MODEL, "gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"];
const BASE = "https://workers-ai.invalid/v1";
const fail = (message) => { throw new Error(`Workers AI Responses: ${message}`); };
const json = (value) => typeof value === "string" ? value : JSON.stringify(value);
const key = (namespace, name) => JSON.stringify([namespace ?? null, name]);

/** Stateless, buffered Responses transport for the Workers AI binding. */
export function createWorkersAiResponses(ai, options = {}) {
  if (typeof ai?.run !== "function") throw new TypeError("Workers AI binding must provide run()");
  const apiBaseUrl = options.apiBaseUrl ?? BASE;
  const model = options.model ?? MODEL;
  if (!MODELS.includes(model)) fail("unsupported canonical model");
  return Object.freeze({
    apiBaseUrl,
    async createResponse(endpoint, _sessionId, request) {
      if (endpoint !== `${apiBaseUrl}/responses`) fail("unexpected endpoint; compaction is not supported");
      if (request.authorization !== "host_managed") fail("hostManaged authorization is required");
      request.signal?.throwIfAborted();
      const body = JSON.parse(request.body);
      const { input, registry } = translate(body, model);
      const result = await abortable(Promise.resolve().then(() => ai.run(model, input)), request.signal);
      request.signal?.throwIfAborted();
      return toResponse(result, registry, model);
    },
  });
}

async function abortable(promise, signal) {
  if (!signal) return promise;
  signal.throwIfAborted();
  let listener;
  const cancelled = new Promise((_, reject) => {
    listener = () => reject(signal.reason ?? new DOMException("Aborted", "AbortError"));
    signal.addEventListener("abort", listener, { once: true });
  });
  try { return await Promise.race([promise, cancelled]); }
  finally { signal.removeEventListener("abort", listener); }
}

function translate(body, model) {
  if (!body || typeof body !== "object" || Array.isArray(body)) fail("expected a Responses request object");
  if (body.model !== undefined && body.model !== model) fail("unsupported model override; expected pinned model");
  const effort = (value) => {
    if (value !== undefined && !["low", "medium", "high"].includes(value)) fail("unsupported reasoning effort; expected low, medium or high");
    return value;
  };
  if (body.previous_response_id) fail("previous_response_id is unsupported; send the complete Responses history");
  if (body.context_management?.length) fail("provider compaction is unsupported; supply portable text history");
  const history = typeof body.input === "string"
    ? [{ type: "message", role: "user", content: body.input }] : body.input ?? [];
  if (!Array.isArray(history)) fail("expected complete Responses history");
  if (body.text?.format && body.text.format.type !== "text") fail("structured output formats are unsupported");
  const registry = new Map();
  const byIdentity = new Map();
  function register(tool, namespace, description = "") {
    if (tool.type === "namespace") {
      if (namespace) fail("nested tool namespaces are unsupported");
      for (const child of tool.tools) register(child, tool.name, tool.description);
      return;
    }
    if (!["function", "custom", "tool_search"].includes(tool.type)) fail(`unsupported tool type ${tool.type}`);
    if (tool.type === "tool_search" && tool.execution !== "client") fail("tool search must use client execution");
    const name = tool.type === "tool_search" ? "tool_search" : tool.name;
    const identity = key(namespace, name);
    let entry = byIdentity.get(identity);
    if (!entry) {
      entry = { alias: `tool_${registry.size}`, name, namespace, type: tool.type };
      registry.set(entry.alias, entry);
      byIdentity.set(identity, entry);
    }
    entry.definition = { type: "function", function: {
      name: entry.alias,
      description: [namespace ? `${namespace}.${name}` : name, description, tool.description,
        tool.type === "custom" ? "Pass the exact free-form tool input as the JSON string field input. Preserve all code and newlines." : "",
        tool.format?.definition ? `Input grammar (${tool.format.syntax}): ${tool.format.definition}` : ""].filter(Boolean).join("\n"),
      parameters: tool.type === "custom" ? {
        type: "object", properties: { input: { type: "string" } }, required: ["input"], additionalProperties: false,
      } : tool.parameters ?? { type: "object", properties: {} },
    } };
  }
  for (const tool of body.tools ?? []) register(tool);
  for (const item of history) {
    if (item.type === "additional_tools" || item.type === "tool_search_output") {
      for (const tool of item.tools ?? []) register(tool);
    }
  }
  const messages = [];
  if (body.instructions) messages.push({ role: "system", content: body.instructions });
  const pending = new Set();
  const seenCallIds = new Set();
  let reasoningEffort = effort(body.reasoning?.effort);
  for (const item of history) {
    switch (item.type ?? "message") {
      case "additional_tools": break;
      case "configuration_update":
        reasoningEffort = effort(item.reasoning?.effort) ?? reasoningEffort;
        break;
      case "message": {
        if (pending.size) fail("tool calls require their outputs before the next message");
        if (!["user", "assistant", "system", "developer"].includes(item.role)) fail(`unsupported message role ${item.role}`);
        messages.push({ role: item.role === "developer" ? "system" : item.role, content: textContent(item.content) });
        break;
      }
      case "agent_message": {
        if (pending.size) fail("tool calls require their outputs before an agent message");
        messages.push({ role: "user", content: JSON.stringify({ author: item.author, recipient: item.recipient,
          message: textContent(item.content) }) });
        break;
      }
      case "reasoning": {
        // Encrypted provider reasoning is not portable, but is not user history.
        const text = [...(item.summary ?? []), ...(item.content ?? [])].map(part => part.text ?? "").join("\n");
        if (text) {
          const previous = messages.at(-1);
          if (previous?.role === "assistant") previous.reasoning_content = text;
          else messages.push({ role: "assistant", content: null, reasoning_content: text });
        }
        break;
      }
      case "function_call":
      case "custom_tool_call":
      case "tool_search_call": {
        if (item.encrypted_function_args?.length) fail("encrypted function arguments cannot be replayed");
        const name = item.type === "tool_search_call" ? "tool_search" : item.name;
        let entry = byIdentity.get(key(item.namespace, name));
        if (!entry) {
          // Historical calls may belong to tools no longer available this turn.
          entry = { alias: `history_${byIdentity.size}` };
          byIdentity.set(key(item.namespace, name), entry);
        }
        if (typeof item.call_id !== "string" || !item.call_id || seenCallIds.has(item.call_id)) fail("missing or duplicate tool call ID");
        if (pending.size && messages.at(-1)?.role !== "assistant") fail("tool calls require all outputs before another tool call");
        seenCallIds.add(item.call_id);
        pending.add(item.call_id);
        const call = { id: item.call_id, type: "function", function: { name: entry.alias,
          arguments: item.type === "custom_tool_call" ? JSON.stringify({ input: item.input }) : json(item.arguments) } };
        const previous = messages.at(-1);
        if (previous?.role === "assistant") (previous.tool_calls ??= []).push(call);
        else messages.push({ role: "assistant", content: null, tool_calls: [call] });
        break;
      }
      case "function_call_output":
      case "custom_tool_call_output":
      case "tool_search_output": {
        if (!pending.delete(item.call_id)) fail("tool output has no matching call in full history");
        messages.push({ role: "tool", tool_call_id: item.call_id,
          content: item.type === "tool_search_output" ? JSON.stringify({ tools: item.tools }) : textContent(item.output) });
        break;
      }
      case "compaction": case "compaction_summary": case "context_compaction": case "compaction_trigger":
        fail("compaction is unsupported; restore portable text history before using Workers AI");
        break;
      default: fail(`unsupported history item ${item.type}`);
    }
  }
  if (pending.size) fail("full history contains tool calls without outputs");
  const input = { messages, stream: false };
  if (registry.size) input.tools = [...registry.values()].map(entry => entry.definition);
  for (const field of ["temperature", "top_p", "parallel_tool_calls"]) {
    if (body[field] !== undefined) input[field] = body[field];
  }
  if (body.max_output_tokens !== undefined) input.max_completion_tokens = body.max_output_tokens;
  if (reasoningEffort !== undefined) input.reasoning_effort = reasoningEffort;
  if (body.tool_choice !== undefined) {
    if (typeof body.tool_choice === "string") {
      if (!["auto", "none", "required"].includes(body.tool_choice)) fail("unsupported tool_choice");
      input.tool_choice = body.tool_choice;
    }
    else {
      const choice = byIdentity.get(key(body.tool_choice.namespace, body.tool_choice.name ?? (body.tool_choice.type === "tool_search" ? "tool_search" : undefined)));
      if (!choice?.definition) fail("tool_choice refers to an unavailable tool");
      input.tool_choice = { type: "function", function: { name: choice.alias } };
    }
  }
  return { input, registry };
}

function textContent(content) {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) fail("expected text content");
  return content.map(part => {
    if (!["input_text", "output_text", "text"].includes(part.type) || typeof part.text !== "string") {
      fail(`unsupported content ${part.type}; GLM history must contain text`);
    }
    return part.text;
  }).join("\n");
}

function toResponse(result, registry, model) {
  const choice = result?.choices?.[0];
  if (result?.error || !choice?.message || typeof choice.message !== "object" || Array.isArray(choice.message)
    || !["stop", "tool_calls", "length", "content_filter"].includes(choice.finish_reason)) {
    fail("invalid chat completion or unsupported finish reason");
  }
  const message = choice.message;
  if (message.content != null && typeof message.content !== "string") fail("unsupported completion content");
  if (message.tool_calls != null && !Array.isArray(message.tool_calls)) fail("invalid completion tool calls");
  if (message.refusal) fail("provider refused completion");
  if (["length", "content_filter"].includes(choice.finish_reason) && message.tool_calls?.length) {
    fail("incomplete completion cannot dispatch tool calls");
  }
  const output = [];
  const id = `resp_${crypto.randomUUID()}`;
  const reasoning = message.reasoning_content ?? message.reasoning;
  if (typeof reasoning === "string" && reasoning) {
    output.push({ type: "reasoning", id: `rs_${crypto.randomUUID()}`, status: "completed",
      summary: [], content: [{ type: "reasoning_text", text: reasoning }] });
  }
  if (typeof message.content === "string" && message.content) {
    output.push({ type: "message", id: `msg_${crypto.randomUUID()}`, role: "assistant", status: "completed",
      content: [{ type: "output_text", text: message.content, annotations: [] }] });
  }
  const callIds = new Set();
  for (const call of message.tool_calls ?? []) {
    if (!call || (call.type !== undefined && call.type !== "function")) fail("unsupported completion tool call");
    const returnedName = call.function?.name;
    let entry = registry.get(returnedName);
    if (!entry && typeof returnedName === "string") {
      // GLM sometimes returns the original name shown in the description instead
      // of the advertised alias. Resolve only an exact, unambiguous registered
      // identity; never guess among namespaces or dispatch an undeclared tool.
      const matches = [...registry.values()].filter(candidate => returnedName === candidate.name
        || returnedName === (candidate.namespace ? `${candidate.namespace}.${candidate.name}` : candidate.name));
      if (matches.length === 1) entry = matches[0];
    }
    if (!entry) fail("model returned an unknown tool alias");
    const argumentsText = json(call.function.arguments);
    let args;
    try { args = JSON.parse(argumentsText); } catch { fail("model returned invalid tool JSON"); }
    if (call.id !== undefined && (typeof call.id !== "string" || !call.id)) fail("invalid tool call ID");
    if (!args || typeof args !== "object" || Array.isArray(args)) fail("tool arguments must be a JSON object");
    const call_id = call.id || `call_${crypto.randomUUID()}`;
    if (callIds.has(call_id)) fail("model returned duplicate tool call IDs");
    callIds.add(call_id);
    const common = { id: `tool_${crypto.randomUUID()}`, call_id, status: "completed" };
    if (entry.type === "tool_search") {
      output.push({ ...common, type: "tool_search_call", execution: "client", arguments: args });
    } else {
      const named = { ...common, name: entry.name, ...(entry.namespace ? { namespace: entry.namespace } : {}) };
      if (entry.type === "custom") {
        if (typeof args?.input !== "string") fail("custom tool arguments must contain a string input");
        output.push({ ...named, type: "custom_tool_call", input: args.input });
      } else output.push({ ...named, type: "function_call", arguments: argumentsText });
    }
  }
  if (choice.finish_reason === "tool_calls" && !callIds.size) fail("tool_calls finish reason omitted tool calls");
  const incomplete = ["length", "content_filter"].includes(choice.finish_reason);
  const usage = result.usage ? {
    input_tokens: result.usage.prompt_tokens ?? 0, output_tokens: result.usage.completion_tokens ?? 0,
    total_tokens: result.usage.total_tokens ?? (result.usage.prompt_tokens ?? 0) + (result.usage.completion_tokens ?? 0),
    input_tokens_details: { cached_tokens: result.usage.prompt_tokens_details?.cached_tokens ?? 0 },
    output_tokens_details: { reasoning_tokens: result.usage.completion_tokens_details?.reasoning_tokens ?? 0 },
  } : null;
  const response = { id, object: "response", model, status: incomplete ? "incomplete" : "completed", output,
    usage, end_turn: !incomplete && !callIds.size,
    ...(incomplete ? { incomplete_details: { reason: choice.finish_reason === "length" ? "max_output_tokens" : "content_filter" } } : {}) };
  const events = [];
  const emit = (type, fields) => events.push({ type, sequence_number: events.length, ...fields });
  emit("response.created", { response: { ...response, status: "in_progress", output: [], usage: null } });
  for (const [output_index, item] of output.entries()) {
    const fields = { output_index, item_id: item.id };
    const initial = { ...item, status: "in_progress" };
    if (item.type === "message" || item.type === "reasoning") initial.content = [];
    if (item.type === "function_call") initial.arguments = "";
    if (item.type === "custom_tool_call") initial.input = "";
    emit("response.output_item.added", { output_index, item: initial });
    if (item.type === "message") {
      emit("response.content_part.added", { ...fields, content_index: 0, part: { type: "output_text", text: "", annotations: [] } });
      emit("response.output_text.delta", { ...fields, content_index: 0, delta: item.content[0].text });
      emit("response.output_text.done", { ...fields, content_index: 0, text: item.content[0].text });
      emit("response.content_part.done", { ...fields, content_index: 0, part: item.content[0] });
    } else if (item.type === "function_call" || item.type === "custom_tool_call") {
      const custom = item.type === "custom_tool_call";
      const stem = custom ? "custom_tool_call_input" : "function_call_arguments";
      const value = custom ? item.input : item.arguments;
      emit(`response.${stem}.delta`, { ...fields, call_id: item.call_id, delta: value });
      emit(`response.${stem}.done`, { ...fields, call_id: item.call_id, [custom ? "input" : "arguments"]: value });
    } else if (item.type === "reasoning") {
      emit("response.content_part.added", { ...fields, content_index: 0, part: { type: "reasoning_text", text: "" } });
      emit("response.reasoning_text.delta", { ...fields, content_index: 0, delta: item.content[0].text });
      emit("response.reasoning_text.done", { ...fields, content_index: 0, text: item.content[0].text });
      emit("response.content_part.done", { ...fields, content_index: 0, part: item.content[0] });
    }
    emit("response.output_item.done", { output_index, item });
  }
  emit(incomplete ? "response.incomplete" : "response.completed", { response });
  return new Response(events.map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join(""), {
    headers: { "content-type": "text/event-stream", "cache-control": "no-cache" },
  });
}
