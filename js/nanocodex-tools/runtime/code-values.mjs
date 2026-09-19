// Ported from openai/codex 36430b36881cf5c289cb48e671cfc9e8b542ae7b:
// code-mode-runtime/src/runtime/{value,audio}.rs. Keep these pure helpers in
// the guest realm as well, so validation and JSON exceptions are catchable.
// Serialize one closed scope: bundlers may rename functions and dependencies.
// Object method names stay stable even with minification and keepNames enabled.
function createValueHelpers() {
  const helpers = {
    stringify(value) {
      if (value === null || ["undefined", "boolean", "number", "bigint", "string"].includes(typeof value)) return String(value);
      return JSON.stringify(value) ?? String(value);
    },

    storeSnapshot(key, value) {
      key = `${key}`;
      const encoded = JSON.stringify(value);
      if (encoded === undefined) throw `Unable to store ${JSON.stringify(key)}. Only plain serializable objects can be stored.`;
      return [key, JSON.parse(encoded)];
    },

    normalizeImage(value, detail) {
      const expected = "image expects a non-empty image URL string, an object with image_url and optional detail, or a raw MCP image block";
      if (detail != null && typeof detail !== "string") throw "image detail must be a string when provided";
      let url;
      let embedded;
      if (typeof value === "string") url = value;
      else if (value !== null && typeof value === "object" && !Array.isArray(value)) {
        if (value.image_url !== undefined) {
          url = value.image_url;
          if (typeof url !== "string") throw expected;
          embedded = value.detail;
          if (embedded != null && typeof embedded !== "string") throw "image detail must be a string when provided";
        } else {
          const block = JSON.parse(JSON.stringify(value));
          if (typeof block?.type !== "string") throw expected;
          if (block.type !== "image") throw `image only accepts MCP image blocks, got "${block.type}"`;
          if (typeof block.data !== "string" || !block.data) throw "image expected MCP image data";
          const mime = block.mimeType ?? block.mime_type;
          url = /^data:/i.test(block.data) ? block.data
            : `data:${typeof mime === "string" && mime ? mime : "application/octet-stream"};base64,${block.data}`;
          const metadata = block._meta?.["codex/imageDetail"];
          if (["auto", "low", "high", "original"].includes(metadata)) embedded = metadata;
        }
      } else throw expected;
      if (!url) throw expected;
      if (/^https?:/i.test(url)) throw "Tool call failed: remote image URLs are not supported in tool outputs. Pass a base64 data URI instead";
      if (!/^data:/i.test(url)) throw "Tool call failed: invalid image output. Pass a base64 data URI instead";
      const selected = (detail ?? embedded ?? "high").replace(/[A-Z]/g, (c) => c.toLowerCase());
      if (!["auto", "low", "high", "original"].includes(selected)) throw "image detail must be one of: auto, low, high, original";
      return { type: "input_image", image_url: url, detail: selected };
    },

    normalizeAudio(value) {
      const expected = "audio expects a non-empty audio URL string, an object with audio_url, or a raw MCP audio block";
      let url;
      if (typeof value === "string") url = value;
      else if (value !== null && typeof value === "object" && !Array.isArray(value)) {
        if (value.audio_url !== undefined) {
          url = value.audio_url;
          if (typeof url !== "string") throw expected;
        } else {
          const block = JSON.parse(JSON.stringify(value));
          if (typeof block?.type !== "string") throw expected;
          if (block.type !== "audio") throw `audio only accepts MCP audio blocks, got "${block.type}"`;
          if (typeof block.data !== "string" || !block.data) throw "audio expected MCP audio data";
          const mime = block.mimeType ?? block.mime_type;
          url = /^data:/i.test(block.data) ? block.data
            : `data:${typeof mime === "string" && mime ? mime : "application/octet-stream"};base64,${block.data}`;
        }
      } else throw expected;
      if (!url) throw expected;
      if (!/^data:/i.test(url)) throw "Tool call failed: invalid audio output. Pass a base64 data URI instead";
      const duration = helpers.wavDuration(url);
      if (duration !== undefined && duration < 0.025) return {
        type: "input_text", text: "Audio output omitted because the clip is shorter than 25 ms; use a longer clip.",
      };
      return { type: "input_audio", audio_url: url };
    },

    generatedImageItems(value) {
      if (!value || typeof value !== "object" || Array.isArray(value)) throw "generatedImage expects an image generation result object";
      const hint = value.output_hint;
      if (hint !== undefined && typeof hint !== "string") throw "generatedImage output_hint must be a string when provided";
      return [helpers.normalizeImage(value), ...(hint === undefined ? [] : [{ type: "input_text", text: hint }])];
    },

    // Strict standard base64, including canonical padding bits, as in Rust base64.
    // This small decoder also runs inside QuickJS, which has no atob global.
    decodeAudio(url) {
      const comma = url.indexOf(",");
      if (comma < 0 || !url.slice(0, comma).split(";").slice(1).some((s) => s.toLowerCase() === "base64")) return;
      const data = url.slice(comma + 1);
      if (data.length > Math.ceil(50 * 1024 * 1024 / 3) * 4 || data.length % 4 || !/^[A-Za-z0-9+/]*={0,2}$/.test(data)) return;
      const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
      const padding = data.endsWith("==") ? 2 : data.endsWith("=") ? 1 : 0;
      const last = alphabet.indexOf(data[data.length - padding - 1]);
      if ((padding === 2 && (last & 15)) || (padding === 1 && (last & 3))) return;
      const bytes = new Uint8Array(data.length / 4 * 3 - padding);
      for (let input = 0, output = 0; input < data.length; input += 4) {
        const bits = (alphabet.indexOf(data[input]) << 18) | (alphabet.indexOf(data[input + 1]) << 12)
          | ((alphabet.indexOf(data[input + 2]) & 63) << 6) | (alphabet.indexOf(data[input + 3]) & 63);
        if (output < bytes.length) bytes[output++] = bits >> 16;
        if (output < bytes.length) bytes[output++] = bits >> 8;
        if (output < bytes.length) bytes[output++] = bits;
      }
      return bytes;
    },

    wavDuration(url) {
      const bytes = helpers.decodeAudio(url);
      if (!bytes || bytes.length < 12) return;
      const reader = { id(offset, size = 4) { return String.fromCharCode(...bytes.subarray(offset, offset + size)); } };
      if (reader.id(0) !== "RIFF" || reader.id(8) !== "WAVE") return;
      const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
      let rate, align;
      for (let offset = 12; offset + 8 <= bytes.length;) {
        const kind = reader.id(offset);
        const size = view.getUint32(offset + 4, true);
        const start = offset + 8;
        const length = Math.min(size, bytes.length - start);
        if (kind === "fmt ") {
          if (length < 14) return;
          let encoding = view.getUint16(start, true);
          if (encoding === 0xfffe) {
            if (length < 40 || ![0,0,0,0,16,0,128,0,0,170,0,56,155,113].every((b, i) => bytes[start + 26 + i] === b)) return;
            encoding = view.getUint16(start + 24, true);
          }
          if (encoding !== 1 && encoding !== 3) return;
          rate = view.getUint32(start + 4, true);
          align = view.getUint16(start + 12, true);
          if (!rate || !align) return;
        } else if (kind === "data") {
          if (!rate || !align) return;
          return Math.floor(length / align) / rate;
        }
        offset = start + size + size % 2;
      }
    },

  };
  return helpers;
}

export const { stringify, storeSnapshot, normalizeImage, normalizeAudio, generatedImageItems, decodeAudio, wavDuration } = createValueHelpers();

export function guestValueHelpers() {
  return `const { stringify, storeSnapshot, normalizeImage, normalizeAudio, generatedImageItems, decodeAudio, wavDuration } = (${createValueHelpers.toString()})();`;
}
