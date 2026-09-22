import { fromBindingResponsesResult } from "./gateway-binding-responses.mjs";

const invalid = () => { throw new Error("Responses: invalid provider stream"); };
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
    if (size > 4 * 1024 * 1024) invalid();
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
    if (total > 32 * 1024 * 1024) invalid();
    buffer += done ? decoder.decode() : decoder.decode(value, { stream: true });
    if (buffer.length > 4 * 1024 * 1024) invalid();
    let match;
    while ((match = /\r\n|\r|\n/.exec(buffer))) {
      if (!done && match[0] === "\r" && match.index === buffer.length - 1) break;
      const record = line(buffer.slice(0, match.index));
      buffer = buffer.slice(match.index + match[0].length);
      if (record) yield record;
    }
    if (done) {
      // A terminal event must be fully framed; never accept a truncated last frame.
      if (buffer || data.length) invalid();
      return;
    }
  }
}

export function streamResponse(source, normalize, responseEvents, signal, parallelToolCalls) {
  if (!(source.body instanceof ReadableStream)) invalid();
  const reader = source.body.getReader();
  const iterator = records(reader);
  let controller, settled = false, first = false, sequence = 0, finalSeen = false;
  const id = `resp_${crypto.randomUUID()}`;
  const reasoningDetails = [];
  let retainedSize = 0;
  const retain = text => {
    retainedSize += encoder.encode(text).byteLength;
    if (retainedSize > 8 * 1024 * 1024) invalid();
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
    if (typeof text !== "string") invalid();
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
  // checked aliases, JSON, IDs, completeness and the single-call contract.
  const complete = async result => {
    if (parallelToolCalls === false && result.choices?.[0]?.message?.tool_calls?.length > 1) invalid();
    const response = normalize(result);
    response.id = id;
    const output = [];
    for (const [kind, entry] of live) {
      const item = response.output.find(value => value.type === kind);
      if (!item || item.content[0].text !== entry.text) invalid();
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
        || Object.keys(value).some(key => !["response", "usage"].includes(key))) invalid();
      bindingUsageSeen = true;
      usage = value.usage;
      return;
    }
    if (bindingUsageSeen || !value || value.error || !Array.isArray(value.choices) || value.choices.length > 1) invalid();
    if (value.usage != null) usage = value.usage;
    const choice = value.choices[0];
    if (!choice) { if (!finishReason || value.usage == null) invalid(); return; }
    if (choice.index !== undefined && choice.index !== 0) invalid();
    const part = choice.delta;
    if (!part || typeof part !== "object" || Array.isArray(part) || part.refusal
      || (part.role != null && part.role !== "assistant")) invalid();
    if (finishReason) {
      // OpenRouter repeats its finish choice with an empty delta on the usage
      // trailer before [DONE]. Admit metadata only, never additional output or
      // a changed terminal reason after the first finish chunk.
      if (value.usage == null || choice.finish_reason !== finishReason
        || Object.entries(part).some(([field, fragment]) => field === "role" ? fragment != null && fragment !== "assistant"
          : !["content", "reasoning_content", "reasoning"].includes(field) || (fragment !== null && fragment !== ""))) invalid();
      return;
    }
    if (part.content != null) delta("message", part.content);
    if (part.reasoning_content != null || part.reasoning != null) delta("reasoning", part.reasoning_content ?? part.reasoning);
    if (part.reasoning_details != null) {
      if (!Array.isArray(part.reasoning_details) || part.reasoning_details.some(d => !d || typeof d !== "object" || Array.isArray(d))) invalid();
      retain(JSON.stringify(part.reasoning_details));
      reasoningDetails.push(...part.reasoning_details);
    }
    if (part.tool_calls != null) {
      if (!Array.isArray(part.tool_calls)) invalid();
      for (const fragment of part.tool_calls) {
        if (!Number.isSafeInteger(fragment.index) || fragment.index < 0 || fragment.index >= 1024
          || (fragment.type != null && fragment.type !== "function")) invalid();
        let call = calls.get(fragment.index);
        if (!call) { call = { type: "function", function: { name: "", arguments: "" } }; calls.set(fragment.index, call); }
        if (fragment.id != null) {
          if (typeof fragment.id !== "string" || !fragment.id || (call.id && call.id !== fragment.id)) invalid();
          call.id = fragment.id;
        }
        for (const field of ["name", "arguments"]) if (fragment.function?.[field] != null) {
          if (typeof fragment.function[field] !== "string") invalid();
          retain(fragment.function[field]);
          call.function[field] += fragment.function[field];
          if (call.function[field].length > 4 * 1024 * 1024) invalid();

        }
      }
    }
    if (choice.finish_reason != null) {
      if (!["stop", "tool_calls", "length", "content_filter"].includes(choice.finish_reason)) invalid();
      finishReason = choice.finish_reason;
    }
  };
  const native = async value => {
    if (!value || typeof value.type !== "string" || value.error) invalid();
    if (value.type === "response.output_item.added") {
      if (!Number.isSafeInteger(value.output_index) || value.output_index < 0 || value.output_index >= 1024 || nativeItems.has(value.output_index)
        || !value.item || typeof value.item.id !== "string" || !value.item.id || !["message", "reasoning", "function_call"].includes(value.item.type)
        || [...nativeItems.values()].some(item => item.id === value.item.id)
        || (value.item.type === "message" && value.item.role !== "assistant")) invalid();
      nativeItems.set(value.output_index, { ...value.item, streamedText: "", streamedArguments: "" });
    } else if (["response.output_text.delta", "response.reasoning_text.delta", "response.reasoning_summary_text.delta", "response.function_call_arguments.delta"].includes(value.type)) {
      const item = nativeItems.get(value.output_index);
      if (!item || value.item_id !== item.id || typeof value.delta !== "string") invalid();
      if (value.type === "response.function_call_arguments.delta") {
        if (item.type !== "function_call") invalid();
        retain(value.delta);
        item.streamedArguments += value.delta;
        if (item.streamedArguments.length > 4 * 1024 * 1024) invalid();

      } else {
        const kind = value.type === "response.output_text.delta" ? "message" : "reasoning";
        if (item.type !== kind || (value.content_index !== undefined && (!Number.isSafeInteger(value.content_index) || value.content_index < 0))
          || (value.summary_index !== undefined && (!Number.isSafeInteger(value.summary_index) || value.summary_index < 0))) invalid();
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
      if (value.response?.status !== value.type.slice("response.".length)) invalid();
      for (const [index, item] of nativeItems) {
        const final = value.response?.output?.[index];
        if (!final || item.id !== final.id || item.type !== final.type) invalid();
        if (item.streamedText) {
          const parts = item.type === "message" ? final.content : [...(final.summary ?? []), ...(final.content ?? [])];
          if (!Array.isArray(parts) || parts.map(part => part?.text).join("\n") !== item.streamedText) invalid();
        }
        if (item.type === "function_call" && (item.streamedArguments !== final.arguments
          || item.call_id !== final.call_id || item.name !== final.name)) invalid();
      }
      await complete(fromBindingResponsesResult(value.response, parallelToolCalls));
    } else if (!["response.created", "response.in_progress", "response.queued", "response.output_item.done",
      "response.content_part.added", "response.content_part.done", "response.output_text.done", "response.reasoning_text.done",
      "response.reasoning_summary_part.added", "response.reasoning_summary_part.done", "response.reasoning_summary_text.done",
      "response.function_call_arguments.done"].includes(value.type)) invalid();
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
          const base = normalize({ choices: [{ message: { content: "" }, finish_reason: "stop" }] }, true);
          emit("response.created", { response: { ...base, id, status: "in_progress", output: [], usage: null, end_turn: false } });
          return;
        }
        const before = sequence;
        do {
          const record = await iterator.next();
          if (settled) return;
          if (record.done) invalid();
          if (record.value.data === "[DONE]") {
            if (!["chat", "workers_ai_chat"].includes(source.format) || !finishReason) invalid();
            const tool_calls = [...calls].sort(([a], [b]) => a - b).map(([index, call], position) => {
              if (index !== position || !call.id) invalid();
              return call;
            });
            await complete({ choices: [{ message: { content: live.get("message")?.text ?? "",
              reasoning_content: live.get("reasoning")?.text ?? "", ...(reasoningDetails.length ? { reasoning_details: reasoningDetails } : {}), tool_calls }, finish_reason: finishReason }], usage });
          } else {
            const value = JSON.parse(record.value.data);
            if (source.format === "responses") {
              if (record.value.event && record.value.event !== value.type) invalid();
              await native(value);
            } else await chat(value);
          }
        } while (!settled && sequence === before);
      } catch (error) {
        if (settled) return;
        cancelReader();
        controller.error(new Error(error instanceof StreamReadError
          ? "Responses: provider stream read failed" : "Responses: invalid provider stream"));
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
