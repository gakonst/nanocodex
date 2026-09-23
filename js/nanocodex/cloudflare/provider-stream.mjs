import { chatReasoningText } from "./chat-reasoning.mjs";
import { fromBindingResponsesResult } from "./gateway-binding-responses.mjs";

// Codes are local constants only. Never surface an upstream exception/message.
class StreamProtocolError extends Error {
  constructor(code) { super(`Responses: invalid provider stream\nProtocol invariant: ${code}`); this.code = code; }
}
const invalid = code => { throw new StreamProtocolError(code); };
// The portable normalizer has static failures but can also throw arbitrary
// exceptions. Only exact known messages map to public diagnostics.
const normalizationCodes = new Map([
  ["invalid chat completion or unsupported finish reason", "normalize_completion"],
  ["unsupported completion content", "normalize_content"],
  ["invalid completion tool calls", "normalize_tool_calls"],
  ["provider refused completion", "normalize_refusal"],
  ["incomplete completion cannot dispatch tool calls", "normalize_incomplete_tools"],
  ["invalid reasoning details", "normalize_reasoning_details"],
  ["reasoning details exceed limit", "normalize_reasoning_size"],
  ["tool choice forbids calls", "normalize_tool_forbidden"],
  ["required tool call missing", "normalize_tool_required"],
  ["unsupported completion tool call", "normalize_tool_type"],
  ["model returned an unknown tool alias", "normalize_tool_alias"],
  ["model returned a different forced tool", "normalize_forced_tool"],
  ["model returned invalid tool JSON", "normalize_tool_json"],
  ["model returned unwrapped custom tool input", "normalize_custom_raw_input"],
  ["invalid tool call ID", "normalize_tool_id"],
  ["tool arguments must be a JSON object", "normalize_tool_arguments"],
  ["model returned duplicate tool call IDs", "normalize_tool_duplicate_id"],
  ["custom tool arguments must contain a string input", "normalize_custom_input"],
  ["tool_calls finish reason omitted tool calls", "normalize_missing_tools"],
].map(([message, code]) => [`Workers AI Responses: ${message}`, code]));
for (const [message, code] of [
  ["invalid reasoning text", "normalize_reasoning_text"],
  ["invalid reasoning details", "normalize_reasoning_details"],
  ["invalid reasoning detail text", "normalize_reasoning_detail_text"],
]) normalizationCodes.set(`Responses: ${message}`, code);
class StreamReadError extends Error {}
const encoder = new TextEncoder();
const frame = event => encoder.encode(`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`);
export const providerStream = (body, format = "chat", hooks = {}) => ({ providerStream: true, body, format, ...hooks });

// Streaming UTF-8/SSE framing, including split CRLF, multiline data and comments.
// Bound each frame and the full wire stream, including endless keepalives.
async function* records(reader) {
  const decoder = new TextDecoder("utf-8", { fatal: true });
  let buffer = "", data = [], event = "", size = 0, total = 0;
  const line = value => {
    if (!value) {
      const record = data.length ? { data: data.join("\n"), event } : null;
      data = []; event = ""; size = 0;
      return record;
    }
    size += value.length;
    if (size > 4 * 1024 * 1024) invalid("frame_size");
    const colon = value.indexOf(":");
    const field = colon < 0 ? value : value.slice(0, colon);
    let valueText = colon < 0 ? "" : value.slice(colon + 1);
    if (valueText.startsWith(" ")) valueText = valueText.slice(1);
    if (field === "data") data.push(valueText);
    else if (field === "event") event = valueText;
    return null;
  };
  while (true) {
    let read;
    try { read = await reader.read(); } catch { throw new StreamReadError(); }
    const { value, done } = read;
    total += value?.byteLength ?? 0;
    if (total > 32 * 1024 * 1024) invalid("wire_size");
    try { buffer += done ? decoder.decode() : decoder.decode(value, { stream: true }); }
    catch { invalid("frame_utf8"); }
    if (buffer.length > 4 * 1024 * 1024) invalid("frame_buffer_size");
    let match;
    while ((match = /\r\n|\r|\n/.exec(buffer))) {
      if (!done && match[0] === "\r" && match.index === buffer.length - 1) break;
      const record = line(buffer.slice(0, match.index));
      buffer = buffer.slice(match.index + match[0].length);
      if (record) yield record;
    }
    if (done) {
      // A terminal event must be fully framed; never accept a truncated last frame.
      if (buffer || data.length) invalid("frame_truncated");
      return;
    }
  }
}

export function streamResponse(source, normalize, responseEvents, signal) {
  if (!(source.body instanceof ReadableStream)) invalid("body_type");
  const checkedNormalize = (result, prologue) => {
    try { return normalize(result, prologue); }
    catch (error) { invalid(normalizationCodes.get(error instanceof Error ? error.message : undefined) ?? "normalize_unknown"); }
  };
  const reader = source.body.getReader();
  const iterator = records(reader);
  let controller, settled = false, first = false, sequence = 0, finalSeen = false;
  const id = `resp_${crypto.randomUUID()}`;
  const reasoningDetails = [];
  let retainedSize = 0;
  const retain = text => {
    retainedSize += encoder.encode(text).byteLength;
    if (retainedSize > 8 * 1024 * 1024) invalid("retained_size");
  };
  const live = new Map();
  const calls = new Map();
  const nativeItems = new Map();
  const nativeParts = new Map();
  let usage, finishReason, started = false, bindingUsageSeen = false;
  const emit = (type, fields) => controller.enqueue(frame({ type, sequence_number: sequence++, ...fields }));
  const firstToken = () => { if (!first) { first = true; try { source.firstToken?.(); } catch { /* best effort */ } } };
  const finish = async outcome => {
    if (settled) return;
    settled = true;
    signal?.removeEventListener("abort", abort);
    try { await source.finish?.(outcome); } catch { /* best effort */ }
  };
  let readerCancelled = false;
  const cancelReader = () => {
    if (readerCancelled) return;
    readerCancelled = true;
    void reader.cancel().catch(() => {}).finally(() => { reader.releaseLock(); });
  };
  const abort = () => {
    if (settled) return;
    cancelReader();
    controller.error(new DOMException("Provider stream cancelled", signal?.reason?.name === "TimeoutError" ? "TimeoutError" : "AbortError"));
    void finish(signal?.reason?.name === "TimeoutError" ? "timeout" : "cancelled");
  };
  const delta = (kind, text) => {
    if (typeof text !== "string") invalid("text_type");
    if (!text) return;
    retain(text);
    if (kind === "message") firstToken();
    let entry = live.get(kind);
    if (!entry) {
      const item = kind === "message"
        ? { type: kind, id: `msg_${crypto.randomUUID()}`, role: "assistant", status: "in_progress", content: [] }
        : { type: kind, id: `rs_${crypto.randomUUID()}`, status: "in_progress", summary: [], content: [] };
      entry = { item, index: live.size, text: "" };
      live.set(kind, entry);
      emit("response.output_item.added", { output_index: entry.index, item });
      emit("response.content_part.added", { output_index: entry.index, item_id: item.id, content_index: 0,
        part: kind === "message" ? { type: "output_text", text: "", annotations: [] } : { type: "reasoning_text", text: "" } });
    }
    entry.text += text;
    emit(kind === "message" ? "response.output_text.delta" : "response.reasoning_text.delta",
      { output_index: entry.index, item_id: entry.item.id, content_index: 0, delta: text });
  };
  // Tool declarations/arguments stay private until the existing normalizer has
  // checked aliases, JSON, IDs and completeness. The parallel request bit is a
  // generation preference; the host scheduler owns execution concurrency.
  const complete = async result => {
    const response = checkedNormalize(result);
    response.id = id;
    const output = [];
    for (const [kind, entry] of live) {
      const item = response.output.find(value => value.type === kind);
      if (item?.content?.[0]?.text !== entry.text) invalid("output_text_mismatch");
      item.id = entry.item.id;
      output.push(item);
    }
    output.push(...response.output.filter(item => !live.has(item.type)));
    response.output = output;
    for (const event of responseEvents(response)) {
      if (event.type === "response.created") continue;
      const entry = [...live.values()].find(value => value.index === event.output_index);
      if (entry && (event.type.endsWith(".added") || event.type.endsWith(".delta"))) continue;
      const { type, sequence_number: _sequence, ...fields } = event;
      if ((["response.output_text.delta", "response.function_call_arguments.delta", "response.custom_tool_call_input.delta"].includes(type) && fields.delta)
        || (type === "response.output_item.added" && ["function_call", "custom_tool_call", "tool_search_call"].includes(fields.item?.type))) firstToken();
      emit(type, fields);
    }
    finalSeen = true;
    cancelReader();
    await finish("success");
    controller.close();
  };
  const chat = async value => {
    // Workers AI appends its aggregate usage as a binding-specific envelope,
    // after the Chat finish chunk and before [DONE]. It is metadata, not output.
    if (source.format === "workers_ai_chat" && value && Object.hasOwn(value, "response")) {
      if (!finishReason || bindingUsageSeen || value.response !== "" || !value.usage
        || typeof value.usage !== "object" || Array.isArray(value.usage)
        || Object.keys(value).some(key => !["response", "usage"].includes(key))) invalid("binding_usage_trailer");
      bindingUsageSeen = true;
      usage = value.usage;
      return;
    }
    if (bindingUsageSeen || !value || value.error || !Array.isArray(value.choices) || value.choices.length > 1) invalid("chat_envelope");
    if (value.usage != null) usage = value.usage;
    const choice = value.choices[0];
    if (!choice) { if (!finishReason || value.usage == null) invalid("chat_empty_choice"); return; }
    if (choice.index !== undefined && choice.index !== 0) invalid("chat_choice_index");
    const part = choice.delta;
    if (!part || typeof part !== "object" || Array.isArray(part) || part.refusal
      || (part.role != null && part.role !== "assistant")) invalid("chat_delta");
    if (finishReason) {
      // OpenRouter repeats its finish choice with an empty delta on the usage
      // trailer before [DONE]. Admit metadata only, never additional output or
      // a changed terminal reason after the first finish chunk.
      if (value.usage == null || choice.finish_reason !== finishReason
        || Object.entries(part).some(([field, fragment]) => field === "role" ? fragment != null && fragment !== "assistant"
          : !["content", "reasoning_content", "reasoning"].includes(field) || (fragment !== null && fragment !== ""))) invalid("chat_usage_trailer");
      return;
    }
    if (part.content != null) delta("message", part.content);
    let reasoning;
    try { reasoning = chatReasoningText(part); } catch { invalid("chat_reasoning"); }
    delta("reasoning", reasoning);
    if (part.reasoning_details != null) {
      if (!Array.isArray(part.reasoning_details) || part.reasoning_details.some(d => !d || typeof d !== "object" || Array.isArray(d))) invalid("chat_reasoning_details");
      retain(JSON.stringify(part.reasoning_details));
      reasoningDetails.push(...part.reasoning_details);
    }
    if (part.tool_calls != null) {
      if (!Array.isArray(part.tool_calls)) invalid("tool_calls_type");
      for (const fragment of part.tool_calls) {
        if (!Number.isSafeInteger(fragment?.index) || fragment.index < 0 || fragment.index >= 1024) invalid("tool_fragment_index");
        if (fragment.type != null && fragment.type !== "function") invalid("tool_fragment_type");
        let call = calls.get(fragment.index);
        if (!call) { call = { type: "function", function: { name: "", arguments: "" } }; calls.set(fragment.index, call); }
        if (fragment.id != null) {
          if (typeof fragment.id !== "string" || !fragment.id) invalid("tool_fragment_id");
          if (call.id && call.id !== fragment.id) invalid("tool_fragment_id_changed");
          call.id = fragment.id;
        }
        for (const field of ["name", "arguments"]) if (fragment.function?.[field] != null) {
          if (typeof fragment.function[field] !== "string") invalid("tool_fragment_field_type");
          retain(fragment.function[field]);
          call.function[field] += fragment.function[field];
          if (call.function[field].length > 4 * 1024 * 1024) invalid("tool_fragment_size");

        }
      }
    }
    if (choice.finish_reason != null) {
      if (!["stop", "tool_calls", "length", "content_filter"].includes(choice.finish_reason)) invalid("chat_finish_reason");
      finishReason = choice.finish_reason;
    }
  };
  const native = async value => {
    if (!value || typeof value.type !== "string" || value.error) invalid("native_envelope");
    if (value.type === "response.output_item.added") {
      if (!Number.isSafeInteger(value.output_index) || value.output_index < 0 || value.output_index >= 1024 || nativeItems.has(value.output_index)
        || !value.item || typeof value.item.id !== "string" || !value.item.id || !["message", "reasoning", "function_call"].includes(value.item.type)
        || [...nativeItems.values()].some(item => item.id === value.item.id)
        || (value.item.type === "message" && value.item.role !== "assistant")) invalid("native_item");
      nativeItems.set(value.output_index, { ...value.item, streamedText: "", streamedArguments: "" });
    } else if (["response.output_text.delta", "response.reasoning_text.delta", "response.reasoning_summary_text.delta", "response.function_call_arguments.delta"].includes(value.type)) {
      const item = nativeItems.get(value.output_index);
      if (!item || value.item_id !== item.id || typeof value.delta !== "string") invalid("native_delta");
      if (value.type === "response.function_call_arguments.delta") {
        if (item.type !== "function_call") invalid("native_tool_type");
        retain(value.delta);
        item.streamedArguments += value.delta;
        if (item.streamedArguments.length > 4 * 1024 * 1024) invalid("native_arguments_size");

      } else {
        const kind = value.type === "response.output_text.delta" ? "message" : "reasoning";
        if (item.type !== kind || (value.content_index !== undefined && (!Number.isSafeInteger(value.content_index) || value.content_index < 0))
          || (value.summary_index !== undefined && (!Number.isSafeInteger(value.summary_index) || value.summary_index < 0))) invalid("native_text_part");
        if (value.delta) {
          const part = `${value.output_index}:${value.type}:${value.content_index ?? value.summary_index ?? 0}`;
          if (nativeParts.get(kind) !== part) {
            if (nativeParts.has(kind)) delta(kind, "\n");
            if (item.streamedText) item.streamedText += "\n";
            nativeParts.set(kind, part);
          }
          item.streamedText += value.delta;
          delta(kind, value.delta);
        }
      }
    } else if (["response.completed", "response.incomplete"].includes(value.type)) {
      if (value.response?.status !== value.type.slice("response.".length)) invalid("native_terminal_status");
      for (const [index, item] of nativeItems) {
        const final = value.response?.output?.[index];
        if (!final || item.id !== final.id || item.type !== final.type) invalid("native_terminal_item");
        if (item.streamedText) {
          const parts = item.type === "message" ? final.content : [...(final.summary ?? []), ...(final.content ?? [])];
          if (!Array.isArray(parts) || parts.map(part => part?.text).join("\n") !== item.streamedText) invalid("native_terminal_text");
        }
        if (item.type === "function_call" && (item.streamedArguments !== final.arguments
          || item.call_id !== final.call_id || item.name !== final.name)) invalid("native_terminal_tool");
      }
      let result;
      try { result = fromBindingResponsesResult(value.response); }
      catch { invalid("native_normalization"); }
      await complete(result);
    } else if (!["response.created", "response.in_progress", "response.queued", "response.output_item.done",
      "response.content_part.added", "response.content_part.done", "response.output_text.done", "response.reasoning_text.done",
      "response.reasoning_summary_part.added", "response.reasoning_summary_part.done", "response.reasoning_summary_text.done",
      "response.function_call_arguments.done"].includes(value.type)) invalid("native_event_type");
  };
  const body = new ReadableStream({
    start(value) {
      controller = value;
      signal?.addEventListener("abort", abort, { once: true });
      if (signal?.aborted) abort();
    },
    async pull() {
      if (settled) return;
      try {
        if (!started) {
          started = true;
          // Obtain canonical model identity without admitting provider data.
          const base = checkedNormalize({ choices: [{ message: { content: "" }, finish_reason: "stop" }] }, true);
          emit("response.created", { response: { ...base, id, status: "in_progress", output: [], usage: null, end_turn: false } });
          return;
        }
        const before = sequence;
        do {
          const record = await iterator.next();
          if (settled) return;
          if (record.done) invalid("terminal_missing");
          if (record.value.data === "[DONE]") {
            if (!["chat", "workers_ai_chat"].includes(source.format) || !finishReason) invalid("terminal_done");
            const tool_calls = [...calls].sort(([a], [b]) => a - b).map(([index, call], position) => {
              if (index !== position) invalid("tool_terminal_index");
              if (!call.id) invalid("tool_terminal_id_missing");
              return call;
            });
            await complete({ choices: [{ message: { content: live.get("message")?.text ?? "",
              reasoning_content: live.get("reasoning")?.text ?? "", ...(reasoningDetails.length ? { reasoning_details: reasoningDetails } : {}), tool_calls }, finish_reason: finishReason }], usage });
          } else {
            let value;
            try { value = JSON.parse(record.value.data); } catch { invalid("frame_json"); }
            if (source.format === "responses") {
              if (record.value.event && record.value.event !== value.type) invalid("native_event_mismatch");
              await native(value);
            } else await chat(value);
          }
        } while (!settled && sequence === before);
      } catch (error) {
        if (settled) return;
        cancelReader();
        controller.error(new Error(error instanceof StreamReadError
          ? "Responses: provider stream read failed"
          : `Responses: invalid provider stream\nProtocol invariant: ${error instanceof StreamProtocolError ? error.code : "unknown"}`));
        await finish(error instanceof StreamReadError ? "network_error" : "protocol_error");
      }
    },
    async cancel() {
      cancelReader();
      if (!finalSeen) await finish("cancelled");
    },
  });
  return new Response(body, { headers: { "content-type": "text/event-stream", "cache-control": "no-cache", "x-nanocodex-inference-buffering": "streaming" } });
}
