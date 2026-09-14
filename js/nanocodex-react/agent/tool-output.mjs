const MAX_ITEMS = 64;
const MAX_TEXT = 64 * 1024;
const MAX_INLINE = 28 * 1024 * 1024;
const MAX_DEPTH = 10;

/** Project model-visible code output and MCP resources without choosing a transport. */
export function projectToolOutput(...values) {
  const output = [];
  const keys = new Set();
  const visited = new WeakSet();
  let remaining = 1024;
  let retainedChars = 0;
  function add(item) {
    if (output.length >= MAX_ITEMS) return;
    const key = item.kind === "text" ? `text:${item.text}` : `${item.kind}:${item.url}`;
    const size = item.kind === "text" ? item.text.length : item.url.length;
    if (!keys.has(key) && retainedChars + size <= MAX_INLINE * 2) {
      keys.add(key); output.push(item); retainedChars += size;
    }
  }
  function text(value) {
    if (typeof value !== "string" || !value.trim()) return;
    // This is execution status, already represented by the activity row.
    if (/^Script completed\nWall time [\d.]+ seconds(?:\nOutput:)?\s*$/.test(value)) return;
    if (value.startsWith("data:")) return;
    add({ kind: "text", text: bounded(value, MAX_TEXT) });
  }
  function media(kind, source, value = {}) {
    const url = generatedOutputUrl(source, kind);
    if (!url) return false;
    const mimeType = mime(value) || (url.startsWith("data:") ? url.slice(5).split(/[;,]/, 1)[0] : undefined);
    const name = typeof value.name === "string" ? value.name : typeof value.title === "string" ? value.title : undefined;
    add({ kind, url, ...(name ? { name: bounded(name, 240) } : {}), ...(mimeType ? { mimeType } : {}) });
    return true;
  }
  function visit(value, depth = 0, emittedText = false) {
    if (remaining-- <= 0 || depth > MAX_DEPTH || output.length >= MAX_ITEMS) return true;
    if (typeof value === "string") {
      if (value.length > MAX_INLINE) return true;
      const parsed = decoded(value);
      if (parsed !== value) {
        const recognized = visit(parsed, depth + 1);
        if (emittedText && !recognized) text("```json\n" + formatToolOutput(parsed) + "\n```");
        return recognized || emittedText;
      }
      text(value);
      return true;
    }
    if (!value || typeof value !== "object") return false;
    if (visited.has(value)) return true;
    visited.add(value);
    if (Array.isArray(value)) {
      let recognized = false;
      for (const child of value.slice(0, MAX_ITEMS)) recognized = visit(child, depth + 1) || recognized;
      return recognized;
    }
    const type = value.type;
    if (type === "input_text" || type === "text" || type === "output_text") {
      visit(value.text, depth + 1, true);
      return true;
    }
    const image = sourceUrl(value.image_url) ?? (typeof value.imageUrl === "string" ? value.imageUrl : undefined);
    const audio = sourceUrl(value.audio_url);
    const video = sourceUrl(value.video_url);
    if (image) media("image", image, value);
    if (audio) media("audio", audio, value);
    if (video) media("video", video, value);
    if (["image", "input_image", "output_image", "image_url", "audio", "input_audio", "output_audio", "video", "input_video", "output_video"].includes(type)) {
      const kind = type.includes("image") ? "image" : type.includes("audio") ? "audio" : "video";
      const content = value.input_audio && typeof value.input_audio === "object" ? value.input_audio : value;
      const mediaType = mime(content) || (kind === "audio" && typeof content.format === "string"
        ? ({ mp3: "audio/mpeg", wav: "audio/wav", pcm16: "audio/pcm" })[content.format] : undefined);
      if (typeof content.data === "string" && mediaType) media(kind, `data:${mediaType};base64,${content.data}`, { ...value, mimeType: mediaType });
      else if (typeof value.url === "string") media(kind, value.url, value);
      return true;
    }
    if (type === "resource_link" || type === "file" || type === "input_file" || type === "output_file") {
      const source = sourceUrl(value.file_url) ?? value.uri ?? value.url ?? value.file_data;
      if (!media(mediaKind(mime(value)), source, value)) {
        text(`${value.name || value.title || "Generated file"}${typeof source === "string" && !source.startsWith("data:") ? ` — ${bounded(source, 1024)}` : " — preview unavailable"}`);
      }
      return true;
    }
    if (type === "resource" && value.resource) {
      visitResource(value.resource);
      return true;
    }
    if (mime(value) && (value.blob !== undefined || value.uri !== undefined || value.url !== undefined || value.text !== undefined)) { visitResource(value); return true; }
    if (image || audio || video) {
      if (typeof value.output_hint === "string") text(value.output_hint);
      return true;
    }
    let recognized = false;
    for (const key of ["content", "output", "result", "structured_result", "structuredContent", "outputs", "attachments", "artifacts", "files", "images"]) {
      if (value[key] !== undefined) recognized = visit(value[key], depth + 1) || recognized;
    }
    return recognized;
  }
  function visitResource(value) {
    if (!value || typeof value !== "object") return;
    const mediaType = mime(value) || "application/octet-stream";
    const name = value.name || value.title || (typeof value.uri === "string" ? value.uri.split("/").at(-1) : undefined);
    const metadata = { ...value, name, mimeType: mediaType };
    if (typeof value.blob === "string") media(mediaKind(mediaType), `data:${mediaType};base64,${value.blob}`, metadata);
    else if (typeof value.text === "string") {
      if (["text/plain", "text/markdown"].includes(mediaType)) text(value.text);
      const wellFormed = new TextDecoder().decode(new TextEncoder().encode(value.text));
      if (wellFormed.length > MAX_INLINE || !media("file", `data:${mediaType};charset=utf-8,${encodeURIComponent(wellFormed)}`, metadata)) {
        text(`${name || "Generated file"} — too large to preview`);
      }
    } else if (!media(mediaKind(mediaType), value.uri ?? value.url, metadata)) text(`${name || "Generated resource"} — preview unavailable`);
  }
  for (const value of values) visit(value);
  return output;
}

/** Shared renderer URL policy; local paths never become requests to an unrelated host. */
export function generatedOutputUrl(value, kind = "file") {
  if (typeof value !== "string" || !value || value.length > MAX_INLINE || /[\u0000-\u001f]/.test(value)) return undefined;
  if (value.startsWith("data:")) {
    const match = /^data:([a-z0-9.+-]+\/[a-z0-9.+-]+)(;charset=[a-z0-9-]+)?(;base64)?,([\s\S]*)$/i.exec(value);
    if (!match || (kind !== "file" && !match[1].toLowerCase().startsWith(`${kind}/`))) return undefined;
    if (match[3] && !/^[A-Za-z0-9+/]*={0,2}$/.test(match[4])) return undefined;
    return value;
  }
  try {
    const url = new URL(value);
    if (!["https:", "http:"].includes(url.protocol) || url.username || url.password) return undefined;
    return url.href;
  } catch { return undefined; }
}

/** Keep diagnostics readable and bounded without dumping embedded binary payloads. */
export function formatToolOutput(value) {
  let remaining = 1024;
  const visited = new WeakSet();
  function clean(value, depth = 0) {
    if (remaining-- <= 0 || depth > MAX_DEPTH) return "[More output]";
    if (typeof value === "string") {
      if (value.startsWith("data:")) return "[Embedded attachment]";
      const parsed = decoded(value);
      return parsed === value ? bounded(value.replace(/data:[^\s)\]"<>]+/gi, "[Embedded attachment]"), MAX_TEXT) : clean(parsed, depth + 1);
    }
    if (!value || typeof value !== "object") return value;
    if (visited.has(value)) return "[Repeated output]";
    visited.add(value);
    if (Array.isArray(value)) return value.slice(0, MAX_ITEMS).map(child => clean(child, depth + 1));
    return Object.fromEntries(Object.entries(value).slice(0, MAX_ITEMS).map(([key, child]) => [key,
      key === "blob" || key === "file_data" || (key === "data" && (mime(value) || ["image", "input_image", "output_image", "audio", "input_audio", "output_audio", "video", "input_video", "output_video"].includes(value.type) || value.format))
        ? "[Embedded attachment]" : clean(child, depth + 1),
    ]));
  }
  const cleaned = clean(value);
  return bounded(typeof cleaned === "string" ? cleaned : JSON.stringify(cleaned, null, 2) ?? "", MAX_TEXT);
}

function decoded(value) {
  if (typeof value !== "string" || value.length > MAX_INLINE || !/^[\s]*[\[{]/.test(value)) return value;
  try { return JSON.parse(value); } catch { return value; }
}
function sourceUrl(value) { return typeof value === "string" ? value : value && typeof value.url === "string" ? value.url : undefined; }
function mime(value) { return [value.mimeType, value.mime_type, value.media_type].find(item => typeof item === "string" && /^[a-z0-9.+-]+\/[a-z0-9.+-]+$/i.test(item)); }
function mediaKind(mimeType) { return ["image", "audio", "video"].find(kind => mimeType?.startsWith(`${kind}/`)) ?? "file"; }
function bounded(value, max) { return value.length > max ? value.slice(0, max) + "\n…" : value; }
