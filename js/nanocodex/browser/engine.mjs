import init from "../pkg-web/nanocodex.js";

let initialized;
let engine;

/** @internal Initializes the browser WASM module once per realm. */
export function initializeBrowserEngine(options = {}) {
  return initialized ||= (options.module === undefined
    ? init()
    : init({ module_or_path: options.module })).then((exports) => (engine = exports)).catch((error) => {
      initialized = undefined;
      throw error;
    });
}

/** @internal Current linear memory of this realm's engine. Every agent in the
 * realm shares it, and WASM memory never shrinks after growth. */
export function engineMemoryBytes() {
  return engine?.memory?.buffer?.byteLength;
}
