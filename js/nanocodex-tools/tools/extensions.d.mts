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
export type ListedSkill = { authority: { kind: 'orchestrator' } | { kind: 'executor'; id: string }; package: string; name: string; description: string; main_resource: string };
export interface SkillProvider {
  list(authority: { kind: 'orchestrator' | 'executor' }): Promise<{ skills: ListedSkill[]; warnings?: string[] }>;
  read(input: { package: string; resource?: string | null; cursor?: string | null }): Promise<{ resource: string; contents: string; skill_root?: string }>;
}
export function skillsBackend(provider: SkillProvider, byteBudget?: number): ExtensionProvider;
export function extensionTools(options: { skills?: ExtensionProvider; memories?: ExtensionProvider; goals?: ExtensionProvider; authorize(name: string, context: ToolContext): void | Promise<void> }): NamedTool[];
