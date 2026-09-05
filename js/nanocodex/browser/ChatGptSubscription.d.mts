import type {
  ChatGptSubscriptionHandle,
  ChatGptSubscriptionOptions,
} from "../types.mjs";

/** Opens the Rust-owned ChatGPT device-login and credential lifecycle. */
export function open(options: ChatGptSubscriptionOptions): Promise<ChatGptSubscriptionHandle>;
