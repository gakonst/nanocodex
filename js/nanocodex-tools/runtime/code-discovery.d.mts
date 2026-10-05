/** A definition from the immutable callable catalog admitted for one cell. */
export type CodeToolDescription = Readonly<{
  name: string;
  callableName: string;
  description: string;
  inputSchema: unknown;
  outputSchema: unknown;
  kind: string;
}>;
export type CodeToolSummary = Pick<CodeToolDescription, "name" | "callableName" | "description">;
export type CodeDiscovery = Readonly<{
  /** Case-insensitive matching of every whitespace-separated term. Default limit 10; range 1–100. */
  searchTools(query: string, options?: { limit?: number }): readonly CodeToolSummary[];
  /** Accepts the public name or normalized tools property; unknown names return undefined. */
  describeTool(name: string): CodeToolDescription | undefined;
  /** Exact prefix before the first __ or dot; unknown namespaces return an empty array. */
  describeNamespace(namespace: string): readonly CodeToolDescription[];
}>;
export function createCodeDiscovery(definitions?: readonly Record<string, unknown>[]): CodeDiscovery;
