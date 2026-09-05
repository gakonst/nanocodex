const OID_PATTERN = /^[a-f0-9]{40}$/;
const SHARD_KEY_PATTERN = /^generations\/[a-f0-9]{40}\/objects\/\d{4}\.pack$/;

export const gitObjectType = {
  commit: 1,
  tree: 2,
  blob: 3,
  tag: 4,
} as const;

export type GitObjectType = typeof gitObjectType[keyof typeof gitObjectType];

export type GitObjectRecord = [
  type: GitObjectType,
  shard: number,
  offset: number,
  length: number,
  dependencies: string[],
];

export type GitObjectManifest = {
  version: 1;
  head: string;
  shards: Array<{ key: string; size: number }>;
  objects: Record<string, GitObjectRecord>;
};

export type GitFetchSelection = {
  objectIds: string[];
  shallow: string[];
  unshallow: string[];
};

export function isGitObjectManifest(value: unknown): value is GitObjectManifest {
  if (value == null || typeof value !== "object") return false;
  const manifest = value as Partial<GitObjectManifest>;
  if (
    manifest.version !== 1 ||
    typeof manifest.head !== "string" ||
    !OID_PATTERN.test(manifest.head) ||
    !Array.isArray(manifest.shards) ||
    manifest.shards.length === 0 ||
    manifest.shards.length > 256 ||
    manifest.objects == null ||
    typeof manifest.objects !== "object" ||
    Array.isArray(manifest.objects)
  ) {
    return false;
  }
  if (!manifest.shards.every(
    (shard) =>
      shard != null &&
      typeof shard === "object" &&
      typeof shard.key === "string" &&
      SHARD_KEY_PATTERN.test(shard.key) &&
      Number.isSafeInteger(shard.size) &&
      shard.size > 0,
  )) {
    return false;
  }

  for (const [oid, record] of Object.entries(manifest.objects)) {
    if (!OID_PATTERN.test(oid) || !isGitObjectRecord(record, manifest.shards)) return false;
  }
  return manifest.objects[manifest.head] != null;
}

function isGitObjectRecord(
  value: unknown,
  shards: GitObjectManifest["shards"],
): value is GitObjectRecord {
  if (!Array.isArray(value) || value.length !== 5) return false;
  const [type, shardIndex, offset, length, dependencies] = value;
  if (
    ![gitObjectType.commit, gitObjectType.tree, gitObjectType.blob, gitObjectType.tag]
      .includes(type as GitObjectType) ||
    !Number.isSafeInteger(shardIndex) ||
    shardIndex < 0 ||
    shardIndex >= shards.length ||
    !Number.isSafeInteger(offset) ||
    offset < 0 ||
    !Number.isSafeInteger(length) ||
    length <= 0 ||
    !Array.isArray(dependencies) ||
    !dependencies.every((dependency) => typeof dependency === "string" && OID_PATTERN.test(dependency))
  ) {
    return false;
  }
  return offset + length <= shards[shardIndex]!.size;
}

export function selectGitObjects(
  manifest: GitObjectManifest,
  wants: readonly string[],
  haves: readonly string[],
  clientShallows: readonly string[],
  deepen: number,
  deepenRelative = false,
): GitFetchSelection {
  const shallowStops = new Set(clientShallows.filter((oid) => manifest.objects[oid] != null));
  const excluded = collectExcludedObjects(manifest, haves, shallowStops);
  const depth = deepen > 0 && deepen < 0x7fffffff
    ? deepenRelative && shallowStops.size > 0
      ? computeRelativeDepthSet(manifest, wants, shallowStops, deepen)
      : computeDepthSet(manifest, wants, deepen)
    : null;
  const selected = new Set<string>();
  const visited = new Set<string>();
  const stack = [...wants];

  while (stack.length > 0) {
    const oid = stack.pop()!;
    if (visited.has(oid)) continue;
    visited.add(oid);
    const record = manifest.objects[oid];
    if (record == null) continue;
    if (record[0] === gitObjectType.commit && depth != null && !depth.commits.has(oid)) {
      continue;
    }
    if (excluded.has(oid)) {
      if (record[0] === gitObjectType.commit && (!shallowStops.has(oid) || deepen > 0)) {
        stack.push(...commitParents(record));
      }
      continue;
    }
    selected.add(oid);
    stack.push(...record[4]);
  }

  if (deepen >= 0x7fffffff) {
    return {
      objectIds: [...selected],
      shallow: [],
      unshallow: [...shallowStops],
    };
  }
  if (depth == null) {
    return { objectIds: [...selected], shallow: [], unshallow: [] };
  }
  return {
    objectIds: [...selected],
    shallow: [...depth.boundary].filter((oid) => !shallowStops.has(oid)),
    unshallow: [...shallowStops].filter(
      (oid) => depth.commits.has(oid) && !depth.boundary.has(oid),
    ),
  };
}

function computeRelativeDepthSet(
  manifest: GitObjectManifest,
  wants: readonly string[],
  shallowStops: ReadonlySet<string>,
  additionalDepth: number,
): { commits: Set<string>; boundary: Set<string> } {
  const commits = new Set<string>();
  const existing = wants.map((want) => peelToCommit(manifest, want)).filter((oid) => oid != null);
  while (existing.length > 0) {
    const oid = existing.pop()!;
    if (commits.has(oid)) continue;
    const record = manifest.objects[oid];
    if (record == null || record[0] !== gitObjectType.commit) continue;
    commits.add(oid);
    if (!shallowStops.has(oid)) existing.push(...commitParents(record));
  }

  const boundary = new Set<string>();
  const queue: Array<{ oid: string; depth: number }> = [];
  for (const shallow of shallowStops) {
    const record = manifest.objects[shallow];
    if (record?.[0] !== gitObjectType.commit) continue;
    for (const parent of commitParents(record)) queue.push({ oid: parent, depth: 1 });
  }
  for (let index = 0; index < queue.length; index++) {
    const current = queue[index]!;
    const record = manifest.objects[current.oid];
    if (record == null || record[0] !== gitObjectType.commit) continue;
    commits.add(current.oid);
    const parents = commitParents(record);
    if (current.depth >= additionalDepth) {
      if (parents.length > 0) boundary.add(current.oid);
      continue;
    }
    boundary.delete(current.oid);
    for (const parent of parents) queue.push({ oid: parent, depth: current.depth + 1 });
  }
  return { commits, boundary };
}

function collectExcludedObjects(
  manifest: GitObjectManifest,
  haves: readonly string[],
  shallowStops: ReadonlySet<string>,
): Set<string> {
  const excluded = new Set<string>();
  const stack = haves.filter((oid) => manifest.objects[oid] != null);
  while (stack.length > 0) {
    const oid = stack.pop()!;
    if (excluded.has(oid)) continue;
    const record = manifest.objects[oid];
    if (record == null) continue;
    excluded.add(oid);
    if (record[0] === gitObjectType.commit && shallowStops.has(oid)) {
      const tree = record[4][0];
      if (tree != null) stack.push(tree);
    } else {
      stack.push(...record[4]);
    }
  }
  return excluded;
}

function computeDepthSet(
  manifest: GitObjectManifest,
  wants: readonly string[],
  maximumDepth: number,
): { commits: Set<string>; boundary: Set<string> } {
  const commits = new Set<string>();
  const boundary = new Set<string>();
  const queue: Array<{ oid: string; depth: number }> = [];
  for (const want of wants) {
    const commit = peelToCommit(manifest, want);
    if (commit != null && !commits.has(commit)) {
      commits.add(commit);
      queue.push({ oid: commit, depth: 1 });
    }
  }
  for (let index = 0; index < queue.length; index++) {
    const current = queue[index]!;
    const record = manifest.objects[current.oid];
    if (record == null || record[0] !== gitObjectType.commit) continue;
    const parents = commitParents(record);
    if (current.depth >= maximumDepth) {
      if (parents.length > 0) boundary.add(current.oid);
      continue;
    }
    boundary.delete(current.oid);
    for (const parent of parents) {
      if (manifest.objects[parent] != null && !commits.has(parent)) {
        commits.add(parent);
        queue.push({ oid: parent, depth: current.depth + 1 });
      }
    }
  }
  return { commits, boundary };
}

function peelToCommit(manifest: GitObjectManifest, initialOid: string): string | null {
  let oid = initialOid;
  for (let depth = 0; depth < 10; depth++) {
    const record = manifest.objects[oid];
    if (record == null) return null;
    if (record[0] === gitObjectType.commit) return oid;
    if (record[0] !== gitObjectType.tag || record[4][0] == null) return null;
    oid = record[4][0];
  }
  return null;
}

function commitParents(record: GitObjectRecord): string[] {
  return record[4].slice(1);
}
