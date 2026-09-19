/** Fingerprint the browser Prompt representation accepted by Rust's serde contract. */
export async function steerInputKey(input) {
  const instruction = typeof input === "string" ? input : input.map((item) => {
    switch (item.type) {
      case "text": return { type: "text", text: item.text };
      case "image": return { type: "image", image_url: item.image_url, ...(item.detail == null ? {} : { detail: item.detail }) };
      case "audio": return { type: "audio", audio_url: item.audio_url };
      default: throw new TypeError("unsupported browser steering content");
    }
  });
  const bytes = new TextEncoder().encode(JSON.stringify({ instruction }));
  return [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}
