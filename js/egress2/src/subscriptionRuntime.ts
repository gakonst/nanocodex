import { ChatGptSubscription, type ChatGptSubscriptionHandle, type ChatGptSubscriptionOptions } from "nanocodex/worker";
import wasmModule from "../../nanocodex/pkg-web/nanocodex_bg.wasm?module";

/** Standard Rust-owned JS/WASM subscription lifecycle, with a Cloudflare module. */
export function openChatGptSubscription(
  options: Omit<ChatGptSubscriptionOptions, "module">,
): Promise<ChatGptSubscriptionHandle> {
  return ChatGptSubscription.open({ ...options, module: wasmModule });
}
