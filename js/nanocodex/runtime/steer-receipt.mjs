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
      case "file": return { type: "file", file_data: item.file_data, ...(item.filename == null ? {} : { filename: item.filename }) };
      default: throw new TypeError("unsupported browser steering content");
    }
  });
  const bytes = new TextEncoder().encode(JSON.stringify({ instruction }));
  return [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}
