import { createClaude } from '../runtime/claude.mjs';
import { initializeBrowserEngine } from './engine.mjs';

/** Runs inline in this browser isolate; not the Codex Agent module Worker. */
export function create(options) {
  return createClaude(options, async (module) => {
    const wasm = await import('../pkg-web/nanocodex.js');
    await initializeBrowserEngine({ module });
    return wasm.Nanocodex;
  }, 'browser');
}
