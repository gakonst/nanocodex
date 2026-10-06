export function initializeNativeEngine(): Promise<unknown>;
export type Definition = { name: string; description: string; input_schema: Record<string, unknown> };
export type FileSnapshot = { path: string; content?: string; size?: number; modified?: number };
export function fileSchemas(): Definition[];
export function filePlan(request: { root: string; name: string; input: unknown; files: FileSnapshot[]; directories?: string[]; visits?: number; prepare?: boolean }): { output: string; mutations: {path: string; content: string; before: string | null}[]; reads: string[] };
export function taskSchemas(): Definition[];
export function taskPlan(request: {name: string; input: unknown; checkpoint: unknown}): Promise<{output: string; checkpoint: unknown}>;
export function patchPaths(patch: string): string[];
export function rebasePatch(patch: string, mappings: Record<string, string>): string;
