import { extensionTools, fileMemoriesBackend, type ExtensionProvider } from 'nanocodex-tools/extensions';
import type { NamedTool, ToolContext } from 'nanocodex';
import { memoryTarget } from './memory-target';
import { HistorySearchError } from './history-search';

export type ManagedExtensionOptions = {
  organizationId: string; teamId: string; ownerId: string; sessionId: string;
  memories: DurableObjectNamespace<import("./memory-scope").MemoryScope>;
  personal(context: ToolContext): boolean;
  /** The host must resolve live authority, including subagent scope, on each call. */
  authorize(name: string, context: ToolContext): void;
};
/** Bind the memory tools to the authenticated user's permitted partitions. */
export function managedExtensionTools(options: ManagedExtensionOptions): NamedTool[] {
  const memories = Object.fromEntries(['list', 'read', 'search', 'add_ad_hoc_note'].map(method => [method,
    async (input: unknown, context: ToolContext) => {
      const personal = options.personal(context);
      const root = personal ? 'personal' : 'team';
      const call = async (scope: 'personal' | 'team', operation: string, value: unknown) => {
        const target = memoryTarget(options.organizationId, options.teamId, options.ownerId, scope);
        const response = await options.memories.getByName(target.name).fetch('https://memory.internal/extension-memories/' + operation, {
          method: 'POST', signal: context.signal,
          headers: {
            'content-type': 'application/json',
            'x-nanocodex-organization-id': options.organizationId,
            'x-nanocodex-team-id': target.team,
            'x-nanocodex-memory-initialize': '1',
            'x-nanocodex-subject-id': `agent:${options.sessionId}`,
            ...(scope === 'personal' ? { 'x-nanocodex-private-memory-owner': options.ownerId } : {}),
            ...(operation === 'add_ad_hoc_note' ? { 'x-nanocodex-memory-mutation': '1' } : {}),
          }, body: JSON.stringify(value),
        });
        if (!response.ok) {
          const error = await response.json<{ message?: string }>().catch(() => undefined);
          throw new HistorySearchError(response.status, "memory_request_failed", error?.message ?? `memory operation failed with HTTP ${response.status}`);
        }
        return response.json();
      };
      const backend = fileMemoriesBackend({
        listFiles: async () => {
          const own = await call(root, 'files', {}) as string[];
          return personal ? [...own, ...(await call('team', 'files', {}) as string[]).map(path => `team/${path}`)] : own;
        },
        readFile: async path => personal && path.startsWith('team/')
          ? await call('team', 'file', { path: path.slice(5) }) as string
          : await call(root, 'file', { path }) as string,
        createFile: async (path, note) => {
          const prefix = 'extensions/ad_hoc/notes/';
          if (!path.startsWith(prefix)) throw new Error('only ad-hoc notes may be created');
          await call(root, 'add_ad_hoc_note', { filename: path.slice(prefix.length), note });
        },
      });
      return backend[method]!(input, context);
    },
  ])) as ExtensionProvider;
  return extensionTools({ memories, authorize: options.authorize });
}
