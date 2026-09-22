// Direct port of utils/{string,output-truncation} at pinned Codex 36430b3688.
import { wavDuration } from "./code-values.mjs";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const tokens = (text) => Math.ceil(encoder.encode(text).length / 4);

export function truncateText(text, budget, formatted = false) {
  const bytes = encoder.encode(text);
  const byteBudget = budget * 4;
  if (bytes.length <= byteBudget) return text;
  let end = Math.floor(byteBudget / 2);
  let start = bytes.length - (byteBudget - end);
  while (end > 0 && (bytes[end] & 0xc0) === 0x80) end--;
  while (start < bytes.length && (bytes[start] & 0xc0) === 0x80) start++;
  const body = `${decoder.decode(bytes.subarray(0, end))}…${Math.ceil((bytes.length - byteBudget) / 4)} tokens truncated…${decoder.decode(bytes.subarray(start))}`;
  if (!formatted) return body;
  const lines = text.split("\n").length - Number(text.endsWith("\n"));
  return `Warning: truncated output (original token count: ${tokens(text)})\nTotal output lines: ${lines}\n\n${body}`;
}

export function limitCodeOutput(output, budget, estimateAudio = defaultAudioTokens) {
  if (typeof output === "string") {
    const split = output.indexOf("Output:\n");
    return split < 0 ? output : output.slice(0, split + 8) + truncateText(output.slice(split + 8), budget, true);
  }
  const [heading, ...items] = output;
  if (items.every((item) => item.type === "input_text")) {
    let combined = "";
    for (const item of items) combined += (combined ? "\n" : "") + item.text;
    return encoder.encode(combined).length <= budget * 4 ? output
      : [heading, { type: "input_text", text: truncateText(combined, budget, true) }];
  }
  const result = [heading];
  let remaining = budget, omittedText = 0, omittedAudio = 0;
  for (const item of items) {
    if (item.type === "input_text") {
      if (!item.text) continue;
      if (!remaining) { omittedText++; continue; }
      const cost = tokens(item.text);
      result.push({ ...item, text: cost <= remaining ? item.text : truncateText(item.text, remaining) });
      remaining = Math.max(0, remaining - cost);
    } else if (item.type === "input_audio") {
      const cost = estimateAudio(item.audio_url);
      if (cost <= remaining) { result.push(item); remaining -= cost; }
      else omittedAudio++;
    } else result.push(item);
  }
  if (omittedText) result.push({ type: "input_text", text: `[omitted ${omittedText} text items ...]` });
  if (omittedAudio) result.push({ type: "input_text", text: `[omitted ${omittedAudio} audio items ...]` });
  return result;
}

function defaultAudioTokens(url) {
  const duration = wavDuration(url);
  return duration === undefined ? tokens(url) : Math.ceil(duration * 10);
}
