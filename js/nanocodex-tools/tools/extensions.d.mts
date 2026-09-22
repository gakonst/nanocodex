import type { NamedTool, ToolContext } from './types.mjs';
export const extensionSpecs: readonly Omit<NamedTool, 'handler'>[];
export function validateExtensionInput(name: string, input: unknown): unknown;
export interface MemoryFileStore {
  listFiles(): Promise<readonly string[]>;
  listDirectories?(): Promise<readonly string[]>;
  readFile(path: string): Promise<string>;
  createFile(path: string, content: string): Promise<void>;
}
export type ExtensionProvider = Record<string, (input: any, context?: ToolContext) => unknown | Promise<unknown>>;
export function fileMemoriesBackend(store: MemoryFileStore): ExtensionProvider;
export function truncateMemoryText(text: string, tokens: number): string;
export function extensionTools(options: { memories?: ExtensionProvider; authorize(name: string, context: ToolContext): void | Promise<void> }): NamedTool[];
