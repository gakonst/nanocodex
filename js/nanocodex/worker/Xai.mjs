import { createXai } from '../runtime/xai.mjs';
import { initializeBrowserEngine } from '../browser/engine.mjs';

/** Runs the actual Xai WASM backend in the current Worker isolate. */
export function create(options) {
  return createXai(options, async (module) => {
    const wasm = await import('../pkg-web/nanocodex.js');
    await initializeBrowserEngine({ module });
    return wasm.Nanoxai;
  }, 'worker');
}
