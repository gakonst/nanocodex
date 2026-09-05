import type { Workspace, WorkspaceEntry } from "nanocodex/browser/workspace";

const MAX_VISIBLE_WORKSPACE_ENTRIES = 2_000;

export async function listVisibleWorkspaceEntries(
  workspace: Workspace,
): Promise<readonly WorkspaceEntry[]> {
  const hidden = new Set([
    `${workspace.root}/.git`,
    `${workspace.root}/.nanocodex`,
  ]);
  const rootEntries = (await workspace.list(".", {
    maxEntries: MAX_VISIBLE_WORKSPACE_ENTRIES,
  })).filter(({ path }) => !hidden.has(path));
  const entries = [...rootEntries];
  for (const entry of rootEntries) {
    if (entry.kind !== "directory") continue;
    const remaining = MAX_VISIBLE_WORKSPACE_ENTRIES - entries.length;
    if (remaining <= 0) {
      throw new RangeError(
        `visible workspace listing exceeds ${MAX_VISIBLE_WORKSPACE_ENTRIES} entries`,
      );
    }
    entries.push(...await workspace.list(entry.path, {
      recursive: true,
      maxEntries: remaining,
    }));
  }
  return entries.sort((left, right) => left.path.localeCompare(right.path));
}
