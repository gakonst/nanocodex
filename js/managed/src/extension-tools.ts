import { extensionTools, fileMemoriesBackend, skillsBackend, type ExtensionProvider } from 'nanocodex-tools/extensions';
import type { NamedTool, ToolContext } from 'nanocodex';
import { memoryTarget } from './memory-target';

export type ManagedExtensionOptions = {
  organizationId: string; teamId: string; ownerId: string; sessionId: string;
  memories: DurableObjectNamespace<import("./memory-scope").MemoryScope>;
  personal(context: ToolContext): boolean;
  skills?: readonly { name: string; instructions: string }[];
  /** The host must resolve live authority, including subagent scope, on each call. */
  authorize(name: string, context: ToolContext): void;
};
/** Actual managed providers: configured skill packages and the user's private MemoryScope. */
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
          throw new Error(error?.message ?? `memory operation failed with HTTP ${response.status}`);
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
  const packages = (options.skills ?? []).map(skill => ({
    authority: { kind: 'orchestrator' as const }, package: `configured:${skill.name}`,
    name: skill.name, description: skill.instructions.split('\n').find(line => line.trim())?.slice(0, 1024) ?? skill.name,
    main_resource: `skill://configured/${skill.name}/SKILL.md`,
  }));
  return extensionTools({ memories, authorize: options.authorize,
    ...(packages.length ? { skills: skillsBackend({
      list: async authority => ({ skills: packages.filter(p => p.authority.kind === authority.kind), warnings: [] }),
      read: async input => {
        const index = packages.findIndex(p => p.package === input.package);
        const entry = packages[index];
        if (!entry || (input.resource != null && input.resource !== entry.main_resource)) throw new Error('skill resource is not available');
        return { resource: entry.main_resource, contents: options.skills![index]!.instructions };
      },
    }) } : {}),
  });
}
