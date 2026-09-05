import { createNanocodexVitePlugin } from "./plugin.mjs";

/** Browser compatibility plus a same-origin local ChatGPT endpoint for ordinary Vite apps. */
export function nanocodex(options = {}) {
  return createNanocodexVitePlugin(options, { target: "vite" });
}
