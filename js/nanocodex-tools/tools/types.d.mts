export type SubagentToolContext = Readonly<{
  agentId: string;
  parentAgentId: string | null;
  sessionId: string;
  role: string;
  task: string;
}>;

export type ToolContext = Readonly<{
  callId: string;
  parentCallId: string;
  sessionId: string;
  model: string;
  signal: AbortSignal;
  subagent?: SubagentToolContext | undefined;
}>;

export type Tool = Readonly<{
  description: string;
  supportsParallelToolCalls?: boolean | undefined;
  parameters?: Record<string, unknown> | undefined;
  outputSchema?: Record<string, unknown> | undefined;
  handler(input: unknown, context: ToolContext): unknown | Promise<unknown>;
  releaseSession?(sessionId: string): void;
  dispose?(): void | Promise<void>;
}>;

export type NamedTool = Tool & Readonly<{ name: string }>;
export type ToolMap = Record<string, Tool>;

export type WorkspaceEntry = Readonly<{
  kind: "directory" | "file";
  modifiedAt?: number | undefined;
  path: string;
  size?: number | undefined;
}>;

export type Workspace = Readonly<{
  root: string;
  list(path?: string, options?: {
    recursive?: boolean | undefined;
    maxEntries?: number | undefined;
  }): Promise<readonly WorkspaceEntry[]>;
  readFile(path: string): Promise<Uint8Array>;
  writeFile(path: string, contents: string | ArrayBuffer | ArrayBufferView): Promise<void>;
  remove(path: string, options?: { recursive?: boolean | undefined }): Promise<void>;
  mkdir(path: string): Promise<void>;
}>;
