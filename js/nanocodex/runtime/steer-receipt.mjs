/** Fingerprint the browser Prompt representation accepted by Rust's serde contract. */
export async function steerInputKey(input) {
  const instruction = typeof input === "string" ? input : input.map((item) => {
    switch (item.type) {
      case "text": return { type: "text", text: item.text };
      case "image": {
        if ((item.file_id !== undefined) === (item.image_url !== undefined)
          || (item.file_id !== undefined && (typeof item.file_id !== "string" || !/^[A-Za-z0-9_-]{1,512}$/.test(item.file_id)))) {
          throw new TypeError("image steering content requires exactly one valid image_url or file_id");
        }
        return { type: "image", ...(item.file_id === undefined ? { image_url: item.image_url } : { file_id: item.file_id }), ...(item.detail == null ? {} : { detail: item.detail }) };
      }
      case "audio": return { type: "audio", audio_url: item.audio_url };
      default: throw new TypeError("unsupported browser steering content");
    }
  });
  const bytes = new TextEncoder().encode(JSON.stringify({ instruction }));
  return [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** Fingerprint the native host-only completion Prompt for durable receipt reconciliation. */
export async function asyncCompletionInputKey(completion) {
  const output = typeof completion.output === "string" ? completion.output : completion.output.map(item => {
    switch (item.type) {
      case "input_text": return { type: "input_text", text: item.text };
      case "input_image": {
        if ((item.file_id !== undefined) === (item.image_url !== undefined))
          throw new TypeError("completion image requires exactly one reference");
        if (!["auto", "low", "high", "original"].includes(item.detail))
          throw new TypeError("completion image requires a supported detail");
        return { type: "input_image", ...(item.file_id === undefined
          ? { image_url: item.image_url } : { file_id: item.file_id }), detail: item.detail };
      }
      case "input_audio": return { type: "input_audio", audio_url: item.audio_url };
      case "encrypted_content": return { type: "encrypted_content", encrypted_content: item.encrypted_content };
      default: throw new TypeError("unsupported completion content");
    }
  });
  const prompt = { instruction: "", async_completion: {
    delivery_id: completion.delivery_id, job_id: completion.job_id,
    original_call_id: completion.original_call_id, output,
  } };
  const bytes = new TextEncoder().encode(JSON.stringify(prompt));
  return [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))]
    .map(byte => byte.toString(16).padStart(2, "0")).join("");
}
