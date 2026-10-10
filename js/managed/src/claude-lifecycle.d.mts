import type { Agent } from '../../nanocodex/runtime/claude.mjs';
export function observeClaudeRelease(agent: Pick<Agent, "uid" | "sessionId">, listener: () => void): () => void;
/** Shared WASM linear memory of this isolate, when the engine is initialized. */
export function engineMemoryBytes(): number | undefined;
