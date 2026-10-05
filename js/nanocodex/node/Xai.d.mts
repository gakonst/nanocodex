import type { Agent, Options } from '../runtime/xai.mjs';
export type { Agent, Auth, Options, Result, Tool, ToolContent, ToolResult, Turn } from '../runtime/xai.mjs';
/** Creates the actual Xai Rust/WASM backend with explicit host-owned authentication and tools. */
export function create(options: Options): Promise<Agent>;
