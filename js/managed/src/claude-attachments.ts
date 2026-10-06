import type { PromptInput } from "nanocodex";

/** iOS/macOS clients upload originals and send this text descriptor (InboxCore ImageAttachmentContent). */
const IMAGE_ATTACHMENT_PREFIX = "Attached original image file.\n[Image attachment]\n";
const PREVIEW_PATH = /^\/brain\/attachments\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\/preview\.jpg$/;
/** Anthropic's per-image limit; client previews are bounded to 2 MiB. */
export const CLAUDE_INLINE_PREVIEW_MAX_BYTES = 5 * 1024 * 1024;
const MAX_INLINE_PREVIEWS = 20;
/** Stays below the Claude prompt's 20 MiB combined media bound (with documents). */
const MAX_INLINE_TOTAL_BYTES = 16 * 1024 * 1024;

type Item = Exclude<PromptInput, string>[number];

/**
 * Claude has no `view_image` tool, so uploaded image attachments would only
 * be visible as paths. Inline each brain-hosted attachment's bounded JPEG
 * preview as a native image block right after its descriptor. Phone-local
 * attachments (hand_id) and missing/oversized previews keep the text only.
 */
export async function inlineClaudeAttachmentPreviews(
  input: readonly Item[],
  load: (relativePath: string) => Promise<Uint8Array | undefined>,
): Promise<Item[]> {
  let count = input.filter((item) => item.type === "image").length;
  let total = 0;
  const output: Item[] = [];
  for (const item of input) {
    output.push(item);
    if (item.type !== "text" || !item.text.startsWith(IMAGE_ATTACHMENT_PREFIX)) continue;
    if (count >= MAX_INLINE_PREVIEWS) continue;
    let header: Record<string, unknown>;
    try {
      const line = item.text.slice(IMAGE_ATTACHMENT_PREFIX.length).split("\n", 1)[0] ?? "";
      const parsed: unknown = JSON.parse(line);
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) continue;
      header = parsed as Record<string, unknown>;
    } catch { continue; }
    if (header.hand_id !== undefined || typeof header.preview_path !== "string") continue;
    const match = PREVIEW_PATH.exec(header.preview_path);
    if (!match) continue;
    const bytes = await load(`attachments/${match[1]}/preview.jpg`);
    if (!bytes || bytes.byteLength === 0 || bytes.byteLength > CLAUDE_INLINE_PREVIEW_MAX_BYTES
      || total + bytes.byteLength > MAX_INLINE_TOTAL_BYTES
      || bytes[0] !== 0xff || bytes[1] !== 0xd8 || bytes[2] !== 0xff) continue;
    total += bytes.byteLength;
    count += 1;
    output.push({ type: "image", image_url: `data:image/jpeg;base64,${base64(bytes)}` });
  }
  return output;
}

function base64(bytes: Uint8Array): string {
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
  }
  return btoa(binary);
}
