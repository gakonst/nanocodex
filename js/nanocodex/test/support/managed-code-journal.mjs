// Load the shipped managed journal in Node without requiring global TypeScript
// transform flags. Only this fixture's module graph uses the temporary hooks.
import { readFileSync } from 'node:fs';
import { registerHooks } from 'node:module';
import { transformSync } from 'esbuild';
const hooks = registerHooks({
  resolve(specifier, context, next) {
    try { return next(specifier, context); }
    catch (error) {
      if (specifier.startsWith('.') && !/\.[a-z]+$/.test(specifier)) return next(specifier + '.ts', context);
      throw error;
    }
  },
  load(url, context, next) {
    if (url.startsWith('file:') && url.endsWith('.ts')) return {
      format: 'module', shortCircuit: true,
      source: transformSync(readFileSync(new URL(url), 'utf8'), {loader:'ts',format:'esm',target:'node22'}).code,
    };
    return next(url, context);
  },
});
let journal;
try { journal = await import('../../../managed/src/managed-recovery-safety.ts'); }
finally { hooks.deregister(); }
export const createManagedCodeEffectJournal = journal.createManagedCodeEffectJournal;
