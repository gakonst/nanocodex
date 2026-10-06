import type { McpServers, ToolContext } from '../types.mjs';
export function createMcpRuntime(configuration: McpServers, options?: Record<string, unknown> & { loadServers?: () => Promise<McpServers>; inventoryRefreshMs?: number; catalogProvider?: (serverName: string) => string | undefined }): Promise<{
  search(input: Record<string, unknown>): unknown;
  resolve(name: string): { handler(input: unknown, context: ToolContext): Promise<unknown> } | undefined;
  /** Mark inventory stale; the next definitions/search/settled call refreshes in background. */
  invalidateInventory(): void;
  definitions(): unknown[];
  settled(): Promise<void>;
  close(): Promise<void>;
}>;
