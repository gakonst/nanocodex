import type { PluginConfig } from "@cloudflare/vite-plugin";
import type { PluginOption } from "vite";

import type { NanocodexChatGptViteOptions, NanocodexViteOptions } from "./index.mjs";

export type NanocodexCloudflareViteOptions = Readonly<{
  /** Cloudflare Vite plugin options. Nanocodex adds only exact development credential bindings. */
  cloudflare?: PluginConfig | undefined;
  /** Local ChatGPT subscription support is on by default; pass false to disable it. */
  chatGpt?: Pick<NanocodexChatGptViteOptions, "authFile"> | false | undefined;
  /** Generate and continuously reconcile a review-first WebMCP manifest. */
  webMcp?: NanocodexViteOptions["webMcp"];
}>;

/** One call installs browser shims, local subscription brokering, and the Cloudflare Worker plugin. */
export function nanocodex(options?: NanocodexCloudflareViteOptions): PluginOption[];
