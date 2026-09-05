import type { WorkspaceEntry } from "nanocodex/browser/workspace";

export type WorkspaceTreeNode = WorkspaceEntry & {
  name: string;
  children: WorkspaceTreeNode[];
};

export function buildWorkspaceTree(
  root: string,
  entries: readonly WorkspaceEntry[],
): WorkspaceTreeNode[] {
  const nodes = new Map<string, WorkspaceTreeNode>();
  const roots: WorkspaceTreeNode[] = [];

  for (const entry of [...entries].sort(compareEntries)) {
    const relative = relativeWorkspacePath(root, entry.path);
    if (!relative) continue;
    const node: WorkspaceTreeNode = {
      ...entry,
      name: relative.split("/").at(-1) ?? relative,
      children: [],
    };
    nodes.set(relative, node);
    const parent = parentPath(relative);
    const parentNode = parent ? nodes.get(parent) : undefined;
    if (parentNode) parentNode.children.push(node);
    else roots.push(node);
  }
  return roots;
}

export function relativeWorkspacePath(root: string, path: string): string {
  if (path === root) return "";
  if (!path.startsWith(`${root}/`)) {
    throw new Error(`workspace entry must stay within ${root}`);
  }
  return path.slice(root.length + 1);
}

export function parentWorkspaceDirectory(
  root: string,
  path: string | undefined,
  kind: WorkspaceEntry["kind"] | undefined,
): string {
  if (!path || path === root) return root;
  if (kind === "directory") return path;
  const relative = relativeWorkspacePath(root, path);
  const parent = parentPath(relative);
  return parent ? `${root}/${parent}` : root;
}

function compareEntries(left: WorkspaceEntry, right: WorkspaceEntry): number {
  const leftRelative = left.path.split("/").length;
  const rightRelative = right.path.split("/").length;
  return leftRelative - rightRelative
    || Number(right.kind === "directory") - Number(left.kind === "directory")
    || left.path.localeCompare(right.path);
}

function parentPath(path: string): string {
  return path.split("/").slice(0, -1).join("/");
}
