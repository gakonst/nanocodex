import type { NamedTool } from "../types.mjs";
import type { Workspace } from "../runtime/workspace.mjs";
export * from "./artifact.mjs";
export type JsonToolOptions = Readonly<{
  /** Defaults to the same-origin `/api/tools/web-search` or image route. */
  url?: string | URL | undefined;
  fetch?: typeof globalThis.fetch | undefined;
  headers?: Readonly<Record<string, string>> | undefined;
}>;

export type ImageGenerationOptions = JsonToolOptions & Readonly<{
  recentImages?(sessionId: string, count: number): string[];
  rememberImage?(sessionId: string, imageUrl: string): void;
  workspace?: Readonly<{ readFile(path: string): Promise<Uint8Array> }> | undefined;
}>;

/** The standard web search tool with its complete runtime JSON Schema. */
export function web(options?: JsonToolOptions): NamedTool;

/** The standard image generation/editing tool. */
export function imageGeneration(options?: ImageGenerationOptions): NamedTool;

/** Reads supported image formats from a caller-owned workspace. */
export function viewImage(options: {
  workspace: Pick<Workspace, "readFile">;
}): NamedTool;

/** A session-scoped planning tool. */
export function updatePlan(): NamedTool;

export { dataset } from "./dataset.mjs";
export type { DatasetOptions } from "./dataset.mjs";
export { justBash } from "./bash.mjs";
export type { JustBashNetworkOptions, JustBashRuntime } from "./bash.mjs";
export { materializeRepositoryWorkspace } from "./repository-workspace.mjs";
export type {
  RepositoryWorkspaceDescriptor,
  RepositoryWorkspaceFetch,
  RepositoryWorkspaceRemote,
} from "./repository-workspace.mjs";
export { createTools } from "./Tools.mjs";
export type {
  AttachmentClient,
  AttachmentSocket,
  AttachmentTarget,
  AttachmentTransport,
  ToolProvider,
  Tools,
} from "./Tools.mjs";
