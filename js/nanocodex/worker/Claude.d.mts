import type { Agent, Options } from '../runtime/claude.mjs';
export type { Agent, Auth, Options, NativeToolResult, Tool, ToolContent, ToolResult } from '../runtime/claude.mjs';
/** Creates the actual Claude Rust/WASM backend with explicit host-owned authentication and tools. */
export function create(options: Options): Promise<Agent>;
