/** Bounded Responses SSE projection. Never retain a transcript or expose upstream errors. */
export function projectInferenceStream(body: ReadableStream<Uint8Array>,
  project: (event: Record<string, any>) => Record<string, any>, onToken: () => void): ReadableStream<Uint8Array> {
  const reader = body.getReader();
  const decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false });
  const encoder = new TextEncoder();
  let pending = "", ended = false, terminal = false;
  const failure = () => new Error("invalid_provider_protocol");
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      try {
        for (;;) {
          const boundary = /\r?\n\r?\n/.exec(pending);
          if (boundary) {
            const frame = pending.slice(0, boundary.index);
            pending = pending.slice(boundary.index + boundary[0].length);
            const data = frame.split(/\r?\n/).filter(line => line.startsWith("data:"))
              .map(line => line.slice(5).replace(/^ /, "")).join("\n");
            if (!data || data === "[DONE]") continue;
            if (terminal) throw failure();
            const event = JSON.parse(data);
            if (!event || typeof event !== "object" || typeof event.type !== "string"
              || event.type === "error" || event.type === "response.failed") throw failure();
            if ((event.type === "response.output_text.delta" || event.type === "response.function_call_arguments.delta"
              || event.type === "response.custom_tool_call_input.delta") && typeof event.delta === "string" && event.delta.trim().length) onToken();
            if (event.type === "response.completed" || event.type === "response.incomplete") {
              if (!event.response || typeof event.response !== "object" || Array.isArray(event.response)
                || event.response.object !== "response" || event.response.status !== event.type.slice(9)
                || !Array.isArray(event.response.output)) throw failure();
              terminal = true;
            }
            const output = project(event);
            controller.enqueue(encoder.encode(`event: ${output.type}\ndata: ${JSON.stringify(output)}\n\n`));
            return;
          }
          if (ended) {
            if (pending.trim() || !terminal) throw failure();
            reader.releaseLock(); controller.close(); return;
          }
          const next = await reader.read();
          ended = next.done;
          pending += decoder.decode(next.value, { stream: !next.done });
          if (pending.length > 2 * 1024 * 1024) throw failure();
        }
      } catch {
        await reader.cancel().catch(() => {});
        controller.error(failure());
      }
    },
    async cancel(reason) { await reader.cancel(reason); },
  });
}

/** Keep cancellation, timeout and session ownership alive until consumption ends. */
export function finalizeInferenceResponse(response: Response, signal: AbortSignal,
  finish: (successful: boolean) => void | Promise<void>): Response {
  if (!response.body) throw new Error("missing_response_body");
  const reader = response.body.getReader();
  let settled = false;
  let settlement: Promise<void> | undefined;
  let output: ReadableStreamDefaultController<Uint8Array>;
  const settle = (success: boolean, release?: () => Promise<void>): Promise<void> => {
    if (settled) return settlement ?? Promise.resolve();
    settled = true; signal.removeEventListener("abort", abort);
    settlement = (async () => {
      try { await release?.(); } finally { await finish(success); }
    })();
    return settlement;
  };
  const abort = () => {
    if (settled) return;
    // Do not propagate arbitrary abort messages across the public boundary.
    output.error(new Error("inference_cancelled"));
    void settle(false, () => reader.cancel()).catch(() => {});
  };
  const body = new ReadableStream<Uint8Array>({
    start(controller) { output = controller; signal.addEventListener("abort", abort, { once: true }); if (signal.aborted) abort(); },
    async pull(controller) {
      try {
        const next = await reader.read();
        if (settled) return;
        if (next.done) { await settle(true); controller.close(); }
        else controller.enqueue(next.value);
      } catch { await settle(false); controller.error(new Error("inference_failed")); }
    },
    async cancel() { await settle(false, () => reader.cancel()); },
  });
  return new Response(body, { status: response.status, headers: response.headers });
}
