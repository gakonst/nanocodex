import init from "../pkg-web/nanocodex.js";

let initialized;

/** @internal Initializes the browser WASM module once per realm. */
export function initializeBrowserEngine(options = {}) {
  return initialized ||= (options.module === undefined
    ? init()
    : init({ module_or_path: options.module })).catch((error) => {
      initialized = undefined;
      throw error;
    });
}
