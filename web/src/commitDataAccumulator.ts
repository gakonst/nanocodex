import type {
  ChangeTypes,
  CodeViewItem,
  FileDiffMetadata,
} from "@pierre/diffs";

export type CommitStreamItem = CodeViewItem<undefined>;

export interface CommitDataAccumulator {
  fileIndex: number;
  itemIds: Set<string>;
  items: CommitStreamItem[];
  nextCollisionSuffixByBase: Map<string, number>;
  pendingItemById: Map<string, CommitStreamItem>;
  pendingItems: CommitStreamItem[];
  pathStateByTreePath: Map<string, CodeViewPathState>;
}

export interface CommitItemIdRename {
  oldId: string;
  newId: string;
}

interface CodeViewPathState {
  currentItem: CommitStreamItem;
  currentItemId: string;
  currentType: ChangeTypes;
}

export function createCommitDataAccumulator(): CommitDataAccumulator {
  return {
    fileIndex: 0,
    itemIds: new Set(),
    items: [],
    nextCollisionSuffixByBase: new Map(),
    pendingItemById: new Map(),
    pendingItems: [],
    pathStateByTreePath: new Map(),
  };
}

export function appendCommitItemToCommitData(
  accumulator: CommitDataAccumulator,
  item: CommitStreamItem,
): void {
  accumulator.itemIds.add(item.id);
  accumulator.items.push(item);
  accumulator.pendingItems.push(item);
  accumulator.pendingItemById.set(item.id, item);
}

// This is the DiffsHub accumulator path. Keeping the same path-based ids and
// rename semantics matters when a streamed format-patch contains repeated
// entries for the same file.
export function appendFileDiffToCommitData(
  accumulator: CommitDataAccumulator,
  fileDiff: FileDiffMetadata,
  treePathPrefix: string | undefined,
): CommitItemIdRename | undefined {
  const path = fileDiff.name;
  const treePath = treePathPrefix == null ? path : `${treePathPrefix}/${path}`;
  const previousPathState =
    path.length === 0
      ? undefined
      : accumulator.pathStateByTreePath.get(treePath);
  const itemIdRename =
    previousPathState == null
      ? undefined
      : renameCurrentPathItem(accumulator, treePath, previousPathState);
  const id = accumulator.itemIds.has(treePath)
    ? createFallbackItemId(accumulator, treePath)
    : treePath;

  accumulator.fileIndex++;
  const item: CommitStreamItem = {
    id,
    type: "diff",
    fileDiff,
    version: 0,
  };
  accumulator.itemIds.add(id);
  accumulator.items.push(item);
  accumulator.pendingItems.push(item);
  accumulator.pendingItemById.set(id, item);

  if (path.length > 0) {
    accumulator.pathStateByTreePath.set(treePath, {
      currentItem: item,
      currentItemId: id,
      currentType: fileDiff.type,
    });
  }

  return itemIdRename;
}

export function takePendingCommitItems(
  accumulator: CommitDataAccumulator,
): CommitStreamItem[] {
  const { pendingItems } = accumulator;
  accumulator.pendingItems = [];
  accumulator.pendingItemById.clear();
  return pendingItems;
}

function renameCurrentPathItem(
  accumulator: CommitDataAccumulator,
  treePath: string,
  pathState: CodeViewPathState,
): CommitItemIdRename | undefined {
  const oldId = pathState.currentItemId;
  const newId = createSupersededItemId(
    accumulator,
    treePath,
    pathState.currentType,
  );
  pathState.currentItem.id = newId;
  pathState.currentItemId = newId;
  accumulator.itemIds.delete(oldId);
  accumulator.itemIds.add(newId);

  const pendingItem = accumulator.pendingItemById.get(oldId);
  if (pendingItem != null) {
    accumulator.pendingItemById.delete(oldId);
    accumulator.pendingItemById.set(newId, pendingItem);
    return undefined;
  }

  return { oldId, newId };
}

function createSupersededItemId(
  accumulator: CommitDataAccumulator,
  treePath: string,
  changeType: ChangeTypes,
): string {
  const semanticSuffix = changeType === "deleted" ? "?deleted" : "?previous";
  return createUniqueItemId(accumulator, `${treePath}${semanticSuffix}`);
}

function createFallbackItemId(
  accumulator: CommitDataAccumulator,
  treePath: string,
): string {
  return createUniqueItemId(accumulator, `${treePath}?2`);
}

function createUniqueItemId(
  accumulator: CommitDataAccumulator,
  baseId: string,
): string {
  if (!accumulator.itemIds.has(baseId)) return baseId;

  let suffix = accumulator.nextCollisionSuffixByBase.get(baseId) ?? 2;
  let itemId = `${baseId}-${suffix}`;
  while (accumulator.itemIds.has(itemId)) {
    suffix++;
    itemId = `${baseId}-${suffix}`;
  }
  accumulator.nextCollisionSuffixByBase.set(baseId, suffix + 1);
  return itemId;
}
