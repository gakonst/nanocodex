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
