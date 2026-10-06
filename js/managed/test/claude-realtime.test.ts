import { describe, expect, it } from "vitest";
import {
  CLAUDE_PENDING_CONTEXT_MAX_BYTES, CLAUDE_PENDING_CONTEXT_MAX_ENTRIES, CLAUDE_REALTIME_START,
  boundedClaudeContext, claudeRealtimeContext, prependClaudeContext,
} from "../src/claude-realtime";
import { inlineClaudeAttachmentPreviews } from "../src/claude-attachments";
import { validatePromptInput } from "../src/protocol";

describe("Claude realtime context", () => {
  it("prepends queued context to text and keeps attachment order", () => {
    expect(prependClaudeContext("hello", [])).toBe("hello");
    const text = prependClaudeContext("hello", [CLAUDE_REALTIME_START, "transcript"]);
    expect(text.startsWith("<session_context>")).toBe(true);
    expect(text).toContain("Realtime conversation started");
    expect(text.endsWith("\n\nhello")).toBe(true);
    const content = prependClaudeContext([{ type: "text", text: "q" }, { type: "image", image_url: "https://example.com/a.png" }], ["ctx"]);
    expect(content.map((item) => item.type)).toEqual(["text", "text", "image"]);
    expect(JSON.stringify(content[0])).toContain("ctx");
  });

  it("bounds queued context by entries and bytes, keeping the newest", () => {
    const many = Array.from({ length: CLAUDE_PENDING_CONTEXT_MAX_ENTRIES + 4 }, (_, index) => `entry-${index}`);
    const bounded = boundedClaudeContext(many);
    expect(bounded).toHaveLength(CLAUDE_PENDING_CONTEXT_MAX_ENTRIES);
    expect(bounded.at(-1)).toBe(many.at(-1));
    const large = "x".repeat(CLAUDE_PENDING_CONTEXT_MAX_BYTES / 2 + 1);
    expect(boundedClaudeContext([large, large, "newest"])).toEqual([large, "newest"]);
  });

  it("projects completed turns as text-only Responses history", () => {
    const context = claudeRealtimeContext([{ user: "first", assistant: "answer" }, { user: "second" }, { user: "  " }]);
    expect(context.workspace).toBe("/brain");
    expect(context.history).toEqual([
      { type: "message", role: "user", content: [{ type: "input_text", text: "first" }] },
      { type: "message", role: "assistant", content: [{ type: "output_text", text: "answer" }] },
      { type: "message", role: "user", content: [{ type: "input_text", text: "second" }] },
    ]);
    const clipped = claudeRealtimeContext([{ user: "é".repeat(5000) }]).history[0] as { content: { text: string }[] };
    expect(new TextEncoder().encode(clipped.content[0]!.text).byteLength).toBeLessThanOrEqual(4 * 1024 + 3);
  });
});

describe("Claude attachments", () => {
  it("validates inline PDF and text documents", () => {
    const pdf = `data:application/pdf;base64,${btoa("%PDF-1.4\n")}`;
    expect(() => validatePromptInput([{ type: "file", file_data: pdf, filename: "brief.pdf" }])).not.toThrow();
    expect(() => validatePromptInput([{ type: "file", file_data: `data:text/plain;base64,${btoa("hi")}` }])).not.toThrow();
    expect(() => validatePromptInput([{ type: "file", file_data: "data:application/zip;base64,UEsDBA==" }])).toThrow(/application\/pdf/);
    expect(() => validatePromptInput([{ type: "file", file_data: pdf, filename: "../etc/passwd" }])).toThrow(/filename/);
    expect(() => validatePromptInput([{ type: "file", file_data: pdf, extra: true }])).toThrow();
  });

  it("inlines brain-hosted JPEG previews after their descriptors", async () => {
    const id = "01234567-89ab-cdef-0123-456789abcdef";
    const descriptor = `Attached original image file.\n[Image attachment]\n${JSON.stringify({ preview_path: `/brain/attachments/${id}/preview.jpg` })}\nmore`;
    const phone = `Attached original image file.\n[Image attachment]\n${JSON.stringify({ hand_id: "phone", preview_path: `/brain/attachments/${id}/preview.jpg` })}`;
    const jpeg = new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 1, 2]);
    const loaded: string[] = [];
    const output = await inlineClaudeAttachmentPreviews(
      [{ type: "text", text: descriptor }, { type: "text", text: phone }],
      async (path) => { loaded.push(path); return jpeg; },
    );
    expect(loaded).toEqual([`attachments/${id}/preview.jpg`]);
    expect(output.map((item) => item.type)).toEqual(["text", "image", "text"]);
    expect(output[1]).toEqual({ type: "image", image_url: `data:image/jpeg;base64,${btoa(String.fromCharCode(...jpeg))}` });
    const notJpeg = await inlineClaudeAttachmentPreviews([{ type: "text", text: descriptor }], async () => new Uint8Array([1, 2, 3]));
    expect(notJpeg.map((item) => item.type)).toEqual(["text"]);
  });
});
