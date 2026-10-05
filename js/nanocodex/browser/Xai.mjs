import { createXai } from '../runtime/xai.mjs';
import { initializeBrowserEngine } from './engine.mjs';

/** Runs inline in this browser isolate; not the Codex Agent module Worker. */
export function create(options) {
  return createXai(options, async (module) => {
    const wasm = await import('../pkg-web/nanocodex.js');
    await initializeBrowserEngine({ module });
    return wasm.Nanoxai;
  }, 'browser');
}
