import { createClaude } from '../runtime/claude.mjs';
import { initializeBrowserEngine } from '../browser/engine.mjs';

/** Runs the actual Claude WASM backend in the current Worker isolate. */
export function create(options) {
  return createClaude(options, async (module) => {
    const wasm = await import('../pkg-web/nanocodex.js');
    await initializeBrowserEngine({ module });
    return wasm.Nanocodex;
  }, 'worker');
}
