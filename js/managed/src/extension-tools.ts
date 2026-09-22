import type { ToolContext } from 'nanocodex';

export type ManagedExtensionOptions = {
  organizationId: string; teamId: string; ownerId: string; sessionId: string;
  memories: DurableObjectNamespace<import("./memory-scope").MemoryScope>;
  personal(context: ToolContext): boolean;
  /** The host must resolve live authority, including subagent scope, on each call. */
  authorize(name: string, context: ToolContext): void;
};
