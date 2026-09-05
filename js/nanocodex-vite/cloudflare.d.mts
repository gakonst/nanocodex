import type { PluginConfig } from "@cloudflare/vite-plugin";
import type { PluginOption } from "vite";

import type { NanocodexChatGptViteOptions, NanocodexDevApplication } from "./index.mjs";

export type NanocodexCloudflareViteOptions = Readonly<{
  /** Cloudflare Vite plugin options. Nanocodex adds only exact development credential bindings. */
  cloudflare?: PluginConfig | undefined;
  /** Local ChatGPT subscription support is on by default; pass false to disable it. */
  chatGpt?: (Pick<NanocodexChatGptViteOptions, "authFile"> & Readonly<{
    /** Put the local Codex credential only in this auxiliary broker Worker. */
    credentialBrokerWorker?: string | undefined;
  }>) | false | undefined;
  /** Sibling Vite applications mounted into this development server. */
  devApplications?: readonly NanocodexDevApplication[] | undefined;
  /** Start the fixed local OAuth callback relay while serving. */
  oauthRelay?: boolean | undefined;
}>;

/** One call installs browser shims, local subscription brokering, and the Cloudflare Worker plugin. */
export function nanocodex(options?: NanocodexCloudflareViteOptions): PluginOption[];
