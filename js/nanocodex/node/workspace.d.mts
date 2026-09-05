import type { Workspace, WorkspaceBackend, WorkspaceEntry } from "../runtime/workspace.mjs";

export { tools } from "../runtime/workspace.mjs";

export function open(options: {
  path: string;
  root?: string;
}): Promise<Workspace>;

export type { Workspace, WorkspaceBackend, WorkspaceEntry };
