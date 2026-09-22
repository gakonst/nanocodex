import { describe, it, expect, vi } from "vitest";
import { projectInferenceStream, finalizeInferenceResponse } from "../src/inference-stream";
const encoder = new TextEncoder();
const frame = (value: unknown) => encoder.encode(`data: ${JSON.stringify(value)}\r\n\r\n`);

describe("incremental Responses projection", () => {
  it("handles split UTF8 and CRLF and delivers output before completion", async () => {
    let source!: ReadableStreamDefaultController<Uint8Array>;
    const upstream = new ReadableStream<Uint8Array>({ start(c) { source = c; } });
    const token = vi.fn();
    const reader = projectInferenceStream(upstream, e => ({ ...e, fixture: true }), token).getReader();
    const bytes = frame({ type: "response.output_text.delta", delta: "λ" });
    const index = bytes.indexOf(0xce);
    source.enqueue(bytes.slice(0, index + 1)); source.enqueue(bytes.slice(index + 1, bytes.length - 1));
    source.enqueue(bytes.slice(-1));
    expect(new TextDecoder().decode((await reader.read()).value)).toContain('"delta":"λ"');
    expect(token).toHaveBeenCalledTimes(1);
    source.enqueue(frame({ type: "response.completed", response: { object: "response", status: "completed", output: [] } })); source.close();
    expect(new TextDecoder().decode((await reader.read()).value)).toContain('"fixture":true');
    expect((await reader.read()).done).toBe(true);
  });
  it.each(["truncated", "provider_error", "malformed", "too_large"])("fails sanitized on %s", async kind => {
    const upstream = new ReadableStream<Uint8Array>({ start(c) {
      c.enqueue(kind === "provider_error" ? frame({ type: "error", message: "private upstream secret" })
        : encoder.encode(kind === "malformed" ? "data: {private-secret}\n\n"
          : kind === "too_large" ? "x".repeat(2 * 1024 * 1024 + 1) : "data: {}"));
      c.close();
    } });
    await expect(new Response(projectInferenceStream(upstream, e => e, () => {})).text()).rejects.toThrow("invalid_provider_protocol");
  });
  it("keeps lifetime active after headers and finalizes cancellation only once", async () => {
    const cancel = vi.fn(), finish = vi.fn();
    const signal = new AbortController();
    const wrapped = finalizeInferenceResponse(new Response(new ReadableStream({ cancel })), signal.signal, finish);
    expect(finish).not.toHaveBeenCalled();
    await wrapped.body!.cancel(); signal.abort();
    expect(cancel).toHaveBeenCalledTimes(1); expect(finish).toHaveBeenCalledExactlyOnceWith(false);
  });
  it("an abort errors a pending read and finishes even without upstream output", async () => {
    const cancel = vi.fn(), finish = vi.fn();
    const controller = new AbortController();
    const wrapped = finalizeInferenceResponse(new Response(new ReadableStream({ cancel })), controller.signal, finish);
    const pending = wrapped.body!.getReader().read(); controller.abort(new Error("private reason"));
    await expect(pending).rejects.toThrow("inference_cancelled");
    await vi.waitFor(() => expect(finish).toHaveBeenCalledExactlyOnceWith(false));
    expect(cancel).toHaveBeenCalledTimes(1);
  });
});

 it.each([undefined, { status: "completed" }, { object: "response", status: "incomplete", output: [] }])("rejects malformed terminal response %s", async response => {
   const upstream = new ReadableStream<Uint8Array>({ start(c) { c.enqueue(frame({type:"response.completed", response})); c.close(); } });
   await expect(new Response(projectInferenceStream(upstream, e => e, () => {})).text()).rejects.toThrow("invalid_provider_protocol");
 });

it.each(["kimi-k3", "mimo-v2.6-pro", "@cf/zai-org/glm-5.3"] as const)("public %s inference forwards visible reasoning and answer before provider completion", async model => {
  const { createGatewayResponses } = await import("nanocodex/cloudflare/gateway-responses");
  let source!: ReadableStreamDefaultController<Uint8Array>;
  const upstream = new ReadableStream<Uint8Array>({ start(controller) { source = controller; } });
  const transport = createGatewayResponses({ provider: "openrouter", model, reasoningEffort: "low", apiKey: "synthetic-key",
    fetch: async (_url, init) => {
      expect(JSON.parse(init!.body as string).stream).toBe(true);
      return new Response(upstream, { headers: { "content-type": "text/event-stream" } });
    } });
  const response = await transport.createResponse(`${transport.apiBaseUrl}/responses`, "fixture", {
    authorization: "host_managed", signal: new AbortController().signal, body: JSON.stringify({ input: "Inspect fixture", stream: true }) });
  const token = vi.fn(), finish = vi.fn(), abort = new AbortController();
  const reader = finalizeInferenceResponse(new Response(projectInferenceStream(response.body!, event => ({ ...event, fixture: true }), token)),
    abort.signal, finish).body!.getReader();
  const until = async (type: string) => {
    for (;;) {
      const next = await reader.read();
      expect(next.done).toBe(false);
      const event = JSON.parse(new TextDecoder().decode(next.value).split("\ndata: ")[1]);
      expect(event.fixture).toBe(true);
      if (event.type === type) return event;
    }
  };
  const send = (delta: object, finish_reason: string | null = null) => source.enqueue(frame({ choices: [{ index: 0, delta, finish_reason }] }));
  try {
    await until("response.created");
    send({ reasoning_details: [{ type: "reasoning.text", text: "Inspect fixture" }] });
    expect((await until("response.reasoning_text.delta")).delta).toBe("Inspect fixture");
    expect(token).not.toHaveBeenCalled();
    expect(finish).not.toHaveBeenCalled();
    send({ content: "Answer" });
    expect((await until("response.output_text.delta")).delta).toBe("Answer");
    expect(token).toHaveBeenCalledOnce();
    expect(finish).not.toHaveBeenCalled();
    send({}, "stop"); source.enqueue(encoder.encode("data: [DONE]\r\n\r\n"));
    await until("response.completed");
    expect((await reader.read()).done).toBe(true);
    expect(finish).toHaveBeenCalledExactlyOnceWith(true);
  } finally { await reader.cancel(); }
}, 2_000);
